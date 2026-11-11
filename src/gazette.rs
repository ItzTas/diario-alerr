use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Certificate, Response, StatusCode};
use serde::{Deserialize, Deserializer};

use crate::config::go_quote;
use crate::domain::Edition;
use crate::pdf::download_pages;
use crate::text::{Hits, find_terms, normalize};
use crate::timestamp::{format_wordpress_time, parse_wordpress_time};

pub const USER_AGENT: &str = "diario-alerr/2.0";
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const PER_PAGE: u32 = 100;
const ENTITIES_GO_OMITS: [&str; 2] = ["nGt;", "nLt;"];
const LONGEST_ENTITY_WITHOUT_SEMICOLON: usize = 6;
const WINDOWS_1252: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub struct Client {
    http: reqwest::Client,
    api_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GazetteError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("GET {url}: {status}")]
    Status { url: String, status: StatusCode },
    #[error("decode media list: {0}")]
    Decode(serde_json::Error),
    #[error("edition {edition}: date_gmt {} is not a WordPress timestamp", go_quote(.raw))]
    Date { edition: i64, raw: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("pdftotext: {0}")]
    Pdftotext(String),
}

#[derive(Default, Deserialize)]
struct Media {
    #[serde(default, deserialize_with = "null_as_default")]
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    date_gmt: String,
    #[serde(default, deserialize_with = "null_as_default")]
    source_url: String,
    #[serde(default, deserialize_with = "null_as_default")]
    title: Rendered,
}

#[derive(Default, Deserialize)]
struct Rendered {
    #[serde(default, deserialize_with = "null_as_default")]
    rendered: String,
}

impl Client {
    pub fn new(api_url: String, timeout: Duration) -> Result<Client, GazetteError> {
        let roots = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .map(|root| Certificate::from_der(root))
            .collect::<Result<Vec<_>, _>>()?;
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(timeout)
            .gzip(true)
            .tls_certs_only(roots)
            .build()?;
        Ok(Client { http, api_url })
    }

    pub async fn list_editions(&self, after: DateTime<Utc>) -> Result<Vec<Edition>, GazetteError> {
        let mut editions = Vec::new();
        for page in 1..=u32::MAX {
            let url = format!("{}?{}", self.api_url, listing_query(after, page));
            let (batch, total_pages) = self.fetch_page(&url).await?;
            editions.extend(batch);
            if i64::from(page) >= total_pages {
                break;
            }
        }
        Ok(editions)
    }

    pub async fn scan(
        &self,
        edition: &Edition,
        terms: &[String],
    ) -> Result<Option<Hits>, GazetteError> {
        let pages = download_pages(&self.http, &edition.url).await?;
        let normalized: Vec<String> = pages.iter().map(|page| normalize(page)).collect();
        Ok(find_terms(&normalized, terms))
    }

    async fn fetch_page(&self, url: &str) -> Result<(Vec<Edition>, i64), GazetteError> {
        let response = get(&self.http, url).await?;
        let total_pages = total_pages(&response);
        let body = response.bytes().await?;
        let editions = decode_media(&body)
            .map_err(GazetteError::Decode)?
            .into_iter()
            .map(Media::edition)
            .collect::<Result<_, _>>()?;
        Ok((editions, total_pages))
    }
}

pub(crate) async fn get(http: &reqwest::Client, url: &str) -> Result<Response, GazetteError> {
    let response = http.get(url).send().await?;
    let status = response.status();
    if status != StatusCode::OK {
        return Err(GazetteError::Status {
            url: url.to_owned(),
            status,
        });
    }
    Ok(response)
}

fn total_pages(response: &Response) -> i64 {
    response
        .headers()
        .get("X-WP-TotalPages")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}

fn decode_media(body: &[u8]) -> Result<Vec<Media>, serde_json::Error> {
    let mut decoder = serde_json::Deserializer::from_slice(body);
    let items = Option::<Vec<Option<Media>>>::deserialize(&mut decoder)?;
    Ok(items
        .unwrap_or_default()
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect())
}

fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

impl Media {
    fn edition(self) -> Result<Edition, GazetteError> {
        let Some(published) = parse_wordpress_time(&self.date_gmt) else {
            return Err(GazetteError::Date {
                edition: self.id,
                raw: self.date_gmt,
            });
        };
        Ok(Edition {
            id: self.id,
            published,
            url: self.source_url,
            title: html_unescape(&self.title.rendered),
        })
    }
}

pub(crate) fn listing_query(after: DateTime<Utc>, page: u32) -> String {
    let values = [
        ("_fields", "id,date_gmt,source_url,title".to_owned()),
        ("after", format_wordpress_time(after)),
        ("mime_type", "application/pdf".to_owned()),
        ("order", "asc".to_owned()),
        ("orderby", "date".to_owned()),
        ("page", page.to_string()),
        ("per_page", PER_PAGE.to_string()),
    ];
    values
        .iter()
        .map(|(key, value)| format!("{}={}", query_escape(key), query_escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn query_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            escaped.push(char::from(byte));
            continue;
        }
        if byte == b' ' {
            escaped.push('+');
            continue;
        }
        escaped.push_str(&format!("%{byte:02X}"));
    }
    escaped
}

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
    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{GazetteError, decode_media, html_unescape, listing_query};
    use crate::golden::golden;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ListingCase {
        after: String,
        total_pages: u32,
        queries: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct UnescapeCase {
        input: String,
        output: String,
    }

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw).unwrap().to_utc()
    }

    #[test]
    fn listing_query_matches_go() {
        let cases: Vec<ListingCase> = golden!("listing_query.json");
        assert!(!cases.is_empty());
        for case in &cases {
            let after = utc(&case.after);
            let queries: Vec<String> = (1..=case.total_pages)
                .map(|page| listing_query(after, page))
                .collect();
            assert_eq!(queries, case.queries, "after {}", case.after);
        }
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

    #[test]
    fn media_decoding_follows_go() {
        let decode = |body: &str| decode_media(body.as_bytes()).map(|items| items.len());
        assert_eq!(decode("null").unwrap(), 0);
        assert_eq!(decode("[]  trailing data").unwrap(), 0);
        assert_eq!(decode(r#"[null, {"id": null, "title": null}]"#).unwrap(), 2);
        assert!(decode("").is_err());
        assert!(decode(r#"[{"id": "7"}]"#).is_err());

        let items = decode_media(
            br#"[{"id": 3, "date_gmt": "2026-11-06T20:23:18", "title": {"rendered": "ED &#8211; 3"}, "extra": 1}]"#,
        )
        .unwrap();
        let edition = items.into_iter().next().unwrap().edition().unwrap();
        assert_eq!(edition.id, 3);
        assert_eq!(edition.published, utc("2026-11-06T20:23:18Z"));
        assert_eq!(edition.url, "");
        assert_eq!(edition.title, "ED – 3");

        let missing = decode_media(b"[null]").unwrap().into_iter().next().unwrap();
        let error = missing.edition().unwrap_err();
        assert!(
            matches!(&error, GazetteError::Date { edition: 0, raw } if raw.is_empty()),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            r#"edition 0: date_gmt "" is not a WordPress timestamp"#
        );
    }
}
