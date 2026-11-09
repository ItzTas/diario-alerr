package watch

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
	"time"
)

func TestReadStateFileReadsTheJSONStateOfEarlierVersions(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	written := `{
  "cursor": "2026-11-06T20:23:18+00:00",
  "seen": [12077, 12080, 12083],
  "alerts": [{"id": "welcome", "title": "Monitor ligado", "link": "https://diarios.al.rr.leg.br",
    "body": "Vigiando: TALES SABINI", "published": "2026-11-09T14:42:28.945552+00:00", "delivered": ["ntfy"]}],
  "last_success": "2026-11-09T14:42:28.945552+00:00"
}`
	if err := os.WriteFile(path, []byte(written), 0o600); err != nil {
		t.Fatal(err)
	}
	got, found, err := ReadStateFile(path)
	if err != nil || !found {
		t.Fatalf("ReadStateFile: found=%v err=%v", found, err)
	}
	lastSuccess := time.Date(2026, 11, 9, 14, 42, 28, 945552000, time.UTC)
	want := Snapshot{
		Cursor:      time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC),
		LastSuccess: &lastSuccess,
		Seen:        []int64{12077, 12080, 12083},
		Alerts: []StoredAlert{{
			ID:        "welcome",
			Title:     "Monitor ligado",
			Link:      "https://diarios.al.rr.leg.br",
			Body:      "Vigiando: TALES SABINI",
			Published: lastSuccess,
			Delivered: []string{"ntfy"},
		}},
	}
	if !got.LastSuccess.Equal(*want.LastSuccess) {
		t.Errorf("last success = %v, want %v", got.LastSuccess, want.LastSuccess)
	}
	got.LastSuccess = want.LastSuccess
	if !reflect.DeepEqual(got, want) {
		t.Errorf("snapshot = %+v\nwant       %+v", got, want)
	}
}

func TestReadStateFileWithoutAFileFindsNothing(t *testing.T) {
	_, found, err := ReadStateFile(filepath.Join(t.TempDir(), "missing.json"))
	if err != nil || found {
		t.Errorf("found=%v err=%v, want neither", found, err)
	}
}

func TestReadStateFileRejectsBrokenJSON(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	if err := os.WriteFile(path, []byte("{"), 0o600); err != nil {
		t.Fatal(err)
	}
	if _, _, err := ReadStateFile(path); err == nil {
		t.Error("ReadStateFile accepted broken JSON")
	}
}
