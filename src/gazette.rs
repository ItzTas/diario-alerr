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

const USER_AGENT: &str = concat!("diario-alerr/", env!("CARGO_PKG_VERSION"));
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const PER_PAGE: u32 = 100;
const MAX_PAGES: u32 = 100;
const MAX_LISTING_BYTES: usize = 4 << 20;

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
    #[error("decode media list: {0}")]
    ListingBody(#[source] reqwest::Error),
    #[error("media list: more than {limit} pages")]
    TooManyPages { limit: u32 },
    #[error("media list: response larger than {}", byte_size(*.limit))]
    ListingTooLarge { limit: usize },
    #[error("edition {edition}: date_gmt {} is not a WordPress timestamp", go_quote(.raw))]
    Date { edition: i64, raw: String },
    #[error("download {url}: {source}")]
    Download { url: String, source: reqwest::Error },
    #[error("download {url}: larger than {}", byte_size(*.limit))]
    PdfTooLarge { url: String, limit: usize },
    #[error("create temp file: {0}")]
    CreateTempFile(#[source] std::io::Error),
    #[error("write {file}: {source}")]
    WriteTempFile {
        file: String,
        source: std::io::Error,
    },
    #[error("pdftotext: {0}")]
    Pdftotext(String),
    #[error("pdftotext: more than {} of text", byte_size(*.limit))]
    TooMuchText { limit: usize },
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
        self.list_editions_within(after, MAX_PAGES, MAX_LISTING_BYTES)
            .await
    }

    async fn list_editions_within(
        &self,
        after: DateTime<Utc>,
        max_pages: u32,
        max_bytes: usize,
    ) -> Result<Vec<Edition>, GazetteError> {
        let mut editions = Vec::new();
        for page in 1..=max_pages {
            let url = format!("{}?{}", self.api_url, listing_query(after, page));
            let (batch, total_pages) = self.fetch_page(&url, max_bytes).await?;
            let last = batch.is_empty() || i64::from(page) >= total_pages;
            editions.extend(batch);
            if last {
                return Ok(editions);
            }
            if total_pages > i64::from(max_pages) {
                break;
            }
        }
        Err(GazetteError::TooManyPages { limit: max_pages })
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

    async fn fetch_page(
        &self,
        url: &str,
        max_bytes: usize,
    ) -> Result<(Vec<Edition>, i64), GazetteError> {
        let response = get(&self.http, url).await?;
        let total_pages = total_pages(&response);
        let body = read_listing(response, max_bytes).await?;
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

async fn read_listing(mut response: Response, limit: usize) -> Result<Vec<u8>, GazetteError> {
    let too_large = GazetteError::ListingTooLarge { limit };
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(too_large);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(GazetteError::ListingBody)? {
        if body.len() + chunk.len() > limit {
            return Err(too_large);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn byte_size(bytes: usize) -> String {
    let units = [(1 << 20, "MiB"), (1 << 10, "KiB")];
    for (unit, name) in units {
        if bytes >= unit && bytes.is_multiple_of(unit) {
            return format!("{} {name}", bytes / unit);
        }
    }
    format!("{bytes} bytes")
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

impl crate::monitor::Gazette for Client {
    fn list_editions(
        &self,
        after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send {
        Client::list_editions(self, after)
    }

    fn scan(
        &self,
        edition: &Edition,
        terms: &[String],
    ) -> impl Future<Output = Result<Option<Hits>, GazetteError>> + Send {
        Client::scan(self, edition, terms)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::Future;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::http::{HeaderMap, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use chrono::{DateTime, Utc};
    use futures_util::stream::{self, StreamExt};
    use serde::Deserialize;
    use tokio::net::TcpListener;

    use super::{
        Client, GazetteError, HTTP_TIMEOUT, MAX_LISTING_BYTES, MAX_PAGES, decode_media,
        listing_query,
    };
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

    const EDITION: &str = r#"[{"id": 1, "date_gmt": "2026-11-01T10:00:00"}]"#;

    async fn promptly<T>(work: impl Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .expect("the call returned promptly")
    }

    fn page_of(uri: &Uri) -> u32 {
        uri.query()
            .unwrap_or_default()
            .split('&')
            .find_map(|pair| pair.strip_prefix("page="))
            .and_then(|page| page.parse().ok())
            .expect("a page parameter")
    }

    async fn listing(respond: fn(u32) -> Response) -> (Client, Arc<AtomicUsize>) {
        let requests = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&requests);
        let router = Router::new().route(
            "/",
            get(move |uri: Uri| {
                let counted = Arc::clone(&counted);
                async move {
                    counted.fetch_add(1, Ordering::SeqCst);
                    respond(page_of(&uri))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        (Client::new(url, HTTP_TIMEOUT).unwrap(), requests)
    }

    fn page(total: String, body: &'static str) -> Response {
        ([("X-WP-TotalPages", total)], body).into_response()
    }

    fn after() -> DateTime<Utc> {
        utc("2026-10-31T00:00:00Z")
    }

    #[tokio::test]
    async fn listings_announcing_too_many_pages_stop_with_an_error() {
        let (announced, announced_requests) = listing(|_| page("1000".to_owned(), EDITION)).await;
        let (growing, growing_requests) =
            listing(|number| page((number + 1).to_string(), EDITION)).await;

        let announced_error =
            promptly(announced.list_editions_within(after(), 3, MAX_LISTING_BYTES))
                .await
                .unwrap_err();
        let growing_error = promptly(growing.list_editions_within(after(), 3, MAX_LISTING_BYTES))
            .await
            .unwrap_err();

        assert_eq!(announced_error.to_string(), "media list: more than 3 pages");
        assert_eq!(announced_requests.load(Ordering::SeqCst), 1);
        assert_eq!(growing_error.to_string(), "media list: more than 3 pages");
        assert_eq!(growing_requests.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn an_empty_page_ends_the_listing() {
        let (client, requests) = listing(|number| {
            let body = if number == 1 { EDITION } else { "[]" };
            page("50".to_owned(), body)
        })
        .await;

        let editions = promptly(client.list_editions_within(after(), MAX_PAGES, MAX_LISTING_BYTES))
            .await
            .unwrap();

        assert_eq!(editions.len(), 1);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn listing_bodies_above_the_limit_are_refused_while_streaming() {
        let (client, _) = listing(|_| {
            let blank = Bytes::from_static(&[b' '; 4096]);
            let body = stream::iter([Ok::<_, Infallible>(Bytes::from_static(b"["))])
                .chain(stream::repeat(Ok(blank)).take(256))
                .chain(stream::pending());
            ([("X-WP-TotalPages", "1")], Body::from_stream(body)).into_response()
        })
        .await;

        let error = promptly(client.list_editions_within(after(), MAX_PAGES, 64 << 10))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "media list: response larger than 64 KiB");
    }

    #[tokio::test]
    async fn listing_bodies_announced_above_the_limit_are_refused_before_reading() {
        let (client, _) = listing(|_| {
            let body = Body::from_stream(stream::pending::<Result<Bytes, Infallible>>());
            (
                [("X-WP-TotalPages", "1"), ("Content-Length", "1048576")],
                body,
            )
                .into_response()
        })
        .await;

        let error = promptly(client.list_editions_within(after(), MAX_PAGES, 64 << 10))
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "media list: response larger than 64 KiB");
    }

    #[tokio::test]
    async fn requests_name_the_plugin_and_its_version() {
        let (agents, mut received) = tokio::sync::mpsc::unbounded_channel();
        let router = Router::new().route(
            "/",
            get(move |headers: HeaderMap| async move {
                let agent = headers.get("user-agent").cloned();
                agents.send(agent).expect("record the user agent");
                page("1".to_owned(), "[]")
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        promptly(
            Client::new(url, HTTP_TIMEOUT)
                .unwrap()
                .list_editions(after()),
        )
        .await
        .unwrap();

        let agent = received.recv().await.expect("a request").expect("a header");
        assert_eq!(agent, "diario-alerr/2.0.0");
    }

    #[test]
    fn listing_bounds_read_as_the_production_limits() {
        let pages = GazetteError::TooManyPages { limit: MAX_PAGES };
        let bytes = GazetteError::ListingTooLarge {
            limit: MAX_LISTING_BYTES,
        };

        assert_eq!(pages.to_string(), "media list: more than 100 pages");
        assert_eq!(bytes.to_string(), "media list: response larger than 4 MiB");
        assert_eq!(super::byte_size(1500), "1500 bytes");
    }
}
