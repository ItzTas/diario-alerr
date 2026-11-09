package main

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"golang.org/x/sync/errgroup"
)

const (
	cursorOverlap = 24 * time.Hour
	scanWorkers   = 2
	httpTimeout   = 60 * time.Second
	welcomeID     = "welcome"
)

type monitor struct {
	cfg         config
	client      *http.Client
	apiURL      string
	telegramAPI string
	statePath   string
	started     time.Time

	mu sync.RWMutex
	st *state
}

func main() {
	slog.SetDefault(slog.New(slog.NewTextHandler(os.Stdout, nil)))
	if err := run(); err != nil {
		slog.Error("fatal", "err", err)
		os.Exit(1)
	}
}

func run() error {
	cfg, err := loadConfig()
	if err != nil {
		return err
	}
	m, err := newMonitor(cfg)
	if err != nil {
		return err
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	server := &http.Server{Addr: ":" + cfg.httpPort, Handler: m.routes(), ReadHeaderTimeout: 10 * time.Second}
	go func() {
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			slog.Error("http server stopped", "err", err)
			stop()
		}
	}()

	channels := strings.Join(mapKeys(m.channels()), ", ")
	if channels == "" {
		channels = "RSS only"
	}
	slog.Info("watching", "terms", strings.Join(cfg.terms, ", "), "every", cfg.interval, "notifying", channels)
	m.loop(ctx)

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	return server.Shutdown(shutdownCtx)
}

func newMonitor(cfg config) (*monitor, error) {
	statePath := filepath.Join(cfg.dataDir, "state.json")
	st, firstRun, err := loadState(statePath, cfg.startAfter)
	if err != nil {
		return nil, err
	}
	m := &monitor{
		cfg:         cfg,
		client:      &http.Client{Timeout: httpTimeout},
		apiURL:      defaultAPIURL,
		telegramAPI: "https://api.telegram.org",
		statePath:   statePath,
		started:     time.Now().UTC(),
		st:          st,
	}
	if !firstRun {
		return m, nil
	}
	m.st.Alerts = append(m.st.Alerts, welcomeAlert(cfg))
	return m, m.save()
}

func welcomeAlert(cfg config) *alert {
	return &alert{
		ID:        welcomeID,
		Title:     "Monitor do Diário da ALE/RR ligado",
		Link:      siteURL,
		Body:      fmt.Sprintf("Vigiando: %s\nVerificação a cada %.0f min.", strings.Join(cfg.terms, ", "), cfg.interval.Minutes()),
		Published: time.Now().UTC().Format(time.RFC3339),
		Delivered: []string{},
	}
}

func (m *monitor) loop(ctx context.Context) {
	for {
		m.runCycle(ctx)
		select {
		case <-ctx.Done():
			return
		case <-time.After(m.cfg.interval):
		}
	}
}

func (m *monitor) runCycle(ctx context.Context) {
	if m.scanNewEditions(ctx) {
		now := time.Now().UTC()
		m.mu.Lock()
		m.st.LastSuccess = &now
		m.mu.Unlock()
	}
	m.deliverPending(ctx)
	if err := m.save(); err != nil {
		slog.Error("could not save state", "err", err)
	}
}

func (m *monitor) scanNewEditions(ctx context.Context) bool {
	editions, err := listEditions(ctx, m.client, m.apiURL, m.st.Cursor.Add(-cursorOverlap))
	if err != nil {
		slog.Warn("could not list editions", "err", err)
		return false
	}
	fresh := unseen(editions, m.st.Seen)
	results := make([]*hits, len(fresh))
	errs := make([]error, len(fresh))
	var g errgroup.Group
	g.SetLimit(scanWorkers)
	for i, e := range fresh {
		g.Go(func() error {
			results[i], errs[i] = scanEdition(ctx, m.client, e, m.cfg.terms)
			return nil
		})
	}
	_ = g.Wait()
	for i, e := range fresh {
		if errs[i] != nil {
			slog.Warn("could not read edition, retrying next cycle", "edition", e.Title, "err", errs[i])
			return false
		}
		m.record(e, results[i])
	}
	return true
}

func (m *monitor) record(e edition, h *hits) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.st.Seen = append(m.st.Seen, e.ID)
	if e.Published.After(m.st.Cursor) {
		m.st.Cursor = e.Published
	}
	if h == nil {
		slog.Info("no match", "edition", e.Title)
		return
	}
	slog.Info("MATCH", "edition", e.Title, "pages", h.Pages)
	m.st.Alerts = append(m.st.Alerts, newAlert(e, h))
}

func (m *monitor) save() error {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.st.trim()
	return m.st.save(m.statePath)
}

func mapKeys[V any](values map[string]V) []string {
	keys := make([]string, 0, len(values))
	for key := range values {
		keys = append(keys, key)
	}
	return keys
}
