package gazette

import (
	"regexp"
	"strings"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/runes"
	"golang.org/x/text/transform"
	"golang.org/x/text/unicode/norm"
)

const (
	snippetRadius   = 160
	numberSeparator = `[\p{Pd}./\\ ]?`
	numberEdge      = `[^\p{L}\p{N}]`
)

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
	var firstPattern *regexp.Regexp
	for _, term := range terms {
		pattern := termPattern(term)
		for number, text := range pages {
			if !pattern.MatchString(text) {
				continue
			}
			found.Pages[term] = append(found.Pages[term], number+1)
			if number+1 < found.FirstPage {
				found.FirstPage, firstPattern = number+1, pattern
			}
		}
		if len(found.Pages[term]) > 0 {
			found.Order = append(found.Order, term)
		}
	}
	if len(found.Order) == 0 {
		return nil
	}
	found.Snippet = snippet(pages[found.FirstPage-1], firstPattern)
	return &found
}

func termPattern(term string) *regexp.Regexp {
	needle := Normalize(term)
	if strings.ContainsFunc(needle, unicode.IsDigit) {
		return registrationPattern(needle)
	}
	return regexp.MustCompile("(" + regexp.QuoteMeta(needle) + ")")
}

func registrationPattern(needle string) *regexp.Regexp {
	compact := strings.Map(keepAlphanumeric, needle)
	significant := strings.TrimLeft(compact, "0")
	if significant == "" {
		significant = compact
	}
	chars := make([]string, 0, len(significant))
	for _, r := range significant {
		chars = append(chars, regexp.QuoteMeta(string(r)))
	}
	core := "(?:0" + numberSeparator + ")*" + strings.Join(chars, numberSeparator)
	return regexp.MustCompile("(?:^|" + numberEdge + ")(" + core + ")(?:" + numberEdge + "|$)")
}

func keepAlphanumeric(r rune) rune {
	if unicode.IsLetter(r) || unicode.IsDigit(r) {
		return r
	}
	return -1
}

func snippet(text string, pattern *regexp.Regexp) string {
	match := pattern.FindStringSubmatchIndex(text)
	start := utf8.RuneCountInString(text[:match[2]])
	length := utf8.RuneCountInString(text[match[2]:match[3]])
	chars := []rune(text)
	from := max(0, start-snippetRadius)
	to := min(len(chars), start+length+snippetRadius)
	return string(chars[from:to])
}
