package main

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"reflect"
	"strconv"
	"strings"
	"testing"
	"time"
)

func TestNormalize(t *testing.T) {
	cases := map[string]string{
		"José da Silva":          "JOSE DA SILVA",
		"  tales\n\tSABÍNI  ":    "TALES SABINI",
		"Conceição Ávila Müller": "CONCEICAO AVILA MULLER",
		"0021805e":               "0021805E",
	}
	for input, want := range cases {
		if got := normalize(input); got != want {
			t.Errorf("normalize(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestFindTermsListsEveryPageOfEveryTerm(t *testing.T) {
	pages := []string{
		normalize("Sumário Mesa Diretora"),
		normalize("Classificação: 0021805e TALES\nSABINI 132,61 8"),
		normalize("Nomeação de Tales Sabíni para o cargo"),
	}
	got := findTerms(pages, []string{"Tales Sabini", "0021805e", "Fulano"})
	if got == nil {
		t.Fatal("findTerms returned nil, want hits")
	}
	wantPages := map[string][]int{"Tales Sabini": {2, 3}, "0021805e": {2}}
	if !reflect.DeepEqual(got.Pages, wantPages) {
		t.Errorf("pages = %v, want %v", got.Pages, wantPages)
	}
	if !reflect.DeepEqual(got.Order, []string{"Tales Sabini", "0021805e"}) {
		t.Errorf("order = %v", got.Order)
	}
	if got.FirstPage != 2 {
		t.Errorf("first page = %d, want 2", got.FirstPage)
	}
	if !strings.Contains(got.Snippet, "TALES SABINI") {
		t.Errorf("snippet %q does not contain the match", got.Snippet)
	}
}

func TestFindTermsWithoutMatchReturnsNil(t *testing.T) {
	if got := findTerms([]string{normalize("nada aqui")}, []string{"Tales Sabini"}); got != nil {
		t.Errorf("findTerms = %+v, want nil", got)
	}
}

func TestSnippetKeepsRunesIntact(t *testing.T) {
	text := strings.Repeat("º", 300) + " TALES SABINI " + strings.Repeat("–", 300)
	got := snippet(text, "TALES SABINI")
	if !strings.Contains(got, "TALES SABINI") || !strings.HasPrefix(got, "º") || !strings.HasSuffix(got, "–") {
		t.Errorf("snippet = %q", got)
	}
	if want := snippetRadius*2 + len("TALES SABINI"); len([]rune(got)) != want {
		t.Errorf("snippet has %d runes, want %d", len([]rune(got)), want)
	}
}

func TestNewAlertDescribesPagesAndLinksFirstOne(t *testing.T) {
	e := edition{ID: 42, Published: time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC), URL: "https://x/ed.pdf", Title: "DIARIO-ALE-RR-ED.-4763"}
	h := &hits{Pages: map[string][]int{"TALES SABINI": {3, 5}, "0021805e": {3}}, Order: []string{"TALES SABINI", "0021805e"}, FirstPage: 3, Snippet: "TRECHO"}
	got := newAlert(e, h)
	want := &alert{
		ID:        "edition-42",
		Title:     "Seu nome apareceu no Diário da ALE/RR: DIARIO-ALE-RR-ED.-4763",
		Link:      "https://x/ed.pdf#page=3",
		Body:      "TALES SABINI: págs. 3, 5\n0021805e: pág. 3\n\n…TRECHO…",
		Published: "2026-11-06T20:23:18Z",
		Delivered: []string{},
	}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("alert = %+v\nwant    %+v", got, want)
	}
}

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

	got, err := listEditions(context.Background(), server.Client(), server.URL, time.Date(2026, 10, 31, 0, 0, 0, 0, time.UTC))
	if err != nil {
		t.Fatal(err)
	}
	want := []edition{
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

func TestUnseenDropsKnownEditions(t *testing.T) {
	got := unseen([]edition{{ID: 1}, {ID: 2}, {ID: 3}}, []int{2})
	if !reflect.DeepEqual(got, []edition{{ID: 1}, {ID: 3}}) {
		t.Errorf("unseen = %+v", got)
	}
}
