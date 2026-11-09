package gazette

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/runes"
	"golang.org/x/text/transform"
	"golang.org/x/text/unicode/norm"
)

const snippetRadius = 160

type Hits struct {
	Pages     map[string][]int
	Order     []string
	FirstPage int
	Snippet   string
}

func Normalize(text string) string {
	stripMarks := transform.Chain(norm.NFKD, runes.Remove(runes.In(unicode.Mn)), norm.NFC)
	stripped, _, err := transform.String(stripMarks, text)
	if err != nil {
		stripped = text
	}
	return strings.Join(strings.Fields(strings.ToUpper(stripped)), " ")
}

func FindTerms(pages []string, terms []string) *Hits {
	found := Hits{Pages: map[string][]int{}, FirstPage: len(pages) + 1}
	firstNeedle := ""
	for _, term := range terms {
		needle := Normalize(term)
		for number, text := range pages {
			if !strings.Contains(text, needle) {
				continue
			}
			found.Pages[term] = append(found.Pages[term], number+1)
			if number+1 < found.FirstPage {
				found.FirstPage, firstNeedle = number+1, needle
			}
		}
		if len(found.Pages[term]) > 0 {
			found.Order = append(found.Order, term)
		}
	}
	if len(found.Order) == 0 {
		return nil
	}
	found.Snippet = snippet(pages[found.FirstPage-1], firstNeedle)
	return &found
}

func snippet(text, needle string) string {
	start := utf8.RuneCountInString(text[:strings.Index(text, needle)])
	chars := []rune(text)
	from := max(0, start-snippetRadius)
	to := min(len(chars), start+utf8.RuneCountInString(needle)+snippetRadius)
	return string(chars[from:to])
}
