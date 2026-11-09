package main

import (
	"encoding/xml"
	"net/http"
	"slices"
	"strings"
	"time"
)

type rss struct {
	XMLName xml.Name   `xml:"rss"`
	Version string     `xml:"version,attr"`
	Channel rssChannel `xml:"channel"`
}

type rssChannel struct {
	Title       string    `xml:"title"`
	Link        string    `xml:"link"`
	Description string    `xml:"description"`
	Items       []rssItem `xml:"item"`
}

type rssItem struct {
	Title       string  `xml:"title"`
	Link        string  `xml:"link"`
	GUID        rssGUID `xml:"guid"`
	PubDate     string  `xml:"pubDate,omitempty"`
	Description string  `xml:"description"`
}

type rssGUID struct {
	IsPermaLink string `xml:"isPermaLink,attr"`
	Value       string `xml:",chardata"`
}

func (m *monitor) renderFeed() ([]byte, error) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	link := m.cfg.publicURL
	if link == "" {
		link = siteURL
	}
	items := make([]rssItem, 0, len(m.st.Alerts))
	for _, a := range slices.Backward(m.st.Alerts) {
		items = append(items, rssItem{
			Title:       a.Title,
			Link:        a.Link,
			GUID:        rssGUID{IsPermaLink: "false", Value: a.ID},
			PubDate:     rfc822(a.Published),
			Description: a.Body,
		})
	}
	feed := rss{Version: "2.0", Channel: rssChannel{
		Title:       "Diário da ALE/RR: " + strings.Join(m.cfg.terms, ", "),
		Link:        link,
		Description: "Edições do Diário da ALE/RR em que os termos vigiados aparecem",
		Items:       items,
	}}
	body, err := xml.Marshal(feed)
	if err != nil {
		return nil, err
	}
	return append([]byte(xml.Header), body...), nil
}

func rfc822(published string) string {
	parsed, err := time.Parse(time.RFC3339Nano, published)
	if err != nil {
		return ""
	}
	return parsed.Format(time.RFC1123Z)
}

func (m *monitor) healthy() bool {
	m.mu.RLock()
	defer m.mu.RUnlock()
	reference := m.started
	if m.st.LastSuccess != nil {
		reference = *m.st.LastSuccess
	}
	return time.Since(reference) < 3*m.cfg.interval
}

func (m *monitor) routes() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("GET /feed.xml", func(w http.ResponseWriter, _ *http.Request) {
		body, err := m.renderFeed()
		if err != nil {
			http.Error(w, "could not render feed", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/rss+xml; charset=utf-8")
		_, _ = w.Write(body)
	})
	mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, _ *http.Request) {
		if m.healthy() {
			_, _ = w.Write([]byte("ok"))
			return
		}
		http.Error(w, "stale", http.StatusServiceUnavailable)
	})
	return mux
}
