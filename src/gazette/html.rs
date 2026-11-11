const ENTITIES_GO_OMITS: [&str; 2] = ["nGt;", "nLt;"];
const LONGEST_ENTITY_WITHOUT_SEMICOLON: usize = 6;
const WINDOWS_1252: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub(crate) fn html_unescape(text: &str) -> String {
    let Some(first) = text.find('&') else {
        return text.to_owned();
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..first]);
    let mut rest = &text[first..];
    while !rest.is_empty() {
        let consumed = push_reference(&mut out, rest);
        rest = &rest[consumed..];
        let literal = rest.find('&').unwrap_or(rest.len());
        out.push_str(&rest[..literal]);
        rest = &rest[literal..];
    }
    out
}

fn push_reference(out: &mut String, text: &str) -> usize {
    let bytes = text.as_bytes();
    if bytes.len() <= 1 {
        out.push('&');
        return 1;
    }
    if bytes[1] == b'#' {
        return push_numeric_reference(out, bytes);
    }
    push_named_reference(out, text)
}

fn push_numeric_reference(out: &mut String, bytes: &[u8]) -> usize {
    if bytes.len() <= 3 {
        out.push('&');
        return 1;
    }
    let hex = matches!(bytes[2], b'x' | b'X');
    let radix: u32 = if hex { 16 } else { 10 };
    let mut index = if hex { 3 } else { 2 };
    let mut value: i32 = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        index += 1;
        if let Some(digit) = char::from(byte).to_digit(radix) {
            value = value.wrapping_mul(radix as i32).wrapping_add(digit as i32);
            continue;
        }
        if byte != b';' {
            index -= 1;
        }
        break;
    }
    if index <= 3 {
        out.push('&');
        return 1;
    }
    out.push(numeric_character(value));
    index
}

fn numeric_character(value: i32) -> char {
    if (0x80..=0x9F).contains(&value) {
        return WINDOWS_1252[(value - 0x80) as usize];
    }
    if value == 0 {
        return char::REPLACEMENT_CHARACTER;
    }
    u32::try_from(value)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or(char::REPLACEMENT_CHARACTER)
}

fn push_named_reference(out: &mut String, text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut end = 1;
    while end < bytes.len() {
        let byte = bytes[end];
        end += 1;
        if byte.is_ascii_alphanumeric() {
            continue;
        }
        if byte != b';' {
            end -= 1;
        }
        break;
    }
    let name = &text[1..end];
    if let Some(expansion) = entity(name) {
        out.push_str(expansion);
        return end;
    }
    let longest_prefix = name
        .len()
        .saturating_sub(1)
        .min(LONGEST_ENTITY_WITHOUT_SEMICOLON);
    for length in (2..=longest_prefix).rev() {
        if let Some(expansion) = entity(&name[..length]) {
            out.push_str(expansion);
            return length + 1;
        }
    }
    out.push_str(&text[..end]);
    end
}

fn entity(name: &str) -> Option<&'static str> {
    if ENTITIES_GO_OMITS.contains(&name) {
        return None;
    }
    let expansion = htmlize::ENTITIES.get(format!("&{name}").as_bytes())?;
    std::str::from_utf8(expansion).ok()
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::html_unescape;
    use crate::golden::golden;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UnescapeCase {
        input: String,
        output: String,
    }

    #[test]
    fn html_unescape_matches_go() {
        let cases: Vec<UnescapeCase> = golden!("html_unescape.json");
        assert!(!cases.is_empty());
        for case in &cases {
            assert_eq!(html_unescape(&case.input), case.output, "{:?}", case.input);
        }
    }

    #[test]
    fn html_unescape_keeps_go_quirks() {
        let cases = [
            ("&#x;", "\u{FFFD}"),
            ("&#X;", "\u{FFFD}"),
            ("&#4294967361;", "A"),
            ("&#x100000041;", "A"),
            ("&#2147483648;", "\u{FFFD}"),
            ("&nGt;", "&nGt;"),
            ("&nLt;", "&nLt;"),
            ("&nGt", "&nGt"),
            ("&nGtx;", "&nGtx;"),
            ("&nLt", "&nLt"),
            ("&nLtx;", "&nLtx;"),
            ("&nGtv;", "\u{226B}\u{338}"),
            ("&NotEqualTilde;", "\u{2242}\u{338}"),
            ("&CounterClockwiseContourIntegral;", "\u{2233}"),
            ("&eacute", "é"),
            ("&frac12x", "½x"),
            ("a&amp;b&lt", "a&b<"),
        ];
        for (input, output) in cases {
            assert_eq!(html_unescape(input), output, "{input:?}");
        }
    }

    #[test]
    fn entity_table_is_the_one_go_uses() {
        let accepted = htmlize::ENTITIES
            .keys()
            .map(|key| std::str::from_utf8(key).unwrap())
            .filter(|key| html_unescape(key) != *key)
            .count();
        assert_eq!(htmlize::ENTITIES.len(), 2231);
        assert_eq!(accepted, 2229);
        let longest = htmlize::ENTITIES.keys().map(|key| key.len()).max();
        assert_eq!(longest, Some("&CounterClockwiseContourIntegral;".len()));
        let longest_bare = htmlize::ENTITIES
            .keys()
            .filter(|key| !key.ends_with(b";"))
            .map(|key| key.len() - 1)
            .max();
        assert_eq!(longest_bare, Some(6));
    }
}
