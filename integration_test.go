package main

import (
	"encoding/json"
	"encoding/xml"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os/exec"
	"reflect"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/core/coretest"
	"github.com/ItzTas/diario-alerr/internal/gazette/gazettetest"
	"github.com/ItzTas/diario-alerr/internal/modules/feed"
)

type inbox struct {
	mu       sync.Mutex
	messages []map[string]any
}

func (b *inbox) handler(w http.ResponseWriter, r *http.Request) {
	var body map[string]any
	_ = json.NewDecoder(r.Body).Decode(&body)
	b.mu.Lock()
	b.messages = append(b.messages, body)
	b.mu.Unlock()
	w.WriteHeader(http.StatusOK)
}

func (b *inbox) field(key string) []string {
	b.mu.Lock()
	defer b.mu.Unlock()
	values := make([]string, len(b.messages))
	for i, message := range b.messages {
		values[i], _ = message[key].(string)
	}
	return values
}

type bench struct {
	t        *testing.T
	cfg      core.Config
	gazette  *gazettetest.Server
	ntfy     *inbox
	telegram *inbox
	now      time.Time
}

func newBench(t *testing.T) *bench {
	t.Helper()
	if _, err := exec.LookPath("pdftotext"); err != nil {
		t.Skip("pdftotext is not installed")
	}
	b := &bench{t: t, gazette: gazettetest.NewServer(t), ntfy: &inbox{}, telegram: &inbox{}, now: time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)}
	ntfy := httptest.NewServer(http.HandlerFunc(b.ntfy.handler))
	t.Cleanup(ntfy.Close)
	telegram := httptest.NewServer(http.HandlerFunc(b.telegram.handler))
	t.Cleanup(telegram.Close)
	b.cfg = core.Config{
		DatabaseURL:    coretest.Database(t),
		Terms:          []string{"Tales Sabini"},
		Interval:       30 * time.Minute,
		StartAfter:     time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC),
		PublicURL:      "https://diario.example.com",
		HTTPPort:       "0",
		GazetteAPIURL:  b.gazette.APIURL(),
		TelegramAPIURL: telegram.URL,
		TelegramToken:  "123:SECRET",
		TelegramChatID: "42",
		NtfyURL:        ntfy.URL,
		NtfyTopic:      "diario-alerr",
	}
	return b
}

func (b *bench) start() *application {
	b.t.Helper()
	logger := slog.New(slog.NewTextHandler(io.Discard, nil))
	app, err := assemble(b.t.Context(), b.cfg, logger, func() time.Time { return b.now })
	if err != nil {
		b.t.Fatal(err)
	}
	b.t.Cleanup(app.Close)
	return app
}

func (b *bench) get(app *application, path string) *httptest.ResponseRecorder {
	rec := httptest.NewRecorder()
	app.handler.ServeHTTP(rec, httptest.NewRequestWithContext(b.t.Context(), http.MethodGet, path, nil))
	return rec
}

func TestMonitorAlertsEveryChannelAndTheFeedWhenANameAppears(t *testing.T) {
	b := newBench(t)
	b.gazette.Publish(gazettetest.Edition{ID: 501, Published: time.Date(2026, 11, 2, 10, 0, 0, 0, time.UTC), Title: "ED 501", Pages: []string{"Nada aqui"}})
	b.gazette.Publish(gazettetest.Edition{ID: 502, Published: time.Date(2026, 11, 3, 10, 0, 0, 0, time.UTC), Title: "ED 502", Pages: []string{"Sumario", "Nomeacao de TALES SABINI"}})
	app := b.start()

	if rec := b.get(app, "/healthz"); rec.Code != http.StatusOK {
		t.Errorf("healthz before the first cycle = %d", rec.Code)
	}
	app.watcher.RunCycle(t.Context())

	wantTitles := []string{"ALE/RR Official Gazette monitor started", "Your name appeared in the ALE/RR Official Gazette: ED 502"}
	if got := b.ntfy.field("title"); !reflect.DeepEqual(got, wantTitles) {
		t.Errorf("ntfy titles = %v", got)
	}
	if got := b.ntfy.field("click"); got[1] != b.gazette.PDFURL(502)+"#page=2" {
		t.Errorf("ntfy click = %v", got)
	}
	if got := b.telegram.field("text"); len(got) != 2 || !strings.Contains(got[1], "Tales Sabini: p. 2") {
		t.Errorf("telegram texts = %q", got)
	}

	var document feed.RSS
	if err := xml.Unmarshal(b.get(app, "/feed.xml").Body.Bytes(), &document); err != nil {
		t.Fatal(err)
	}
	var guids []string
	for _, item := range document.Channel.Items {
		guids = append(guids, item.GUID.Value)
	}
	if !reflect.DeepEqual(guids, []string{"edition-502", core.WelcomeAlertID}) {
		t.Errorf("feed guids = %v", guids)
	}
}

func TestMonitorRemembersWhatItSentAcrossRestarts(t *testing.T) {
	b := newBench(t)
	b.gazette.Publish(gazettetest.Edition{ID: 601, Published: time.Date(2026, 11, 2, 10, 0, 0, 0, time.UTC), Title: "ED 601", Pages: []string{"TALES SABINI"}})
	b.start().watcher.RunCycle(t.Context())

	b.gazette.Publish(gazettetest.Edition{ID: 602, Published: time.Date(2026, 11, 4, 10, 0, 0, 0, time.UTC), Title: "ED 602", Pages: []string{"TALES SABINI aparece de novo"}})
	b.now = b.now.Add(time.Hour)
	restarted := b.start()
	restarted.watcher.RunCycle(t.Context())

	want := []string{
		"ALE/RR Official Gazette monitor started",
		"Your name appeared in the ALE/RR Official Gazette: ED 601",
		"Your name appeared in the ALE/RR Official Gazette: ED 602",
	}
	if got := b.ntfy.field("title"); !reflect.DeepEqual(got, want) {
		t.Errorf("ntfy titles = %v", got)
	}
	queries := b.gazette.Queries()
	if last := queries[len(queries)-1]; !strings.Contains(last, "after=2026-11-01T10%3A00%3A00") {
		t.Errorf("second listing = %q, want it to start a day before the newest edition", last)
	}
}
