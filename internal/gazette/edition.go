package gazette

import (
	"fmt"
	"html"
	"time"
)

type Edition struct {
	ID        int64
	Published time.Time
	URL       string
	Title     string
}

type media struct {
	ID        int64  `json:"id"`
	DateGMT   string `json:"date_gmt"`
	SourceURL string `json:"source_url"`
	Title     struct {
		Rendered string `json:"rendered"`
	} `json:"title"`
}

func (m media) edition() (Edition, error) {
	published, err := time.Parse("2006-01-02T15:04:05", m.DateGMT)
	if err != nil {
		return Edition{}, fmt.Errorf("edition %d: date_gmt %q: %w", m.ID, m.DateGMT, err)
	}
	return Edition{ID: m.ID, Published: published.UTC(), URL: m.SourceURL, Title: html.UnescapeString(m.Title.Rendered)}, nil
}
