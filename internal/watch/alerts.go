package watch

import (
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/gazette"
)

func WelcomeAlert(terms []string, interval time.Duration, now time.Time) core.Alert {
	return core.Alert{
		ID:        core.WelcomeAlertID,
		Title:     core.GazetteName + " monitor started",
		Link:      core.SiteURL,
		Body:      fmt.Sprintf("Watching: %s\nChecking every %.0f min.", strings.Join(terms, ", "), interval.Minutes()),
		Published: now.UTC(),
	}
}

func EditionAlert(e gazette.Edition, h *gazette.Hits) core.Alert {
	lines := make([]string, 0, len(h.Order))
	for _, term := range h.Order {
		lines = append(lines, term+": "+formatPages(h.Pages[term]))
	}
	return core.Alert{
		ID:        "edition-" + strconv.FormatInt(e.ID, 10),
		Title:     "Your name appeared in the " + core.GazetteName + ": " + e.Title,
		Link:      e.URL + "#page=" + strconv.Itoa(h.FirstPage),
		Body:      strings.Join(lines, "\n") + "\n\n…" + h.Snippet + "…",
		Published: e.Published,
	}
}

func formatPages(numbers []int) string {
	label := "pp."
	if len(numbers) == 1 {
		label = "p."
	}
	parts := make([]string, len(numbers))
	for i, n := range numbers {
		parts[i] = strconv.Itoa(n)
	}
	return label + " " + strings.Join(parts, ", ")
}
