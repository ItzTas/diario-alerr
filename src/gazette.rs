use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Certificate, Response, StatusCode};
use serde::{Deserialize, Deserializer};

use crate::config::go_quote;
use crate::domain::Edition;
use crate::pdf::download_pages;
use crate::text::{Hits, find_terms, normalize};
use crate::timestamp::{format_wordpress_time, parse_wordpress_time};

mod html;

pub(crate) use html::html_unescape;

pub const USER_AGENT: &str = "diario-alerr/2.0";
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const PER_PAGE: u32 = 100;

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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{GazetteError, decode_media, listing_query};
    use crate::golden::golden;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ListingCase {
        after: String,
        total_pages: u32,
        queries: Vec<String>,
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
