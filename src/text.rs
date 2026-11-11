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
    let mut first_needle = String::new();
    for term in terms {
        let needle = normalize(term);
        for (index, text) in pages.iter().enumerate() {
            if !text.contains(&needle) {
                continue;
            }
            let number = index + 1;
            found.pages.entry(term.clone()).or_default().push(number);
            if number < found.first_page {
                found.first_page = number;
                first_needle.clone_from(&needle);
            }
        }
        if found.pages.contains_key(term) {
            found.order.push(term.clone());
        }
    }
    if found.order.is_empty() {
        return None;
    }
    found.snippet = snippet(&pages[found.first_page - 1], &first_needle);
    Some(found)
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

fn snippet(text: &str, needle: &str) -> String {
    let start = text
        .find(needle)
        .map_or(0, |offset| text[..offset].chars().count());
    let from = start.saturating_sub(SNIPPET_RADIUS);
    let to = start + needle.chars().count() + SNIPPET_RADIUS;
    text.chars().skip(from).take(to - from).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde::Deserialize;

    use super::{Hits, SNIPPET_RADIUS, find_terms, normalize, snippet};
    use crate::golden::golden;

    fn normalized(pages: &[String]) -> Vec<String> {
        pages.iter().map(|page| normalize(page)).collect()
    }

    #[test]
    fn snippet_keeps_chars_intact() {
        let text = format!("{} TALES SABINI {}", "º".repeat(300), "–".repeat(300));
        let got = snippet(&text, "TALES SABINI");
        assert!(got.contains("TALES SABINI"));
        assert!(got.starts_with('º'));
        assert!(got.ends_with('–'));
        assert_eq!(
            got.chars().count(),
            2 * SNIPPET_RADIUS + "TALES SABINI".len()
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
