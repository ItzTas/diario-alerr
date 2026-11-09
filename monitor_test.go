package main

import (
	"context"
	"encoding/json"
	"encoding/xml"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"
)

func testMonitor(t *testing.T, cfg config, alerts ...*alert) *monitor {
	t.Helper()
	cfg.terms = []string{"TALES SABINI"}
	cfg.interval = 30 * time.Minute
	return &monitor{
		cfg:       cfg,
		client:    &http.Client{Timeout: 5 * time.Second},
		statePath: filepath.Join(t.TempDir(), "state.json"),
		started:   time.Now().UTC(),
		st:        &state{Seen: []int{}, Alerts: alerts},
	}
}

func TestDeliverRetriesOnlyTheChannelThatFailed(t *testing.T) {
	var received []map[string]any
	var auth []string
	calls := 0
	ntfy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		if calls == 1 {
			w.WriteHeader(http.StatusInternalServerError)
			return
		}
		var body map[string]any
		_ = json.NewDecoder(r.Body).Decode(&body)
		received = append(received, body)
		auth = append(auth, r.Header.Get("Authorization"))
	}))
	defer ntfy.Close()

	a := &alert{ID: "edition-1", Title: "T", Link: "L", Body: "B", Delivered: []string{"telegram"}}
	m := testMonitor(t, config{ntfyURL: ntfy.URL, ntfyTopic: "diario-alerr", ntfyToken: "tk_test"}, a)

	m.deliverPending(context.Background())
	if len(a.Delivered) != 1 {
		t.Fatalf("after a failed send delivered = %v, want only telegram", a.Delivered)
	}
	m.deliverPending(context.Background())
	if !reflect.DeepEqual(a.Delivered, []string{"telegram", "ntfy"}) {
		t.Fatalf("delivered = %v", a.Delivered)
	}
	want := map[string]any{"topic": "diario-alerr", "title": "T", "message": "B", "click": "L", "tags": []any{"rotating_light"}, "priority": float64(5)}
	if len(received) != 1 || !reflect.DeepEqual(received[0], want) {
		t.Errorf("ntfy payload = %v, want %v", received, want)
	}
	if auth[0] != "Bearer tk_test" {
		t.Errorf("Authorization = %q", auth[0])
	}
}

func TestTelegramErrorOmitsTheBotToken(t *testing.T) {
	m := testMonitor(t, config{telegramToken: "123:SECRET", telegramChatID: "42"})
	m.telegramAPI = "http://127.0.0.1:1"
	err := m.sendTelegram(context.Background(), &alert{ID: "x"})
	if err == nil {
		t.Fatal("sendTelegram to a closed port returned nil")
	}
	if strings.Contains(err.Error(), "SECRET") {
		t.Errorf("error leaks the token: %v", err)
	}
}

func TestChannelsFollowConfiguration(t *testing.T) {
	cases := map[string]struct {
		cfg  config
		want []string
	}{
		"none":     {config{}, []string{}},
		"telegram": {config{telegramToken: "t", telegramChatID: "c"}, []string{"telegram"}},
		"ntfy":     {config{ntfyURL: "http://n", ntfyTopic: "x"}, []string{"ntfy"}},
		"both":     {config{telegramToken: "t", telegramChatID: "c", ntfyURL: "http://n", ntfyTopic: "x"}, []string{"ntfy", "telegram"}},
	}
	for name, tc := range cases {
		got := mapKeys(testMonitor(t, tc.cfg).channels())
		if len(got) == 2 && got[0] == "telegram" {
			got[0], got[1] = got[1], got[0]
		}
		if !reflect.DeepEqual(got, tc.want) {
			t.Errorf("%s: channels = %v, want %v", name, got, tc.want)
		}
	}
}

func TestFeedListsAlertsNewestFirst(t *testing.T) {
	m := testMonitor(t, config{publicURL: "https://diario.example.com"},
		&alert{ID: "welcome", Title: "Ligado", Link: "https://a", Body: "b1", Published: "2026-11-09T14:42:28.945552+00:00"},
		&alert{ID: "edition-9", Title: "Achou <você>", Link: "https://b#page=2", Body: "b2", Published: "2026-11-10T12:00:00Z"},
	)
	rec := httptest.NewRecorder()
	m.routes().ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/feed.xml", nil))
	if rec.Code != http.StatusOK || !strings.HasPrefix(rec.Header().Get("Content-Type"), "application/rss+xml") {
		t.Fatalf("status %d, content type %q", rec.Code, rec.Header().Get("Content-Type"))
	}
	var feed rss
	if err := xml.Unmarshal(rec.Body.Bytes(), &feed); err != nil {
		t.Fatal(err)
	}
	if feed.Channel.Link != "https://diario.example.com" || feed.Channel.Title != "Diário da ALE/RR: TALES SABINI" {
		t.Errorf("channel = %+v", feed.Channel)
	}
	want := []rssItem{
		{Title: "Achou <você>", Link: "https://b#page=2", GUID: rssGUID{IsPermaLink: "false", Value: "edition-9"}, PubDate: "Tue, 10 Nov 2026 12:00:00 +0000", Description: "b2"},
		{Title: "Ligado", Link: "https://a", GUID: rssGUID{IsPermaLink: "false", Value: "welcome"}, PubDate: "Mon, 09 Nov 2026 14:42:28 +0000", Description: "b1"},
	}
	if !reflect.DeepEqual(feed.Channel.Items, want) {
		t.Errorf("items = %+v\nwant    %+v", feed.Channel.Items, want)
	}
}

func TestHealthzReflectsLastSuccessfulCheck(t *testing.T) {
	m := testMonitor(t, config{})
	cases := map[string]struct {
		lastSuccess time.Duration
		want        int
	}{
		"recent": {-time.Minute, http.StatusOK},
		"stale":  {-2 * time.Hour, http.StatusServiceUnavailable},
	}
	for name, tc := range cases {
		at := time.Now().Add(tc.lastSuccess)
		m.st.LastSuccess = &at
		rec := httptest.NewRecorder()
		m.routes().ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/healthz", nil))
		if rec.Code != tc.want {
			t.Errorf("%s: status %d, want %d", name, rec.Code, tc.want)
		}
	}
}

func TestLoadStateReadsTheStateFileOnTheServer(t *testing.T) {
	path := filepath.Join(t.TempDir(), "state.json")
	written := `{
  "cursor": "2026-11-06T20:23:18+00:00",
  "seen": [12077, 12080, 12083],
  "alerts": [{"id": "welcome", "title": "Monitor do Diário da ALE/RR ligado", "link": "https://diarios.al.rr.leg.br",
    "body": "Vigiando: TALES SABINI", "published": "2026-11-09T14:42:28.945552+00:00", "delivered": ["ntfy"]}],
  "last_success": "2026-11-09T14:42:28.945552+00:00"
}`
	if err := os.WriteFile(path, []byte(written), 0o600); err != nil {
		t.Fatal(err)
	}
	st, firstRun, err := loadState(path, time.Time{})
	if err != nil || firstRun {
		t.Fatalf("loadState: firstRun=%v err=%v", firstRun, err)
	}
	if !st.Cursor.Equal(time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC)) || len(st.Seen) != 3 {
		t.Errorf("cursor %v seen %v", st.Cursor, st.Seen)
	}
	if len(st.Alerts) != 1 || !reflect.DeepEqual(st.Alerts[0].Delivered, []string{"ntfy"}) || st.LastSuccess == nil {
		t.Errorf("alerts %+v last_success %v", st.Alerts, st.LastSuccess)
	}
}

func TestFirstRunQueuesTheWelcomeAlertAndSavesState(t *testing.T) {
	cfg := config{terms: []string{"TALES SABINI", "0021805e"}, interval: 30 * time.Minute, dataDir: t.TempDir(), startAfter: time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC)}
	m, err := newMonitor(cfg)
	if err != nil {
		t.Fatal(err)
	}
	if len(m.st.Alerts) != 1 || m.st.Alerts[0].ID != welcomeID || m.st.Alerts[0].Body != "Vigiando: TALES SABINI, 0021805e\nVerificação a cada 30 min." {
		t.Errorf("alerts = %+v", m.st.Alerts)
	}
	saved, firstRun, err := loadState(m.statePath, time.Time{})
	if err != nil || firstRun || !saved.Cursor.Equal(cfg.startAfter) {
		t.Errorf("saved state: firstRun=%v err=%v cursor=%v", firstRun, err, saved.Cursor)
	}
}
