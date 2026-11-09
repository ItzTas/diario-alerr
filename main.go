package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"syscall"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	"golang.org/x/sync/errgroup"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/gazette"
	"github.com/ItzTas/diario-alerr/internal/modules/feed"
	"github.com/ItzTas/diario-alerr/internal/modules/health"
	"github.com/ItzTas/diario-alerr/internal/notify"
	"github.com/ItzTas/diario-alerr/internal/watch"
)

const (
	httpTimeout     = 60 * time.Second
	shutdownTimeout = 5 * time.Second
)

type application struct {
	cfg     core.Config
	log     *slog.Logger
	pool    *pgxpool.Pool
	watcher *watch.Service
	cycle   *watch.Cycle
	handler http.Handler
}

func main() {
	logger := slog.New(slog.NewTextHandler(os.Stdout, nil))
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	err := run(ctx, logger)
	stop()
	if err != nil {
		logger.Error("fatal", "err", err)
		os.Exit(1)
	}
}

func run(ctx context.Context, logger *slog.Logger) error {
	cfg, err := core.Load()
	if err != nil {
		return err
	}
	app, err := assemble(ctx, cfg, logger, time.Now)
	if err != nil {
		return err
	}
	defer app.Close()
	return app.Run(ctx)
}

func assemble(ctx context.Context, cfg core.Config, logger *slog.Logger, now func() time.Time) (*application, error) {
	pool, err := core.OpenDatabase(ctx, cfg.DatabaseURL)
	if err != nil {
		return nil, err
	}
	client := &http.Client{Timeout: httpTimeout}
	watcher := watch.NewService(watch.NewPostgres(pool), &gazette.Client{HTTP: client, APIURL: cfg.GazetteAPIURL}, watch.Options{
		Terms:      cfg.Terms,
		Interval:   cfg.Interval,
		StartAfter: cfg.StartAfter,
		StateFile:  cfg.StateFile,
		Channels:   notify.Channels(cfg, client),
		Now:        now,
		Log:        logger,
	})
	if err := watcher.Prepare(ctx); err != nil {
		pool.Close()
		return nil, err
	}
	mux := http.NewServeMux()
	feed.Configure(mux, pool, feed.Options{Terms: cfg.Terms, PublicURL: cfg.PublicURL}, logger)
	health.Configure(mux, pool, health.Options{Started: now(), Interval: cfg.Interval, Now: now}, logger)
	return &application{
		cfg:     cfg,
		log:     logger,
		pool:    pool,
		watcher: watcher,
		cycle:   watch.NewCycle(watcher, cfg.Interval),
		handler: mux,
	}, nil
}

func (a *application) Run(ctx context.Context) error {
	channels := strings.Join(a.watcher.ChannelNames(), ", ")
	if channels == "" {
		channels = "RSS only"
	}
	a.log.Info("watching", "terms", strings.Join(a.cfg.Terms, ", "), "every", a.cfg.Interval, "notifying", channels)
	server := &http.Server{Addr: ":" + a.cfg.HTTPPort, Handler: a.handler, ReadHeaderTimeout: 10 * time.Second}
	g, gctx := errgroup.WithContext(ctx)
	g.Go(func() error { return a.cycle.Run(gctx) })
	g.Go(func() error { return serve(gctx, server) })
	return g.Wait()
}

func serve(ctx context.Context, server *http.Server) error {
	stopped := make(chan error, 1)
	go func() {
		<-ctx.Done()
		shutdownCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), shutdownTimeout)
		defer cancel()
		stopped <- server.Shutdown(shutdownCtx)
	}()
	if err := server.ListenAndServe(); !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return <-stopped
}

func (a *application) Close() {
	a.pool.Close()
}
