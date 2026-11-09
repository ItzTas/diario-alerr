package feed

import (
	"context"
	"encoding/xml"
	"strings"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
)

const maxItems = 100

type Service struct {
	repo Repository
	opts Options
}

func NewService(repo Repository, opts Options) *Service {
	return &Service{repo: repo, opts: opts}
}

func (s *Service) Render(ctx context.Context) ([]byte, error) {
	alerts, err := s.repo.RecentAlerts(ctx, maxItems)
	if err != nil {
		return nil, err
	}
	link := s.opts.PublicURL
	if link == "" {
		link = core.SiteURL
	}
	items := make([]Item, len(alerts))
	for i, a := range alerts {
		items[i] = Item{
			Title:       a.Title,
			Link:        a.Link,
			GUID:        GUID{IsPermaLink: "false", Value: a.ID},
			PubDate:     a.Published.UTC().Format(time.RFC1123Z),
			Description: a.Body,
		}
	}
	document := RSS{Version: "2.0", Channel: Channel{
		Title:       core.GazetteName + ": " + strings.Join(s.opts.Terms, ", "),
		Link:        link,
		Description: "Editions of the " + core.GazetteName + " where the watched terms appear",
		Items:       items,
	}}
	body, err := xml.Marshal(document)
	if err != nil {
		return nil, err
	}
	return append([]byte(xml.Header), body...), nil
}
