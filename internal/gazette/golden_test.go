package gazette

import (
	"html"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core/coretest"
	"github.com/ItzTas/diario-alerr/internal/gazette/gazettetest"
)

func TestGoldenVectors(t *testing.T) {
	coretest.Golden(t, "normalize.json", normalizeVectors())
	coretest.Golden(t, "find_terms.json", findTermsVectors())
	coretest.Golden(t, "date_gmt.json", dateVectors())
	coretest.Golden(t, "html_unescape.json", unescapeVectors())
	coretest.Golden(t, "listing_query.json", listingVectors(t))
	coretest.GoldenBytes(t, "fake.pdf", gazettetest.PDF("Mesa Diretora", "Nomeacao de (TALES) \\ SABINI"))
}

type stringCase struct {
	Input  string `json:"input"`
	Output string `json:"output"`
}

type nullableCase struct {
	Input  string  `json:"input"`
	Output *string `json:"output"`
}

func mapStrings(inputs []string, transform func(string) string) []stringCase {
	cases := make([]stringCase, len(inputs))
	for i, input := range inputs {
		cases[i] = stringCase{Input: input, Output: transform(input)}
	}
	return cases
}

func normalizeVectors() []stringCase {
	return mapStrings([]string{
		"José da Silva", "  tales\n\tSABÍNI  ", "Conceição Ávila Müller", "0021805e",
		"Straße", "é", "Nº 12", "1º", "²", "Ⅻ", "ﬁscal", "Ｔａｌｅｓ",
		"a b", "a  b", "a\u00a0b", "a\u3000b", "a\u0085b", "a\u200bb", "a\tb",
		"\u0301", "\u0915\u093e", "a\u20dd", "ǅ", "ı", "İstanbul", "ŉ", "ᾳ", "", "   ",
	}, Normalize)
}

func unescapeVectors() []stringCase {
	return mapStrings([]string{
		"ED &#8211; 1", "&amp;", "&amp;amp;", "&ndash;", "&#x2013;", "&#X2013;", "&#8211", "&#0;", "&#128;",
		"&#xD800;", "&#1114112;", "&notit;", "&amp", "&ampx", "&Aacute;", "&aacute", "&nbsp;", "&#39;",
		"&unknown;", "&", "&;", "&#;", "&#x;", "DIARIO-ALE-RR-ED.-4763",
	}, html.UnescapeString)
}

func dateVectors() []nullableCase {
	inputs := []string{
		"2026-11-06T20:23:18", "2026-11-06T20:23:18.5", "2026-11-06T20:23:18.123456789", "2026-11-06T8:23:18",
		"2026-11-06T20:23:18Z", "2026-11-06 20:23:18", "2026-11-06", "2026-02-30T10:00:00",
		"2026-11-06T24:00:00", "2026-11-06T20:23:60", "yesterday", "",
	}
	cases := make([]nullableCase, len(inputs))
	for i, input := range inputs {
		cases[i] = nullableCase{Input: input}
		edition, err := media{ID: 1, DateGMT: input}.edition()
		if err != nil {
			continue
		}
		formatted := edition.Published.Format(time.RFC3339Nano)
		cases[i].Output = &formatted
	}
	return cases
}

type findTermsHits struct {
	Pages     map[string][]int `json:"pages"`
	Order     []string         `json:"order"`
	FirstPage int              `json:"first_page"`
	Snippet   string           `json:"snippet"`
}

type findTermsCase struct {
	RawPages []string       `json:"raw_pages"`
	Terms    []string       `json:"terms"`
	Hits     *findTermsHits `json:"hits"`
}

func findTermsVectors() []findTermsCase {
	long := func(filler string, count int) string { return strings.Repeat(filler, count) }
	inputs := []findTermsCase{
		{
			RawPages: []string{"Sumário Mesa Diretora", "Classificação: 0021805e TALES\nSABINI 132,61 8", "Nomeação de Tales Sabíni para o cargo"},
			Terms:    []string{"Tales Sabini", "0021805e", "Fulano"},
		},
		{RawPages: []string{"nada aqui"}, Terms: []string{"Tales Sabini"}},
		{
			RawPages: []string{long("X", 300) + " ALFA " + long("Y", 300) + " BETA " + long("Z", 300)},
			Terms:    []string{"beta", "alfa"},
		},
		{
			RawPages: []string{long("X", 300) + " ALFA " + long("Y", 300) + " BETA " + long("Z", 300)},
			Terms:    []string{"alfa", "beta"},
		},
		{
			RawPages: []string{"beta aqui " + long("Q", 200), "alfa aqui " + long("W", 200)},
			Terms:    []string{"alfa", "beta"},
		},
		{RawPages: []string{"um", "dois alfa", "tres alfa"}, Terms: []string{"alfa", "alfa"}},
		{RawPages: []string{"um", "dois"}, Terms: []string{"   ", "alfa"}},
		{RawPages: []string{"Nomeação de José"}, Terms: []string{"José", "Jose", "JOSÉ"}},
		{RawPages: []string{"Rua Straße 10", "STRASSE"}, Terms: []string{"straße", "STRASSE"}},
		{RawPages: []string{"Rua Straße 10"}, Terms: []string{"straße"}},
		{RawPages: []string{"STRASSE"}, Terms: []string{"straße"}},
		{RawPages: []string{"Tales Sabini " + long("a", 400)}, Terms: []string{"tales sabini"}},
		{RawPages: []string{long("a", 400) + " Tales Sabini"}, Terms: []string{"tales sabini"}},
		{RawPages: []string{long("a", 100) + " Tales Sabini " + long("b", 100)}, Terms: []string{"tales sabini"}},
		{RawPages: []string{long("º", 300) + " TALES SABINI " + long("–", 300)}, Terms: []string{"tales sabini"}},
		{RawPages: strings.Split("Capa\fNomeação de Tales Sabini\f", "\f"), Terms: []string{"Tales Sabini"}},
		{RawPages: strings.Split("Capa\f\fTales\f", "\f"), Terms: []string{"Tales", "Capa"}},
	}
	for i, input := range inputs {
		pages := make([]string, len(input.RawPages))
		for j, page := range input.RawPages {
			pages[j] = Normalize(page)
		}
		if hits := FindTerms(pages, input.Terms); hits != nil {
			inputs[i].Hits = &findTermsHits{Pages: hits.Pages, Order: hits.Order, FirstPage: hits.FirstPage, Snippet: hits.Snippet}
		}
	}
	return inputs
}

type listingCase struct {
	After      string   `json:"after"`
	TotalPages int      `json:"total_pages"`
	Queries    []string `json:"queries"`
}

func listingVectors(t *testing.T) []listingCase {
	inputs := []struct {
		after string
		pages int
	}{
		{"2026-10-31T00:00:00Z", 1},
		{"2026-11-01T10:00:00.999Z", 3},
		{"2026-11-01T10:00:00Z", 12},
	}
	cases := make([]listingCase, len(inputs))
	for i, input := range inputs {
		after, err := time.Parse(time.RFC3339Nano, input.after)
		if err != nil {
			t.Fatal(err)
		}
		var mu sync.Mutex
		var queries []string
		server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			mu.Lock()
			queries = append(queries, r.URL.RawQuery)
			mu.Unlock()
			w.Header().Set("X-WP-TotalPages", strconv.Itoa(input.pages))
			_, _ = w.Write([]byte("[]"))
		}))
		client := &Client{HTTP: server.Client(), APIURL: server.URL}
		_, err = client.ListEditions(t.Context(), after)
		server.Close()
		if err != nil {
			t.Fatal(err)
		}
		mu.Lock()
		cases[i] = listingCase{After: input.after, TotalPages: input.pages, Queries: queries}
		mu.Unlock()
	}
	return cases
}
