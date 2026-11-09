package watch

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"reflect"
	"sync"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/core/coretest"
	"github.com/ItzTas/diario-alerr/internal/gazette"
	"github.com/ItzTas/diario-alerr/internal/notify"
)

var clock = time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)

type fakeGazette struct {
	editions []gazette.Edition
	hits     map[int64]*gazette.Hits
	failScan map[int64]bool
	after    []time.Time
}

func (f *fakeGazette) ListEditions(_ context.Context, after time.Time) ([]gazette.Edition, error) {
	f.after = append(f.after, after)
	return append([]gazette.Edition(nil), f.editions...), nil
}

func (f *fakeGazette) Scan(_ context.Context, e gazette.Edition, _ []string) (*gazette.Hits, error) {
	if f.failScan[e.ID] {
		return nil, errors.New("pdf unavailable")
	}
	return f.hits[e.ID], nil
}

type recorder struct {
	mu   sync.Mutex
	ids  []string
	fail int
}

func (r *recorder) send(_ context.Context, a core.Alert) error {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.fail > 0 {
		r.fail--
		return errors.New("channel down")
	}
	r.ids = append(r.ids, a.ID)
	return nil
}

func newTestService(t *testing.T, gz Gazette, channels map[string]notify.Sender, stateFile string) (*Service, *Postgres) {
	t.Helper()
	repo := NewPostgres(coretest.Pool(t))
	service := NewService(repo, gz, Options{
		Terms:      []string{"TALES SABINI"},
		Interval:   30 * time.Minute,
		StartAfter: time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC),
		StateFile:  stateFile,
		Channels:   channels,
		Now:        func() time.Time { return clock },
		Log:        slog.New(slog.NewTextHandler(io.Discard, nil)),
	})
	if err := service.Prepare(t.Context()); err != nil {
		t.Fatal(err)
	}
	return service, repo
}

func TestRunCycleAlertsOnMatchesAndDeliversEverything(t *testing.T) {
	published := time.Date(2026, 11, 6, 20, 0, 0, 0, time.UTC)
	gz := &fakeGazette{
		editions: []gazette.Edition{{ID: 10, Published: published, URL: "https://x/10.pdf", Title: "ED 10"}, {ID: 11, Published: published.Add(time.Hour), Title: "ED 11"}},
		hits:     map[int64]*gazette.Hits{10: {Pages: map[string][]int{"TALES SABINI": {4}}, Order: []string{"TALES SABINI"}, FirstPage: 4, Snippet: "S"}},
	}
	ntfy := &recorder{}
	service, repo := newTestService(t, gz, map[string]notify.Sender{"ntfy": ntfy.send}, "")

	service.RunCycle(t.Context())

	if !reflect.DeepEqual(ntfy.ids, []string{core.WelcomeAlertID, "edition-10"}) {
		t.Errorf("delivered = %v", ntfy.ids)
	}
	if want := time.Date(2026, 10, 31, 0, 0, 0, 0, time.UTC); !gz.after[0].Equal(want) {
		t.Errorf("listed after %v, want the start minus one day (%v)", gz.after[0], want)
	}
	if cursor, _ := repo.Cursor(t.Context()); !cursor.Equal(published.Add(time.Hour)) {
		t.Errorf("cursor = %v", cursor)
	}
	if last, _ := repo.queries.LastSuccess(t.Context()); last == nil || !last.Equal(clock) {
		t.Errorf("last success = %v, want %v", last, clock)
	}

	service.RunCycle(t.Context())
	if len(ntfy.ids) != 2 {
		t.Errorf("second cycle delivered again: %v", ntfy.ids)
	}
}

func TestRunCycleRetriesOnlyTheChannelThatFailed(t *testing.T) {
	ntfy := &recorder{fail: 1}
	telegram := &recorder{}
	service, _ := newTestService(t, &fakeGazette{}, map[string]notify.Sender{"ntfy": ntfy.send, "telegram": telegram.send}, "")

	service.RunCycle(t.Context())
	if len(ntfy.ids) != 0 || !reflect.DeepEqual(telegram.ids, []string{core.WelcomeAlertID}) {
		t.Fatalf("after first cycle ntfy=%v telegram=%v", ntfy.ids, telegram.ids)
	}
	service.RunCycle(t.Context())
	if !reflect.DeepEqual(ntfy.ids, []string{core.WelcomeAlertID}) || len(telegram.ids) != 1 {
		t.Errorf("after second cycle ntfy=%v telegram=%v", ntfy.ids, telegram.ids)
	}
}

func TestRunCycleKeepsUnreadableEditionsForTheNextCycle(t *testing.T) {
	published := time.Date(2026, 11, 6, 20, 0, 0, 0, time.UTC)
	gz := &fakeGazette{
		editions: []gazette.Edition{{ID: 1, Published: published}, {ID: 2, Published: published.Add(time.Hour)}},
		failScan: map[int64]bool{2: true},
	}
	service, repo := newTestService(t, gz, nil, "")

	service.RunCycle(t.Context())

	if seen, _ := repo.SeenEditions(t.Context(), []int64{1, 2}); !reflect.DeepEqual(seen, []int64{1}) {
		t.Errorf("seen = %v, want only the readable edition", seen)
	}
	if last, _ := repo.queries.LastSuccess(t.Context()); last != nil {
		t.Errorf("last success = %v, want none after a failed scan", last)
	}
}

func TestPrepareImportsTheStateFileInsteadOfTheWelcome(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	written := `{"cursor": "2026-11-06T20:23:18Z", "seen": [12077],
  "alerts": [{"id": "edition-12077", "title": "T", "link": "L", "body": "B", "published": "2026-11-06T20:23:18Z", "delivered": ["ntfy"]}],
  "last_success": null}`
	if err := os.WriteFile(path, []byte(written), 0o600); err != nil {
		t.Fatal(err)
	}
	ntfy, telegram := &recorder{}, &recorder{}
	service, repo := newTestService(t, &fakeGazette{}, map[string]notify.Sender{"ntfy": ntfy.send, "telegram": telegram.send}, path)

	if cursor, _ := repo.Cursor(t.Context()); !cursor.Equal(time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC)) {
		t.Errorf("cursor = %v", cursor)
	}
	service.RunCycle(t.Context())
	if len(ntfy.ids) != 0 || !reflect.DeepEqual(telegram.ids, []string{"edition-12077"}) {
		t.Errorf("ntfy=%v telegram=%v", ntfy.ids, telegram.ids)
	}
}
