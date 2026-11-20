use std::collections::HashMap;
use std::fmt;

use regex::Regex;
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
        let Ok(pattern) = term_pattern(term) else {
            continue;
        };
        for (index, text) in pages.iter().enumerate() {
            let Some(location) = locate(&pattern, text) else {
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

const NUMBER_SEPARATOR: &str = r"[\p{Pd}./\\ ]?";
const NUMBER_EDGE: &str = r"[^\p{L}\p{N}]";
const MIDDLE_NAMES: &str = r"(?: \p{L}[\p{L}\p{Pd}'’]*){0,4} ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TooLong {
    Words(usize),
    Characters(usize),
}

impl fmt::Display for TooLong {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TooLong::Words(count) => write!(f, "{count} words"),
            TooLong::Characters(count) => write!(f, "{count} characters"),
        }
    }
}

pub(crate) fn term_pattern(term: &str) -> Result<Regex, TooLong> {
    let needle = normalize(term);
    if needle.chars().any(is_digit) {
        return Regex::new(&registration_pattern(&needle))
            .map_err(|_| TooLong::Characters(term.chars().count()));
    }
    let words: Vec<String> = needle.split(' ').map(regex::escape).collect();
    Regex::new(&format!("({})", words.join(MIDDLE_NAMES))).map_err(|_| TooLong::Words(words.len()))
}

fn is_digit(c: char) -> bool {
    c.general_category() == GeneralCategory::DecimalNumber
}

fn is_letter_or_digit(c: char) -> bool {
    let letters = [
        GeneralCategory::UppercaseLetter,
        GeneralCategory::LowercaseLetter,
        GeneralCategory::TitlecaseLetter,
        GeneralCategory::ModifierLetter,
        GeneralCategory::OtherLetter,
    ];
    is_digit(c) || letters.contains(&c.general_category())
}

fn registration_pattern(needle: &str) -> String {
    let compact: String = needle.chars().filter(|&c| is_letter_or_digit(c)).collect();
    let trimmed = compact.trim_start_matches('0');
    let significant = if trimmed.is_empty() {
        compact.as_str()
    } else {
        trimmed
    };
    let core = significant
        .chars()
        .map(|c| regex::escape(&c.to_string()))
        .collect::<Vec<_>>()
        .join(NUMBER_SEPARATOR);
    format!("(?:^|{NUMBER_EDGE})((?:0{NUMBER_SEPARATOR})*{core})(?:{NUMBER_EDGE}|$)")
}

fn locate(pattern: &Regex, text: &str) -> Option<(usize, usize)> {
    let found = pattern.captures(text)?.get(1)?;
    let start = text[..found.start()].chars().count();
    Some((start, found.as_str().chars().count()))
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

    use super::{
        Hits, SNIPPET_RADIUS, TooLong, find_terms, locate, normalize, snippet, term_pattern,
    };
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
    fn find_terms_matches_names_with_middle_names() {
        let matching = [
            "Nomear PAULO SANTOS para o cargo",
            "Nomear PAULO HENRIQUE SANTOS para o cargo",
            "Exonerar Paulo Henrique de Oliveira Santos, matrícula",
            "servidora PAULO DAS GRAÇAS DE SOUZA-LIMA SANTOS",
            "PAULO D'ÁVILA\nSANTOS",
        ];
        for text in matching {
            assert!(hits_for(text, "Paulo Santos").is_some(), "{text:?}");
        }
    }

    #[test]
    fn find_terms_keeps_name_words_in_order_and_close() {
        let missing = [
            "SANTOS PAULO",
            "PAULO SILVA 132,61 MARIA SANTOS",
            "PAULO SILVA, MARIA SANTOS",
            "PAULO A B C D E SANTOS",
        ];
        for text in missing {
            assert_eq!(hits_for(text, "Paulo Santos"), None, "{text:?}");
        }
    }

    #[test]
    fn find_terms_snippet_spans_the_middle_names() {
        let hits = hits_for("Exonerar PAULO HENRIQUE SANTOS do cargo", "Paulo Santos");
        assert!(hits.is_some_and(|hits| hits.snippet.contains("PAULO HENRIQUE SANTOS")));
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
    fn find_terms_reads_letters_and_digits_by_general_category() {
        let matching = [
            ("34712K\u{1f150}", "MAT 34712K"),
            ("34712K\u{903}", "MAT 34712K"),
            ("34712K\u{f2a}", "MAT 34712K"),
            ("PAULO \u{2180}", "PAULO X \u{2180}"),
        ];
        for (term, text) in matching {
            assert!(hits_for(text, term).is_some(), "{term:?} in {text:?}");
        }
        let missing = [
            ("34712K\u{f2a}", "MAT 34712K\u{f2a}"),
            ("PAULO \u{2180}", "PAULO-\u{2180}"),
        ];
        for (term, text) in missing {
            assert_eq!(hits_for(text, term), None, "{term:?} in {text:?}");
        }
    }

    #[test]
    fn find_terms_matches_the_longest_name_it_accepts() {
        let name = vec!["PAULO"; 31].join(" ");
        assert!(hits_for(&name, &name).is_some());
    }

    #[test]
    fn find_terms_passes_over_a_term_too_long_to_match() {
        let name = vec!["PAULO"; 32].join(" ");
        let hits = find_terms(
            std::slice::from_ref(&name),
            &[name.clone(), "PAULO".to_owned()],
        );
        assert_eq!(hits.map(|hits| hits.order), Some(vec!["PAULO".to_owned()]));
    }

    #[test]
    fn term_pattern_reports_the_size_of_a_term_too_long_to_match() {
        let name = vec!["PAULO"; 32].join(" ");
        let registration = "0034712k".repeat(1000);
        assert_eq!(term_pattern(&name).err(), Some(TooLong::Words(32)));
        assert_eq!(
            term_pattern(&registration).err(),
            Some(TooLong::Characters(8000))
        );
    }

    #[test]
    fn find_terms_snippet_centers_on_formatted_registration() {
        let hits = hits_for("Exonerar FULANO, matrícula: 34.712-K, do cargo", "0034712k");
        assert!(hits.is_some_and(|hits| hits.snippet.contains("34.712-K")));
    }

    #[test]
    fn snippet_keeps_chars_intact() {
        let text = format!("{} PAULO SANTOS {}", "º".repeat(300), "–".repeat(300));
        let location = locate(&term_pattern("PAULO SANTOS").unwrap(), &text);
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
