package watch

import (
	"reflect"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/gazette"
)

func TestEditionAlertDescribesPagesAndLinksTheFirstOne(t *testing.T) {
	e := gazette.Edition{ID: 42, Published: time.Date(2026, 11, 6, 20, 23, 18, 0, time.UTC), URL: "https://x/ed.pdf", Title: "DIARIO-ALE-RR-ED.-4763"}
	h := &gazette.Hits{Pages: map[string][]int{"PAULO SANTOS": {3, 5}, "0034712k": {3}}, Order: []string{"PAULO SANTOS", "0034712k"}, FirstPage: 3, Snippet: "SNIPPET"}
	want := core.Alert{
		ID:        "edition-42",
		Title:     "Your name appeared in the ALE/RR Official Gazette: DIARIO-ALE-RR-ED.-4763",
		Link:      "https://x/ed.pdf#page=3",
		Body:      "PAULO SANTOS: pp. 3, 5\n0034712k: p. 3\n\n…SNIPPET…",
		Published: e.Published,
	}
	if got := EditionAlert(e, h); !reflect.DeepEqual(got, want) {
		t.Errorf("alert = %+v\nwant    %+v", got, want)
	}
}

func TestWelcomeAlertListsTermsAndInterval(t *testing.T) {
	now := time.Date(2026, 11, 9, 14, 0, 0, 0, time.UTC)
	want := core.Alert{
		ID:        core.WelcomeAlertID,
		Title:     "ALE/RR Official Gazette monitor started",
		Link:      core.SiteURL,
		Body:      "Watching: PAULO SANTOS, 0034712k\nChecking every 30 min.",
		Published: now,
	}
	if got := WelcomeAlert([]string{"PAULO SANTOS", "0034712k"}, 30*time.Minute, now); !reflect.DeepEqual(got, want) {
		t.Errorf("alert = %+v\nwant    %+v", got, want)
	}
}
