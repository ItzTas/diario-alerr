package gazettetest

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strconv"
	"sync"
	"testing"
	"time"
)

type Edition struct {
	ID        int64
	Published time.Time
	Title     string
	Pages     []string
}

type Server struct {
	*httptest.Server
	mu       sync.Mutex
	editions []Edition
	queries  []string
}

func NewServer(t testing.TB, editions ...Edition) *Server {
	t.Helper()
	s := &Server{editions: editions}
	mux := http.NewServeMux()
	mux.HandleFunc("GET /media", s.media)
	mux.HandleFunc("GET /pdf/{id}", s.pdf)
	s.Server = httptest.NewServer(mux)
	t.Cleanup(s.Close)
	return s
}

func (s *Server) APIURL() string {
	return s.URL + "/media"
}

func (s *Server) PDFURL(id int64) string {
	return s.URL + "/pdf/" + strconv.FormatInt(id, 10)
}

func (s *Server) Publish(e Edition) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.editions = append(s.editions, e)
}

func (s *Server) Queries() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.queries...)
}

func (s *Server) media(w http.ResponseWriter, r *http.Request) {
	s.mu.Lock()
	s.queries = append(s.queries, r.URL.RawQuery)
	after, _ := time.Parse("2006-01-02T15:04:05", r.URL.Query().Get("after"))
	var items []map[string]any
	for _, e := range s.editions {
		if !e.Published.After(after) {
			continue
		}
		items = append(items, map[string]any{
			"id":         e.ID,
			"date_gmt":   e.Published.UTC().Format("2006-01-02T15:04:05"),
			"source_url": s.PDFURL(e.ID),
			"title":      map[string]string{"rendered": e.Title},
		})
	}
	s.mu.Unlock()
	w.Header().Set("X-WP-TotalPages", "1")
	_ = json.NewEncoder(w).Encode(items)
}

func (s *Server) pdf(w http.ResponseWriter, r *http.Request) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, e := range s.editions {
		if strconv.FormatInt(e.ID, 10) == r.PathValue("id") {
			w.Header().Set("Content-Type", "application/pdf")
			_, _ = w.Write(PDF(e.Pages...))
			return
		}
	}
	http.NotFound(w, r)
}
