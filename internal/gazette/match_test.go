package gazette

import (
	"reflect"
	"strings"
	"sync"
	"testing"
)

func TestNormalizeUppercasesStripsAccentsAndCollapsesSpaces(t *testing.T) {
	cases := map[string]string{
		"José da Silva":          "JOSE DA SILVA",
		"  paulo\n\tSANTÓS  ":    "PAULO SANTOS",
		"Conceição Ávila Müller": "CONCEICAO AVILA MULLER",
		"0034712k":               "0034712K",
	}
	for input, want := range cases {
		if got := Normalize(input); got != want {
			t.Errorf("Normalize(%q) = %q, want %q", input, got, want)
		}
	}
}

func TestFindTermsListsEveryPageOfEveryTerm(t *testing.T) {
	pages := []string{
		Normalize("Sumário Mesa Diretora"),
		Normalize("Classificação: 0034712k PAULO\nSANTOS 132,61 8"),
		Normalize("Nomeação de Paulo Santós para o cargo"),
	}
	got := FindTerms(pages, []string{"Paulo Santos", "0034712k", "Fulano"})
	if got == nil {
		t.Fatal("FindTerms returned nil, want hits")
	}
	if want := map[string][]int{"Paulo Santos": {2, 3}, "0034712k": {2}}; !reflect.DeepEqual(got.Pages, want) {
		t.Errorf("pages = %v, want %v", got.Pages, want)
	}
	if !reflect.DeepEqual(got.Order, []string{"Paulo Santos", "0034712k"}) {
		t.Errorf("order = %v", got.Order)
	}
	if got.FirstPage != 2 {
		t.Errorf("first page = %d, want 2", got.FirstPage)
	}
	if !strings.Contains(got.Snippet, "PAULO SANTOS") {
		t.Errorf("snippet %q does not contain the match", got.Snippet)
	}
}

func TestFindTermsWithoutMatchReturnsNil(t *testing.T) {
	if got := FindTerms([]string{Normalize("nada aqui")}, []string{"Paulo Santos"}); got != nil {
		t.Errorf("FindTerms = %+v, want nil", got)
	}
}

func TestSnippetKeepsRunesIntact(t *testing.T) {
	text := strings.Repeat("º", 300) + " PAULO SANTOS " + strings.Repeat("–", 300)
	got := snippet(text, "PAULO SANTOS")
	if !strings.Contains(got, "PAULO SANTOS") || !strings.HasPrefix(got, "º") || !strings.HasSuffix(got, "–") {
		t.Errorf("snippet = %q", got)
	}
	if want := snippetRadius*2 + len("PAULO SANTOS"); len([]rune(got)) != want {
		t.Errorf("snippet has %d runes, want %d", len([]rune(got)), want)
	}
}

func TestNormalizeIsSafeToCallConcurrently(t *testing.T) {
	var wg sync.WaitGroup
	for range 8 {
		wg.Go(func() {
			for range 200 {
				if got := Normalize("Nomeação de Paulo Santós"); got != "NOMEACAO DE PAULO SANTOS" {
					t.Errorf("Normalize = %q", got)
					return
				}
			}
		})
	}
	wg.Wait()
}
