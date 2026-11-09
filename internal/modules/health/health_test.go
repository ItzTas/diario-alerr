package health

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

type fakeRepository struct {
	last *time.Time
	err  error
}

func (f fakeRepository) LastSuccess(context.Context) (*time.Time, error) {
	return f.last, f.err
}

func TestHealthzReflectsTheLastSuccessfulCheck(t *testing.T) {
	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	at := func(ago time.Duration) *time.Time { value := now.Add(-ago); return &value }
	cases := map[string]struct {
		repo    fakeRepository
		started time.Time
		want    int
	}{
		"recent check":                   {fakeRepository{last: at(time.Minute)}, now.Add(-24 * time.Hour), http.StatusOK},
		"stale check":                    {fakeRepository{last: at(2 * time.Hour)}, now, http.StatusServiceUnavailable},
		"no check yet, just started":     {fakeRepository{}, now.Add(-time.Minute), http.StatusOK},
		"no check yet, started long ago": {fakeRepository{}, now.Add(-2 * time.Hour), http.StatusServiceUnavailable},
		"database unreachable":           {fakeRepository{err: errors.New("db down")}, now, http.StatusServiceUnavailable},
	}
	for name, tc := range cases {
		mux := http.NewServeMux()
		service := NewService(tc.repo, Options{Started: tc.started, Interval: 30 * time.Minute, Now: func() time.Time { return now }})
		RegisterRoutes(mux, &Handler{service: service, log: slog.New(slog.NewTextHandler(io.Discard, nil))})
		rec := httptest.NewRecorder()
		mux.ServeHTTP(rec, httptest.NewRequestWithContext(t.Context(), http.MethodGet, "/healthz", nil))
		if rec.Code != tc.want {
			t.Errorf("%s: status %d, want %d", name, rec.Code, tc.want)
		}
	}
}
