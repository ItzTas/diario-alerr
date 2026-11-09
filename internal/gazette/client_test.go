package gazette

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/gazette/gazettetest"
)

func TestListEditionsFollowsPagination(t *testing.T) {
	var queries []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		queries = append(queries, r.URL.RawQuery)
		page, _ := strconv.Atoi(r.URL.Query().Get("page"))
		w.Header().Set("X-WP-TotalPages", "2")
		_ = json.NewEncoder(w).Encode([]map[string]any{{
			"id":         page,
			"date_gmt":   "2026-11-0" + strconv.Itoa(page) + "T10:00:00",
			"source_url": "https://x/" + strconv.Itoa(page) + ".pdf",
			"title":      map[string]string{"rendered": "ED &#8211; " + strconv.Itoa(page)},
		}})
	}))
	defer server.Close()

	client := &Client{HTTP: server.Client(), APIURL: server.URL}
	got, err := client.ListEditions(t.Context(), time.Date(2026, 10, 31, 0, 0, 0, 0, time.UTC))
	if err != nil {
		t.Fatal(err)
	}
	want := []Edition{
		{ID: 1, Published: time.Date(2026, 11, 1, 10, 0, 0, 0, time.UTC), URL: "https://x/1.pdf", Title: "ED – 1"},
		{ID: 2, Published: time.Date(2026, 11, 2, 10, 0, 0, 0, time.UTC), URL: "https://x/2.pdf", Title: "ED – 2"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("editions = %+v\nwant       %+v", got, want)
	}
	if len(queries) != 2 || !strings.Contains(queries[0], "after=2026-10-31T00%3A00%3A00") || !strings.Contains(queries[0], "mime_type=application%2Fpdf") {
		t.Errorf("queries = %v", queries)
	}
}

func TestListEditionsRejectsAnUnreadableDate(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		_, _ = w.Write([]byte(`[{"id": 7, "date_gmt": "yesterday"}]`))
	}))
	defer server.Close()
	client := &Client{HTTP: server.Client(), APIURL: server.URL}
	if _, err := client.ListEditions(t.Context(), time.Now()); err == nil || !strings.Contains(err.Error(), "edition 7") {
		t.Errorf("err = %v, want one naming edition 7", err)
	}
}

func TestScanFindsTermsInThePDFPages(t *testing.T) {
	requirePDFToText(t)
	published := time.Date(2026, 11, 6, 20, 0, 0, 0, time.UTC)
	server := gazettetest.NewServer(t, gazettetest.Edition{ID: 5, Published: published, Pages: []string{"Mesa Diretora", "Nomeacao de TALES SABINI"}})
	client := &Client{HTTP: server.Client(), APIURL: server.APIURL()}
	hits, err := client.Scan(t.Context(), Edition{ID: 5, URL: server.PDFURL(5)}, []string{"Tales Sabini"})
	if err != nil {
		t.Fatal(err)
	}
	if hits == nil || !reflect.DeepEqual(hits.Pages, map[string][]int{"Tales Sabini": {2}}) {
		t.Errorf("hits = %+v", hits)
	}
}

func TestScanReportsAMissingPDF(t *testing.T) {
	server := gazettetest.NewServer(t)
	client := &Client{HTTP: server.Client(), APIURL: server.APIURL()}
	if _, err := client.Scan(t.Context(), Edition{URL: server.PDFURL(99)}, []string{"x"}); err == nil {
		t.Error("Scan of a missing PDF returned nil")
	}
}

func requirePDFToText(t *testing.T) {
	t.Helper()
	if _, err := exec.LookPath("pdftotext"); err == nil {
		return
	}
	if os.Getenv("CI") != "" {
		t.Fatal("pdftotext is required when CI is set")
	}
	t.Skip("pdftotext is not installed")
}
