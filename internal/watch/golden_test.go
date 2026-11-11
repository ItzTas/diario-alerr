package watch

import (
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/core/coretest"
	"github.com/ItzTas/diario-alerr/internal/gazette"
)

func TestGoldenVectors(t *testing.T) {
	coretest.Golden(t, "alerts.json", alertVectors(t))
}

type alertJSON struct {
	ID        string `json:"id"`
	Title     string `json:"title"`
	Link      string `json:"link"`
	Body      string `json:"body"`
	Published string `json:"published"`
}

type welcomeCase struct {
	Terms           []string  `json:"terms"`
	IntervalMinutes string    `json:"interval_minutes"`
	Now             string    `json:"now"`
	Alert           alertJSON `json:"alert"`
}

type editionJSON struct {
	ID        int64  `json:"id"`
	Published string `json:"published"`
	URL       string `json:"url"`
	Title     string `json:"title"`
}

type editionCase struct {
	Edition  editionJSON `json:"edition"`
	RawPages []string    `json:"raw_pages"`
	Terms    []string    `json:"terms"`
	Alert    *alertJSON  `json:"alert"`
}

type alertVectorSet struct {
	Welcome []welcomeCase `json:"welcome"`
	Edition []editionCase `json:"edition"`
}

func toAlertJSON(a core.Alert) alertJSON {
	return alertJSON{ID: a.ID, Title: a.Title, Link: a.Link, Body: a.Body, Published: a.Published.UTC().Format(time.RFC3339Nano)}
}

func alertVectors(t *testing.T) alertVectorSet {
	var set alertVectorSet
	now := time.Date(2026, 11, 9, 14, 0, 0, 0, time.UTC)
	terms := []string{"PAULO SANTOS", "0034712k"}
	for _, minutes := range []string{"30", "0.5", "1.5", "2.5", "0.25", "7.5", "90", "1440"} {
		t.Setenv("DATABASE_URL", "x")
		t.Setenv("WATCH_TERMS", "x")
		t.Setenv("CHECK_INTERVAL_MINUTES", minutes)
		t.Setenv("START_AFTER", "")
		cfg, err := core.FromEnvironment(time.Now)
		if err != nil {
			t.Fatal(err)
		}
		set.Welcome = append(set.Welcome, welcomeCase{
			Terms:           terms,
			IntervalMinutes: minutes,
			Now:             now.Format(time.RFC3339Nano),
			Alert:           toAlertJSON(WelcomeAlert(terms, cfg.Interval, now)),
		})
	}

	published := time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC)
	cases := []struct {
		edition gazette.Edition
		pages   []string
		terms   []string
	}{
		{
			gazette.Edition{ID: 42, Published: published, URL: "https://x/ed.pdf", Title: "DIARIO-ALE-RR-ED.-4763"},
			[]string{"Sumário Mesa Diretora", "Classificação: 0034712k PAULO\nSANTOS 132,61 8", "Nomeação de Paulo Santós para o cargo"},
			[]string{"Paulo Santos", "0034712k", "Fulano"},
		},
		{
			gazette.Edition{ID: 7, Published: published.Add(500 * time.Millisecond), URL: "https://x/7.pdf", Title: "DIARIO-ALE-RR-ED.-4700"},
			[]string{"nada aqui", "Nomeação de Paulo Santós", "fim"},
			[]string{"paulo santos"},
		},
		{
			gazette.Edition{ID: 8, Published: published, URL: "https://x/8.pdf?ver=2", Title: "DIARIO-ALE-RR-ED.-4701"},
			[]string{"PAULO SANTOS", "x", "paulo santos e 0034712K", "0034712k"},
			[]string{"Paulo Santos", "0034712k"},
		},
		{
			gazette.Edition{ID: 9, Published: published, URL: "https://x/9.pdf", Title: "Edição – Extra"},
			[]string{"Portaria de José da Conceição"},
			[]string{"josé da conceição"},
		},
		{
			gazette.Edition{ID: 10, Published: published, URL: "https://x/10.pdf", Title: "DIARIO-ALE-RR-ED.-4702"},
			[]string{"nada", "aqui"},
			[]string{"Paulo Santos", "0034712k"},
		},
	}
	for _, c := range cases {
		normalized := make([]string, len(c.pages))
		for i, page := range c.pages {
			normalized[i] = gazette.Normalize(page)
		}
		result := editionCase{
			Edition:  editionJSON{ID: c.edition.ID, Published: c.edition.Published.Format(time.RFC3339Nano), URL: c.edition.URL, Title: c.edition.Title},
			RawPages: c.pages,
			Terms:    c.terms,
		}
		if hits := gazette.FindTerms(normalized, c.terms); hits != nil {
			alert := toAlertJSON(EditionAlert(c.edition, hits))
			result.Alert = &alert
		}
		set.Edition = append(set.Edition, result)
	}
	return set
}
