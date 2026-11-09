package watch

import (
	"context"
	"log/slog"
	"maps"
	"slices"
	"time"

	"golang.org/x/sync/errgroup"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/gazette"
	"github.com/ItzTas/diario-alerr/internal/notify"
)

const (
	cursorOverlap = 24 * time.Hour
	scanWorkers   = 2
	maxSeen       = 1000
	maxAlerts     = 100
)

type Options struct {
	Terms      []string
	Interval   time.Duration
	StartAfter time.Time
	StateFile  string
	Channels   map[string]notify.Sender
	Now        func() time.Time
	Log        *slog.Logger
}

type Service struct {
	repo    Repository
	gazette Gazette
	opts    Options
}

func NewService(repo Repository, gz Gazette, opts Options) *Service {
	return &Service{repo: repo, gazette: gz, opts: opts}
}

func (s *Service) ChannelNames() []string {
	return slices.Sorted(maps.Keys(s.opts.Channels))
}

func (s *Service) Prepare(ctx context.Context) error {
	snapshot, imported, err := s.initialSnapshot()
	if err != nil {
		return err
	}
	created, err := s.repo.Initialize(ctx, snapshot)
	if err != nil || !created {
		return err
	}
	s.opts.Log.Info("monitor state created", "imported_from", s.opts.StateFile, "imported", imported, "cursor", snapshot.Cursor)
	return nil
}

func (s *Service) initialSnapshot() (Snapshot, bool, error) {
	if s.opts.StateFile != "" {
		snapshot, found, err := ReadStateFile(s.opts.StateFile)
		if err != nil || found {
			return snapshot, found, err
		}
	}
	welcome := WelcomeAlert(s.opts.Terms, s.opts.Interval, s.opts.Now())
	return Snapshot{Cursor: s.opts.StartAfter, Alerts: []StoredAlert{{Alert: welcome}}}, false, nil
}

func (s *Service) RunCycle(ctx context.Context) {
	if s.scanNewEditions(ctx) {
		if err := s.repo.MarkSuccess(ctx, s.opts.Now().UTC()); err != nil {
			s.opts.Log.Error("could not record the successful check", "err", err)
		}
	}
	s.deliverPending(ctx)
	if err := s.repo.Prune(ctx, maxSeen, maxAlerts); err != nil {
		s.opts.Log.Error("could not prune old editions and alerts", "err", err)
	}
}

func (s *Service) scanNewEditions(ctx context.Context) bool {
	fresh, err := s.freshEditions(ctx)
	if err != nil {
		s.opts.Log.Warn("could not list editions", "err", err)
		return false
	}
	results := make([]*gazette.Hits, len(fresh))
	errs := make([]error, len(fresh))
	var g errgroup.Group
	g.SetLimit(scanWorkers)
	for i, e := range fresh {
		g.Go(func() error {
			results[i], errs[i] = s.gazette.Scan(ctx, e, s.opts.Terms)
			return nil
		})
	}
	_ = g.Wait()
	for i, e := range fresh {
		if errs[i] != nil {
			s.opts.Log.Warn("could not read edition, retrying next cycle", "edition", e.Title, "err", errs[i])
			return false
		}
		if err := s.record(ctx, e, results[i]); err != nil {
			s.opts.Log.Error("could not record edition, retrying next cycle", "edition", e.Title, "err", err)
			return false
		}
	}
	return true
}

func (s *Service) freshEditions(ctx context.Context) ([]gazette.Edition, error) {
	cursor, err := s.repo.Cursor(ctx)
	if err != nil {
		return nil, err
	}
	editions, err := s.gazette.ListEditions(ctx, cursor.Add(-cursorOverlap))
	if err != nil {
		return nil, err
	}
	ids := make([]int64, len(editions))
	for i, e := range editions {
		ids[i] = e.ID
	}
	seen, err := s.repo.SeenEditions(ctx, ids)
	if err != nil {
		return nil, err
	}
	return slices.DeleteFunc(editions, func(e gazette.Edition) bool { return slices.Contains(seen, e.ID) }), nil
}

func (s *Service) record(ctx context.Context, e gazette.Edition, h *gazette.Hits) error {
	record := EditionRecord{ID: e.ID, Published: e.Published}
	if h == nil {
		s.opts.Log.Info("no match", "edition", e.Title)
		return s.repo.RecordEdition(ctx, record)
	}
	s.opts.Log.Info("MATCH", "edition", e.Title, "pages", h.Pages)
	alert := EditionAlert(e, h)
	record.Alert = &alert
	return s.repo.RecordEdition(ctx, record)
}

func (s *Service) deliverPending(ctx context.Context) {
	for _, name := range s.ChannelNames() {
		s.deliverChannel(ctx, name, s.opts.Channels[name])
	}
}

func (s *Service) deliverChannel(ctx context.Context, name string, send notify.Sender) {
	pending, err := s.repo.PendingAlerts(ctx, name)
	if err != nil {
		s.opts.Log.Error("could not read pending alerts", "channel", name, "err", err)
		return
	}
	for _, a := range pending {
		if err := deliver(ctx, s.repo, name, send, a); err != nil {
			s.opts.Log.Warn("delivery failed, retrying next cycle", "channel", name, "err", err)
			return
		}
	}
}

func deliver(ctx context.Context, repo Repository, channel string, send notify.Sender, a core.Alert) error {
	if err := send(ctx, a); err != nil {
		return err
	}
	return repo.MarkDelivered(ctx, a.ID, channel)
}
