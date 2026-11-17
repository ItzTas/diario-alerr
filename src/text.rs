use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

pub fn normalize(text: &str) -> String {
    let stripped: String = text
        .nfkd()
        .filter(|c| c.general_category() != GeneralCategory::NonspacingMark)
        .nfc()
        .collect();
    let upper: String = stripped.chars().map(simple_uppercase).collect();
    upper.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn simple_uppercase(c: char) -> char {
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hits {
    pub pages: HashMap<String, Vec<usize>>,
    pub order: Vec<String>,
    pub first_page: usize,
    pub snippet: String,
}

pub fn find_terms(pages: &[String], terms: &[String]) -> Option<Hits> {
    let mut found = Hits {
        pages: HashMap::new(),
        order: Vec::new(),
        first_page: pages.len() + 1,
        snippet: String::new(),
    };
    let mut first_match = (0, 0);
    for term in terms {
        let needle = Needle::new(term);
        for (index, text) in pages.iter().enumerate() {
            let Some(location) = needle.locate(text) else {
                continue;
            };
            let number = index + 1;
            found.pages.entry(term.clone()).or_default().push(number);
            if number < found.first_page {
                found.first_page = number;
                first_match = location;
            }
        }
        if found.pages.contains_key(term) {
            found.order.push(term.clone());
        }
    }
    if found.order.is_empty() {
        return None;
    }
    found.snippet = snippet(&pages[found.first_page - 1], first_match);
    Some(found)
}

enum Needle {
    Name(String),
    Registration {
        significant: Vec<char>,
        skips_zeros: bool,
    },
}

impl Needle {
    fn new(term: &str) -> Self {
        let normalized = normalize(term);
        if !normalized.chars().any(char::is_numeric) {
            return Self::Name(normalized);
        }
        let compact: Vec<char> = normalized.chars().filter(|c| c.is_alphanumeric()).collect();
        let significant: Vec<char> = compact.iter().copied().skip_while(|&c| c == '0').collect();
        if significant.is_empty() {
            return Self::Registration {
                significant: compact,
                skips_zeros: false,
            };
        }
        Self::Registration {
            significant,
            skips_zeros: true,
        }
    }

    fn locate(&self, text: &str) -> Option<(usize, usize)> {
        match self {
            Self::Name(needle) => text
                .find(needle.as_str())
                .map(|offset| (text[..offset].chars().count(), needle.chars().count())),
            Self::Registration {
                significant,
                skips_zeros,
            } => locate_registration(text, significant, *skips_zeros),
        }
    }
}

fn locate_registration(
    text: &str,
    significant: &[char],
    skips_zeros: bool,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    (0..chars.len())
        .filter(|&start| start == 0 || !chars[start - 1].is_alphanumeric())
        .find_map(|start| {
            registration_end(&chars, start, significant, skips_zeros)
                .map(|end| (start, end - start))
        })
}

fn registration_end(
    chars: &[char],
    start: usize,
    significant: &[char],
    skips_zeros: bool,
) -> Option<usize> {
    let mut at = start;
    while skips_zeros && chars.get(at) == Some(&'0') {
        at = skip_separator(chars, at + 1);
    }
    for (position, &wanted) in significant.iter().enumerate() {
        if position > 0 {
            at = skip_separator(chars, at);
        }
        if chars.get(at) != Some(&wanted) {
            return None;
        }
        at += 1;
    }
    let ends_on_edge = chars.get(at).is_none_or(|c| !c.is_alphanumeric());
    ends_on_edge.then_some(at)
}

fn skip_separator(chars: &[char], at: usize) -> usize {
    let is_separator = chars.get(at).is_some_and(|&c| {
        matches!(c, '.' | '/' | '\\' | ' ')
            || c.general_category() == GeneralCategory::DashPunctuation
    });
    at + usize::from(is_separator)
}

const SNIPPET_RADIUS: usize = 160;
const MAX_SHOWN_CHARS: usize = 300;
const CUT_MARK: char = '…';

pub(crate) fn capped(text: &str) -> String {
    let Some((cut, _)) = text.char_indices().nth(MAX_SHOWN_CHARS) else {
        return text.to_owned();
    };
    format!("{}{CUT_MARK}", &text[..cut])
}

fn snippet(text: &str, (start, length): (usize, usize)) -> String {
    let from = start.saturating_sub(SNIPPET_RADIUS);
    let to = start + length + SNIPPET_RADIUS;
    text.chars().skip(from).take(to - from).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::Deserialize;

    use super::{Hits, Needle, SNIPPET_RADIUS, find_terms, normalize, snippet};
    use crate::golden::golden;

    fn normalized(pages: &[String]) -> Vec<String> {
        pages.iter().map(|page| normalize(page)).collect()
    }

    fn hits_for(text: &str, term: &str) -> Option<Hits> {
        find_terms(&[normalize(text)], &[term.to_owned()])
    }

    #[test]
    fn find_terms_matches_registration_in_any_formatting() {
        let matching = [
            "Classificação: 0034712k PAULO",
            "matrícula: 34712-K, CPF",
            "matrícula 34.712/K do cargo",
            "matrícula 034712 K do cargo",
            "matrícula 00.034.712–K do cargo",
            "(34712K)",
        ];
        for text in matching {
            for term in ["0034712k", "34.712-K"] {
                assert!(
                    hits_for(text, term).is_some(),
                    "{term:?} does not match {text:?}"
                );
            }
        }
    }

    #[test]
    fn find_terms_keeps_registration_whole() {
        let missing = [
            "matrícula 134712-K",
            "matrícula 347122-K",
            "matrícula 34712, CPF",
            "código 34712-KF",
            "valor de R$ 34.712,00",
        ];
        for text in missing {
            assert_eq!(hits_for(text, "0034712k"), None, "{text:?}");
        }
    }

    #[test]
    fn find_terms_snippet_centers_on_formatted_registration() {
        let hits = hits_for("Exonerar FULANO, matrícula: 34.712-K, do cargo", "0034712k");
        assert!(hits.is_some_and(|hits| hits.snippet.contains("34.712-K")));
    }

    #[test]
    fn snippet_keeps_chars_intact() {
        let text = format!("{} PAULO SANTOS {}", "º".repeat(300), "–".repeat(300));
        let location = Needle::new("PAULO SANTOS").locate(&text);
        let got = snippet(&text, location.unwrap_or_default());
        assert!(got.contains("PAULO SANTOS"));
        assert!(got.starts_with('º'));
        assert!(got.ends_with('–'));
        assert_eq!(
            got.chars().count(),
            2 * SNIPPET_RADIUS + "PAULO SANTOS".len()
        );
    }

    #[test]
    fn normalize_applies_go_simple_case_mapping() {
        let cases = [
            ("ß", "ß"),
            ("ŉ", "ʼN"),
            ("ǰ", "J"),
            ("ﬁscal", "FISCAL"),
            ("ᾳ", "Α"),
            ("ǅ", "DZ"),
            ("ı", "I"),
            ("İstanbul", "ISTANBUL"),
        ];
        for (input, want) in cases {
            assert_eq!(normalize(input), want, "{input:?}");
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NormalizeCase {
        input: String,
        output: String,
    }

    #[test]
    fn normalize_matches_go() {
        let cases: Vec<NormalizeCase> = golden!("normalize.json");
        assert!(!cases.is_empty());
        for case in &cases {
            assert_eq!(normalize(&case.input), case.output, "{:?}", case.input);
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct FindTermsCase {
        raw_pages: Vec<String>,
        terms: Vec<String>,
        hits: Option<HitsCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HitsCase {
        pages: HashMap<String, Vec<usize>>,
        order: Vec<String>,
        first_page: usize,
        snippet: String,
    }

    #[test]
    fn find_terms_matches_go() {
        let cases: Vec<FindTermsCase> = golden!("find_terms.json");
        assert!(!cases.is_empty());
        for case in &cases {
            let want = case.hits.as_ref().map(|hits| Hits {
                pages: hits.pages.clone(),
                order: hits.order.clone(),
                first_page: hits.first_page,
                snippet: hits.snippet.clone(),
            });
            let got = find_terms(&normalized(&case.raw_pages), &case.terms);
            assert_eq!(got, want, "{:?} in {:?}", case.terms, case.raw_pages);
        }
    }
}
