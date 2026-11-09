package gazette

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
)

const perPage = 100

type Client struct {
	HTTP   *http.Client
	APIURL string
}

func (c *Client) ListEditions(ctx context.Context, after time.Time) ([]Edition, error) {
	var editions []Edition
	for page := 1; ; page++ {
		query := url.Values{
			"mime_type": {"application/pdf"},
			"after":     {after.UTC().Format("2006-01-02T15:04:05")},
			"orderby":   {"date"},
			"order":     {"asc"},
			"per_page":  {strconv.Itoa(perPage)},
			"page":      {strconv.Itoa(page)},
			"_fields":   {"id,date_gmt,source_url,title"},
		}
		batch, totalPages, err := c.fetchPage(ctx, c.APIURL+"?"+query.Encode())
		if err != nil {
			return nil, err
		}
		editions = append(editions, batch...)
		if page >= totalPages {
			return editions, nil
		}
	}
}

func (c *Client) fetchPage(ctx context.Context, target string) ([]Edition, int, error) {
	resp, err := core.Get(ctx, c.HTTP, target)
	if err != nil {
		return nil, 0, err
	}
	defer func() { _ = resp.Body.Close() }()
	var items []media
	if err := json.NewDecoder(resp.Body).Decode(&items); err != nil {
		return nil, 0, fmt.Errorf("decode media list: %w", err)
	}
	editions := make([]Edition, 0, len(items))
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

func (c *Client) Scan(ctx context.Context, e Edition, terms []string) (*Hits, error) {
	pages, err := c.downloadPages(ctx, e.URL)
	if err != nil {
		return nil, err
	}
	normalized := make([]string, len(pages))
	for i, page := range pages {
		normalized[i] = Normalize(page)
	}
	return FindTerms(normalized, terms), nil
}
