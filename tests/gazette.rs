mod support;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::Query;
use axum::http::{StatusCode, Uri};
use axum::routing::get;
use chrono::{DateTime, Utc};
use diario_alerr::domain::Edition;
use diario_alerr::gazette::{Client, GazetteError, HTTP_TIMEOUT};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use support::gazette_site::{GazetteSite, SiteEdition};
use support::pdf::pdf;
use support::require_pdftotext;

struct TestServer {
    url: String,
    task: JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(router: Router) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("address"));
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("serve");
    });
    TestServer { url, task }
}

fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .expect("timestamp")
        .to_utc()
}

fn edition_five() -> SiteEdition {
    SiteEdition {
        id: 5,
        published: utc("2026-11-06T20:00:00Z"),
        title: "DIARIO-ALE-RR-ED.-5".to_owned(),
        pages: vec![
            "Mesa Diretora".to_owned(),
            "Nomeacao de TALES SABINI".to_owned(),
        ],
    }
}

fn edition_at(url: String) -> Edition {
    Edition {
        id: 5,
        published: utc("2026-11-06T20:00:00Z"),
        url,
        title: String::new(),
    }
}

fn terms() -> Vec<String> {
    vec!["Tales Sabini".to_owned()]
}

async fn within_client_timeout<T>(work: impl Future<Output = T>) -> T {
    let started = Instant::now();
    let result = tokio::time::timeout(Duration::from_secs(30), work)
        .await
        .expect("the request ended");
    let elapsed = started.elapsed();
    assert!(elapsed < Duration::from_secs(3), "took {elapsed:?}");
    result
}

fn assert_timeout(error: &GazetteError) {
    assert!(
        matches!(error, GazetteError::Http(inner) if inner.is_timeout()),
        "{error}"
    );
}

#[test]
fn fake_pdf_matches_go() {
    let golden = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/golden/fake.pdf"
    ));
    assert_eq!(
        pdf(&["Mesa Diretora", "Nomeacao de (TALES) \\ SABINI"]),
        golden
    );
}

#[tokio::test]
async fn list_editions_follows_pagination() {
    let queries = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&queries);
    let router = Router::new().route(
        "/",
        get(move |uri: Uri| {
            let recorded = Arc::clone(&recorded);
            async move {
                recorded
                    .lock()
                    .unwrap()
                    .push(uri.query().unwrap_or_default().to_owned());
                let Query(params) = Query::<HashMap<String, String>>::try_from_uri(&uri).unwrap();
                let page = &params["page"];
                let body = json!([{
                    "id": page.parse::<i64>().unwrap(),
                    "date_gmt": format!("2026-11-0{page}T10:00:00"),
                    "source_url": format!("https://x/{page}.pdf"),
                    "title": {"rendered": format!("ED &#8211; {page}")},
                }]);
                ([("X-WP-TotalPages", "2")], body.to_string())
            }
        }),
    );
    let server = serve(router).await;
    let client = Client::new(server.url.clone(), HTTP_TIMEOUT).unwrap();

    let editions = client
        .list_editions(utc("2026-10-31T00:00:00Z"))
        .await
        .unwrap();

    assert_eq!(
        editions,
        [
            Edition {
                id: 1,
                published: utc("2026-11-01T10:00:00Z"),
                url: "https://x/1.pdf".to_owned(),
                title: "ED – 1".to_owned(),
            },
            Edition {
                id: 2,
                published: utc("2026-11-02T10:00:00Z"),
                url: "https://x/2.pdf".to_owned(),
                title: "ED – 2".to_owned(),
            },
        ]
    );
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 2, "{queries:?}");
    assert!(
        queries[0].contains("after=2026-10-31T00%3A00%3A00"),
        "{queries:?}"
    );
    assert!(
        queries[0].contains("mime_type=application%2Fpdf"),
        "{queries:?}"
    );
}

#[tokio::test]
async fn list_editions_rejects_an_unreadable_date() {
    let router = Router::new().route(
        "/",
        get(|| async { r#"[{"id": 7, "date_gmt": "yesterday"}]"# }),
    );
    let server = serve(router).await;
    let client = Client::new(server.url.clone(), HTTP_TIMEOUT).unwrap();

    let error = client.list_editions(Utc::now()).await.unwrap_err();

    assert!(
        matches!(error, GazetteError::Date { edition: 7, .. }),
        "{error}"
    );
    assert!(error.to_string().contains("edition 7"), "{error}");
}

#[tokio::test]
async fn list_editions_accepts_only_200() {
    let router = Router::new().route("/", get(|| async { StatusCode::NO_CONTENT }));
    let server = serve(router).await;
    let client = Client::new(server.url.clone(), HTTP_TIMEOUT).unwrap();

    let error = client.list_editions(Utc::now()).await.unwrap_err();

    assert!(error.to_string().contains("204 No Content"), "{error}");
}

#[tokio::test]
async fn list_editions_reads_the_gazette_site_after_the_cursor() {
    let site = GazetteSite::start(vec![edition_five()]).await;
    site.publish(SiteEdition {
        id: 6,
        published: utc("2026-11-07T20:00:00Z"),
        title: "ED &#8211; 6".to_owned(),
        pages: vec![],
    });
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).unwrap();

    let recent = client
        .list_editions(utc("2026-11-06T20:00:00Z"))
        .await
        .unwrap();
    let none = client
        .list_editions(utc("2026-11-07T20:00:00Z"))
        .await
        .unwrap();

    assert_eq!(
        recent,
        [Edition {
            id: 6,
            published: utc("2026-11-07T20:00:00Z"),
            url: site.pdf_url(6),
            title: "ED – 6".to_owned(),
        }]
    );
    assert_eq!(none, []);
    assert_eq!(site.queries().len(), 2);
}

#[tokio::test]
async fn scan_finds_terms_in_the_pdf_pages() {
    if !require_pdftotext() {
        return;
    }
    let site = GazetteSite::start(vec![edition_five()]).await;
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).unwrap();

    let hits = client
        .scan(&edition_at(site.pdf_url(5)), &terms())
        .await
        .unwrap()
        .expect("hits");

    assert_eq!(
        hits.pages,
        HashMap::from([("Tales Sabini".to_owned(), vec![2])])
    );
}

#[tokio::test]
async fn scan_reports_a_missing_pdf() {
    let site = GazetteSite::start(vec![]).await;
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).unwrap();

    let error = client
        .scan(&edition_at(site.pdf_url(99)), &terms())
        .await
        .unwrap_err();

    assert!(error.to_string().contains("404 Not Found"), "{error}");
}

#[tokio::test]
async fn stalled_download_fails_within_the_client_timeout() {
    let site = GazetteSite::start(vec![edition_five()]).await;
    let client = Client::new(site.api_url(), Duration::from_secs(1)).unwrap();
    site.stall();

    let result = within_client_timeout(client.scan(&edition_at(site.pdf_url(5)), &terms())).await;

    assert_timeout(&result.unwrap_err());
}

#[tokio::test]
async fn stalled_listing_fails_within_the_client_timeout() {
    let site = GazetteSite::start(vec![edition_five()]).await;
    let client = Client::new(site.api_url(), Duration::from_secs(1)).unwrap();
    site.stall();

    let result = within_client_timeout(client.list_editions(utc("2026-11-01T00:00:00Z"))).await;

    assert_timeout(&result.unwrap_err());
    assert_eq!(site.queries().len(), 1);
}

#[tokio::test]
async fn a_stalled_site_sends_headers_and_a_first_chunk_then_nothing() {
    let site = GazetteSite::start(vec![edition_five()]).await;
    site.stall();
    let http = reqwest::Client::new();

    for (url, first_chunk) in [
        (site.pdf_url(5), &b"%PDF-1.4\n"[..]),
        (site.api_url(), &b"["[..]),
    ] {
        let mut response = http.get(&url).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let chunk = response.chunk().await.unwrap().expect("a first chunk");
        assert_eq!(&chunk[..], first_chunk);
        let rest = tokio::time::timeout(Duration::from_millis(300), response.chunk()).await;
        assert!(rest.is_err(), "{url}: the body went on");
    }
}
