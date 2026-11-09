package feed

import (
	"context"
	"encoding/xml"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
)

type fakeRepository struct {
	alerts []core.Alert
	err    error
	limit  int32
}

func (f *fakeRepository) RecentAlerts(_ context.Context, limit int32) ([]core.Alert, error) {
	f.limit = limit
	return f.alerts, f.err
}

func serveFeed(t *testing.T, repo Repository, opts Options) *httptest.ResponseRecorder {
	t.Helper()
	mux := http.NewServeMux()
	RegisterRoutes(mux, &Handler{service: NewService(repo, opts), log: slog.New(slog.NewTextHandler(io.Discard, nil))})
	rec := httptest.NewRecorder()
	mux.ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/feed.xml", nil))
	return rec
}

func TestFeedListsTheAlertsTheRepositoryReturns(t *testing.T) {
	repo := &fakeRepository{alerts: []core.Alert{
		{ID: "edition-9", Title: "Found <you>", Link: "https://b#page=2", Body: "b2", Published: time.Date(2026, 11, 10, 12, 0, 0, 0, time.UTC)},
		{ID: "welcome", Title: "Started", Link: "https://a", Body: "b1", Published: time.Date(2026, 11, 9, 14, 42, 28, 0, time.UTC)},
	}}
	rec := serveFeed(t, repo, Options{Terms: []string{"TALES SABINI"}, PublicURL: "https://diario.example.com"})
	if rec.Code != http.StatusOK || !strings.HasPrefix(rec.Header().Get("Content-Type"), "application/rss+xml") {
		t.Fatalf("status %d, content type %q", rec.Code, rec.Header().Get("Content-Type"))
	}
	var document RSS
	if err := xml.Unmarshal(rec.Body.Bytes(), &document); err != nil {
		t.Fatal(err)
	}
	wantChannel := Channel{
		Title:       "ALE/RR Official Gazette: TALES SABINI",
		Link:        "https://diario.example.com",
		Description: "Editions of the ALE/RR Official Gazette where the watched terms appear",
		Items: []Item{
			{Title: "Found <you>", Link: "https://b#page=2", GUID: GUID{IsPermaLink: "false", Value: "edition-9"}, PubDate: "Tue, 10 Nov 2026 12:00:00 +0000", Description: "b2"},
			{Title: "Started", Link: "https://a", GUID: GUID{IsPermaLink: "false", Value: "welcome"}, PubDate: "Mon, 09 Nov 2026 14:42:28 +0000", Description: "b1"},
		},
	}
	if !reflect.DeepEqual(document.Channel, wantChannel) {
		t.Errorf("channel = %+v\nwant      %+v", document.Channel, wantChannel)
	}
	if repo.limit != maxItems {
		t.Errorf("asked for %d alerts, want %d", repo.limit, maxItems)
	}
}

func TestFeedLinksTheGazetteSiteWithoutAPublicURL(t *testing.T) {
	rec := serveFeed(t, &fakeRepository{}, Options{Terms: []string{"X"}})
	var document RSS
	if err := xml.Unmarshal(rec.Body.Bytes(), &document); err != nil {
		t.Fatal(err)
	}
	if document.Channel.Link != core.SiteURL {
		t.Errorf("link = %q, want %q", document.Channel.Link, core.SiteURL)
	}
}

func TestFeedAnswers500WhenTheRepositoryFails(t *testing.T) {
	rec := serveFeed(t, &fakeRepository{err: errors.New("db down")}, Options{})
	if rec.Code != http.StatusInternalServerError {
		t.Errorf("status = %d, want 500", rec.Code)
	}
}
