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
		"  tales\n\tSABÍNI  ":    "TALES SABINI",
		"Conceição Ávila Müller": "CONCEICAO AVILA MULLER",
		"0021805e":               "0021805E",
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
		Normalize("Classificação: 0021805e TALES\nSABINI 132,61 8"),
		Normalize("Nomeação de Tales Sabíni para o cargo"),
	}
	got := FindTerms(pages, []string{"Tales Sabini", "0021805e", "Fulano"})
	if got == nil {
		t.Fatal("FindTerms returned nil, want hits")
	}
	if want := map[string][]int{"Tales Sabini": {2, 3}, "0021805e": {2}}; !reflect.DeepEqual(got.Pages, want) {
		t.Errorf("pages = %v, want %v", got.Pages, want)
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
	if got := FindTerms([]string{Normalize("nada aqui")}, []string{"Tales Sabini"}); got != nil {
		t.Errorf("FindTerms = %+v, want nil", got)
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

func TestNormalizeIsSafeToCallConcurrently(t *testing.T) {
	var wg sync.WaitGroup
	for range 8 {
		wg.Go(func() {
			for range 200 {
				if got := Normalize("Nomeação de Tales Sabíni"); got != "NOMEACAO DE TALES SABINI" {
					t.Errorf("Normalize = %q", got)
					return
				}
			}
		})
	}
	wg.Wait()
}
