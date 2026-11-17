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

func TestFindTermsMatchesRegistrationInAnyFormatting(t *testing.T) {
	matching := []string{
		"Classificação: 0034712k PAULO",
		"matrícula: 34712-K, CPF",
		"matrícula 34.712/K do cargo",
		"matrícula 034712 K do cargo",
		"matrícula 00.034.712–K do cargo",
		"(34712K)",
	}
	for _, text := range matching {
		for _, term := range []string{"0034712k", "34.712-K"} {
			if FindTerms([]string{Normalize(text)}, []string{term}) == nil {
				t.Errorf("term %q does not match %q", term, text)
			}
		}
	}
}

func TestFindTermsMatchesNamesWithMiddleNames(t *testing.T) {
	matching := []string{
		"Nomear PAULO SANTOS para o cargo",
		"Nomear PAULO HENRIQUE SANTOS para o cargo",
		"Exonerar Paulo Henrique de Oliveira Santos, matrícula",
		"servidora PAULO DAS GRAÇAS DE SOUZA-LIMA SANTOS",
		"PAULO D'ÁVILA\nSANTOS",
	}
	for _, text := range matching {
		if FindTerms([]string{Normalize(text)}, []string{"Paulo Santos"}) == nil {
			t.Errorf("Paulo Santos does not match %q", text)
		}
	}
}

func TestFindTermsKeepsNameWordsInOrderAndClose(t *testing.T) {
	missing := []string{
		"SANTOS PAULO",
		"PAULO SILVA 132,61 MARIA SANTOS",
		"PAULO SILVA, MARIA SANTOS",
		"PAULO A B C D E SANTOS",
	}
	for _, text := range missing {
		if got := FindTerms([]string{Normalize(text)}, []string{"Paulo Santos"}); got != nil {
			t.Errorf("Paulo Santos matches %q", text)
		}
	}
}

func TestFindTermsSnippetSpansTheMiddleNames(t *testing.T) {
	got := FindTerms([]string{Normalize("Exonerar PAULO HENRIQUE SANTOS do cargo")}, []string{"Paulo Santos"})
	if got == nil || !strings.Contains(got.Snippet, "PAULO HENRIQUE SANTOS") {
		t.Errorf("hits = %+v, want a snippet with PAULO HENRIQUE SANTOS", got)
	}
}

func TestFindTermsKeepsRegistrationWhole(t *testing.T) {
	missing := []string{
		"matrícula 134712-K",
		"matrícula 347122-K",
		"matrícula 34712, CPF",
		"código 34712-KF",
		"valor de R$ 34.712,00",
	}
	for _, text := range missing {
		if got := FindTerms([]string{Normalize(text)}, []string{"0034712k"}); got != nil {
			t.Errorf("0034712k matches %q", text)
		}
	}
}

func TestFindTermsSnippetCentersOnFormattedRegistration(t *testing.T) {
	got := FindTerms([]string{Normalize("Exonerar FULANO, matrícula: 34.712-K, do cargo")}, []string{"0034712k"})
	if got == nil || !strings.Contains(got.Snippet, "34.712-K") {
		t.Errorf("hits = %+v, want a snippet with 34.712-K", got)
	}
}

func TestFindTermsWithoutMatchReturnsNil(t *testing.T) {
	if got := FindTerms([]string{Normalize("nada aqui")}, []string{"Paulo Santos"}); got != nil {
		t.Errorf("FindTerms = %+v, want nil", got)
	}
}

func TestSnippetKeepsRunesIntact(t *testing.T) {
	text := strings.Repeat("º", 300) + " PAULO SANTOS " + strings.Repeat("–", 300)
	got := snippet(text, termPattern("PAULO SANTOS"))
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
