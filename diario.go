package main

import (
	"context"
	"encoding/json"
	"fmt"
	"html"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"slices"
	"strconv"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/runes"
	"golang.org/x/text/transform"
	"golang.org/x/text/unicode/norm"
)

const (
	defaultAPIURL = "https://diarios.al.rr.leg.br/wp-json/wp/v2/media"
	siteURL       = "https://diarios.al.rr.leg.br"
	userAgent     = "diario-alerr-monitor/2.0"
	perPage       = 100
	snippetRadius = 160
	pdfTimeout    = 5 * time.Minute
)

type edition struct {
	ID        int
	Published time.Time
	URL       string
	Title     string
}

type apiMedia struct {
	ID        int    `json:"id"`
	DateGMT   string `json:"date_gmt"`
	SourceURL string `json:"source_url"`
	Title     struct {
		Rendered string `json:"rendered"`
	} `json:"title"`
}

type hits struct {
	Pages     map[string][]int
	Order     []string
	FirstPage int
	Snippet   string
}

func (m apiMedia) edition() (edition, error) {
	published, err := time.Parse("2006-01-02T15:04:05", m.DateGMT)
	if err != nil {
		return edition{}, fmt.Errorf("edition %d: date_gmt %q: %w", m.ID, m.DateGMT, err)
	}
	return edition{ID: m.ID, Published: published.UTC(), URL: m.SourceURL, Title: html.UnescapeString(m.Title.Rendered)}, nil
}

func get(ctx context.Context, client *http.Client, target string) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", userAgent)
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	if resp.StatusCode != http.StatusOK {
		_ = resp.Body.Close()
		return nil, fmt.Errorf("GET %s: %s", target, resp.Status)
	}
	return resp, nil
}

func listEditions(ctx context.Context, client *http.Client, apiURL string, after time.Time) ([]edition, error) {
	var editions []edition
	for page := 1; ; page++ {
		query := url.Values{
			"mime_type": {"application/pdf"},
			"after":     {after.Format("2006-01-02T15:04:05")},
			"orderby":   {"date"},
			"order":     {"asc"},
			"per_page":  {strconv.Itoa(perPage)},
			"page":      {strconv.Itoa(page)},
			"_fields":   {"id,date_gmt,source_url,title"},
		}
		batch, totalPages, err := fetchMediaPage(ctx, client, apiURL+"?"+query.Encode())
		if err != nil {
			return nil, err
		}
		editions = append(editions, batch...)
		if page >= totalPages {
			return editions, nil
		}
	}
}

func fetchMediaPage(ctx context.Context, client *http.Client, target string) ([]edition, int, error) {
	resp, err := get(ctx, client, target)
	if err != nil {
		return nil, 0, err
	}
	defer func() { _ = resp.Body.Close() }()
	var items []apiMedia
	if err := json.NewDecoder(resp.Body).Decode(&items); err != nil {
		return nil, 0, fmt.Errorf("decode media list: %w", err)
	}
	editions := make([]edition, 0, len(items))
	for _, item := range items {
		e, err := item.edition()
		if err != nil {
			return nil, 0, err
		}
		editions = append(editions, e)
	}
	totalPages, err := strconv.Atoi(resp.Header.Get("X-WP-TotalPages"))
	if err != nil {
		totalPages = 1
	}
	return editions, totalPages, nil
}

func scanEdition(ctx context.Context, client *http.Client, e edition, terms []string) (*hits, error) {
	pages, err := downloadPages(ctx, client, e.URL)
	if err != nil {
		return nil, err
	}
	normalized := make([]string, len(pages))
	for i, page := range pages {
		normalized[i] = normalize(page)
	}
	return findTerms(normalized, terms), nil
}

func downloadPages(ctx context.Context, client *http.Client, pdfURL string) ([]string, error) {
	resp, err := get(ctx, client, pdfURL)
	if err != nil {
		return nil, err
	}
	defer func() { _ = resp.Body.Close() }()
	file, err := os.CreateTemp("", "edition-*.pdf")
	if err != nil {
		return nil, fmt.Errorf("create temp file: %w", err)
	}
	defer func() { _ = os.Remove(file.Name()) }()
	_, copyErr := io.Copy(file, resp.Body)
	closeErr := file.Close()
	if copyErr != nil {
		return nil, fmt.Errorf("download %s: %w", pdfURL, copyErr)
	}
	if closeErr != nil {
		return nil, fmt.Errorf("write %s: %w", file.Name(), closeErr)
	}
	return extractPages(ctx, file.Name())
}

func extractPages(ctx context.Context, path string) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, pdfTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "pdftotext", "-enc", "UTF-8", path, "-").Output()
	if err != nil {
		return nil, fmt.Errorf("pdftotext: %w", err)
	}
	return strings.Split(string(out), "\f"), nil
}

var stripMarks = transform.Chain(norm.NFKD, runes.Remove(runes.In(unicode.Mn)), norm.NFC)

func normalize(text string) string {
	stripped, _, err := transform.String(stripMarks, text)
	if err != nil {
		stripped = text
	}
	return strings.Join(strings.Fields(strings.ToUpper(stripped)), " ")
}

func findTerms(pages []string, terms []string) *hits {
	found := hits{Pages: map[string][]int{}, FirstPage: len(pages) + 1}
	firstNeedle := ""
	for _, term := range terms {
		needle := normalize(term)
		for number, text := range pages {
			if !strings.Contains(text, needle) {
				continue
			}
			found.Pages[term] = append(found.Pages[term], number+1)
			if number+1 < found.FirstPage {
				found.FirstPage, firstNeedle = number+1, needle
			}
		}
		if len(found.Pages[term]) > 0 {
			found.Order = append(found.Order, term)
		}
	}
	if len(found.Order) == 0 {
		return nil
	}
	found.Snippet = snippet(pages[found.FirstPage-1], firstNeedle)
	return &found
}

func snippet(text, needle string) string {
	start := utf8.RuneCountInString(text[:strings.Index(text, needle)])
	chars := []rune(text)
	from := max(0, start-snippetRadius)
	to := min(len(chars), start+utf8.RuneCountInString(needle)+snippetRadius)
	return string(chars[from:to])
}

func formatPages(numbers []int) string {
	label := "págs."
	if len(numbers) == 1 {
		label = "pág."
	}
	parts := make([]string, len(numbers))
	for i, n := range numbers {
		parts[i] = strconv.Itoa(n)
	}
	return label + " " + strings.Join(parts, ", ")
}

func newAlert(e edition, h *hits) *alert {
	lines := make([]string, 0, len(h.Order))
	for _, term := range h.Order {
		lines = append(lines, term+": "+formatPages(h.Pages[term]))
	}
	return &alert{
		ID:        "edition-" + strconv.Itoa(e.ID),
		Title:     "Seu nome apareceu no Diário da ALE/RR: " + e.Title,
		Link:      e.URL + "#page=" + strconv.Itoa(h.FirstPage),
		Body:      strings.Join(lines, "\n") + "\n\n…" + h.Snippet + "…",
		Published: e.Published.Format(time.RFC3339),
		Delivered: []string{},
	}
}

func unseen(editions []edition, seen []int) []edition {
	return slices.DeleteFunc(editions, func(e edition) bool { return slices.Contains(seen, e.ID) })
}
