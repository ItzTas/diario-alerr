package watch

import (
	"errors"
	"reflect"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/core/coretest"
)

func testAlert(id string, published time.Time) core.Alert {
	return core.Alert{ID: id, Title: "title " + id, Link: "https://x/" + id, Body: "body " + id, Published: published}
}

func TestInitializeRestoresTheSnapshotOnlyOnce(t *testing.T) {
	repo := NewPostgres(coretest.Pool(t))
	ctx := t.Context()
	if _, err := repo.Cursor(ctx); !errors.Is(err, ErrNotInitialized) {
		t.Fatalf("cursor before Initialize: err = %v, want ErrNotInitialized", err)
	}
	cursor := time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC)
	welcome := testAlert("welcome", cursor)
	created, err := repo.Initialize(ctx, Snapshot{
		Cursor: cursor,
		Seen:   []int64{7, 9},
		Alerts: []StoredAlert{{Alert: welcome, Delivered: []string{"ntfy"}}},
	})
	if err != nil || !created {
		t.Fatalf("Initialize: created=%v err=%v", created, err)
	}
	again, err := repo.Initialize(ctx, Snapshot{Cursor: cursor.Add(time.Hour), Alerts: []StoredAlert{{Alert: testAlert("other", cursor)}}})
	if err != nil || again {
		t.Fatalf("second Initialize: created=%v err=%v", again, err)
	}

	if got, err := repo.Cursor(ctx); err != nil || !got.Equal(cursor) {
		t.Errorf("cursor = %v, %v; want %v", got, err, cursor)
	}
	if seen, err := repo.SeenEditions(ctx, []int64{7, 8, 9}); err != nil || !reflect.DeepEqual(seen, []int64{7, 9}) {
		t.Errorf("seen = %v, %v", seen, err)
	}
	if pending, err := repo.PendingAlerts(ctx, "ntfy"); err != nil || len(pending) != 0 {
		t.Errorf("pending for ntfy = %v, %v; want none", pending, err)
	}
	if pending, err := repo.PendingAlerts(ctx, "telegram"); err != nil || !reflect.DeepEqual(pending, []core.Alert{welcome}) {
		t.Errorf("pending for telegram = %v, %v", pending, err)
	}
}

func TestRecordEditionMovesTheCursorForwardOnly(t *testing.T) {
	repo := NewPostgres(coretest.Pool(t))
	ctx := t.Context()
	start := time.Date(2026, 11, 5, 0, 0, 0, 0, time.UTC)
	if _, err := repo.Initialize(ctx, Snapshot{Cursor: start}); err != nil {
		t.Fatal(err)
	}
	match := testAlert("edition-2", start.Add(48*time.Hour))
	records := []EditionRecord{
		{ID: 1, Published: start.Add(-time.Hour)},
		{ID: 2, Published: start.Add(48 * time.Hour), Alert: &match},
		{ID: 3, Published: start.Add(24 * time.Hour)},
	}
	for _, record := range records {
		if err := repo.RecordEdition(ctx, record); err != nil {
			t.Fatal(err)
		}
	}
	if got, err := repo.Cursor(ctx); err != nil || !got.Equal(start.Add(48*time.Hour)) {
		t.Errorf("cursor = %v, %v", got, err)
	}
	if seen, err := repo.SeenEditions(ctx, []int64{1, 2, 3}); err != nil || len(seen) != 3 {
		t.Errorf("seen = %v, %v", seen, err)
	}
	if pending, err := repo.PendingAlerts(ctx, "ntfy"); err != nil || !reflect.DeepEqual(pending, []core.Alert{match}) {
		t.Errorf("pending = %v, %v", pending, err)
	}
}

func TestMarkDeliveredClearsTheAlertForThatChannelOnly(t *testing.T) {
	repo := NewPostgres(coretest.Pool(t))
	ctx := t.Context()
	at := time.Date(2026, 11, 5, 0, 0, 0, 0, time.UTC)
	first, second := testAlert("a", at), testAlert("b", at)
	if _, err := repo.Initialize(ctx, Snapshot{Cursor: at, Alerts: []StoredAlert{{Alert: first}, {Alert: second}}}); err != nil {
		t.Fatal(err)
	}
	if err := repo.MarkDelivered(ctx, "a", "ntfy"); err != nil {
		t.Fatal(err)
	}
	if pending, _ := repo.PendingAlerts(ctx, "ntfy"); !reflect.DeepEqual(pending, []core.Alert{second}) {
		t.Errorf("pending for ntfy = %v", pending)
	}
	if pending, _ := repo.PendingAlerts(ctx, "telegram"); !reflect.DeepEqual(pending, []core.Alert{first, second}) {
		t.Errorf("pending for telegram = %v", pending)
	}
}

func TestPruneKeepsTheNewestEditionsAndAlerts(t *testing.T) {
	repo := NewPostgres(coretest.Pool(t))
	ctx := t.Context()
	at := time.Date(2026, 11, 5, 0, 0, 0, 0, time.UTC)
	alerts := []StoredAlert{{Alert: testAlert("a", at)}, {Alert: testAlert("b", at)}, {Alert: testAlert("c", at)}}
	if _, err := repo.Initialize(ctx, Snapshot{Cursor: at, Seen: []int64{1, 2, 3, 4}, Alerts: alerts}); err != nil {
		t.Fatal(err)
	}
	if err := repo.Prune(ctx, 2, 2); err != nil {
		t.Fatal(err)
	}
	if seen, _ := repo.SeenEditions(ctx, []int64{1, 2, 3, 4}); !reflect.DeepEqual(seen, []int64{3, 4}) {
		t.Errorf("seen = %v, want [3 4]", seen)
	}
	if pending, _ := repo.PendingAlerts(ctx, "ntfy"); !reflect.DeepEqual(pending, []core.Alert{alerts[1].Alert, alerts[2].Alert}) {
		t.Errorf("alerts left = %v", pending)
	}
}
