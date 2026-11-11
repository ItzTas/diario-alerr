use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, NaiveDateTime, Utc};
use futures_util::stream::{self, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const WORDPRESS_LAYOUT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Clone, Debug)]
pub struct SiteEdition {
    pub id: i64,
    pub published: DateTime<Utc>,
    pub title: String,
    pub pages: Vec<String>,
}

pub struct GazetteSite {
    site: Arc<Site>,
    server: JoinHandle<()>,
}

struct Site {
    base_url: String,
    editions: Mutex<Vec<SiteEdition>>,
    queries: Mutex<Vec<String>>,
    stalled: AtomicBool,
}

impl GazetteSite {
    pub async fn start(editions: Vec<SiteEdition>) -> GazetteSite {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake gazette site");
        let address = listener.local_addr().expect("fake gazette site address");
        let site = Arc::new(Site {
            base_url: format!("http://{address}"),
            editions: Mutex::new(editions),
            queries: Mutex::new(Vec::new()),
            stalled: AtomicBool::new(false),
        });
        let router = Router::new()
            .route("/media", get(media))
            .route("/pdf/{id}", get(pdf))
            .with_state(Arc::clone(&site));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve the fake gazette site");
        });
        GazetteSite { site, server }
    }

    pub fn api_url(&self) -> String {
        format!("{}/media", self.site.base_url)
    }

    pub fn pdf_url(&self, id: i64) -> String {
        self.site.pdf_url(id)
    }

    pub fn publish(&self, edition: SiteEdition) {
        lock(&self.site.editions).push(edition);
    }

    pub fn queries(&self) -> Vec<String> {
        lock(&self.site.queries).clone()
    }

    pub fn stall(&self) {
        self.site.stalled.store(true, Ordering::SeqCst);
    }
}

impl Drop for GazetteSite {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Site {
    fn pdf_url(&self, id: i64) -> String {
        format!("{}/pdf/{id}", self.base_url)
    }

    fn is_stalled(&self) -> bool {
        self.stalled.load(Ordering::SeqCst)
    }
}

async fn media(State(site): State<Arc<Site>>, uri: Uri) -> Response {
    lock(&site.queries).push(uri.query().unwrap_or_default().to_owned());
    if site.is_stalled() {
        return ([("X-WP-TotalPages", "1")], never_ending(b"[")).into_response();
    }
    let after = after(&uri);
    let items: Vec<Value> = lock(&site.editions)
        .iter()
        .filter(|edition| edition.published > after)
        .map(|edition| {
            json!({
                "id": edition.id,
                "date_gmt": edition.published.format(WORDPRESS_LAYOUT).to_string(),
                "source_url": site.pdf_url(edition.id),
                "title": {"rendered": edition.title},
            })
        })
        .collect();
    ([("X-WP-TotalPages", "1")], media_body(items)).into_response()
}

fn after(uri: &Uri) -> DateTime<Utc> {
    Query::<HashMap<String, String>>::try_from_uri(uri)
        .ok()
        .and_then(|Query(params)| {
            NaiveDateTime::parse_from_str(params.get("after")?, WORDPRESS_LAYOUT).ok()
        })
        .map_or(DateTime::<Utc>::MIN_UTC, |after| after.and_utc())
}

fn media_body(items: Vec<Value>) -> String {
    if items.is_empty() {
        return "null\n".to_owned();
    }
    format!("{}\n", Value::Array(items))
}

async fn pdf(State(site): State<Arc<Site>>, Path(id): Path<String>) -> Response {
    if site.is_stalled() {
        return (
            [(header::CONTENT_TYPE, "application/pdf")],
            never_ending(b"%PDF-1.4\n"),
        )
            .into_response();
    }
    let editions = lock(&site.editions);
    let Some(edition) = editions.iter().find(|edition| edition.id.to_string() == id) else {
        return (StatusCode::NOT_FOUND, "404 page not found\n").into_response();
    };
    let pages: Vec<&str> = edition.pages.iter().map(String::as_str).collect();
    (
        [(header::CONTENT_TYPE, "application/pdf")],
        super::pdf::pdf(&pages),
    )
        .into_response()
}

fn never_ending(first_chunk: &'static [u8]) -> Body {
    let first = stream::iter([Ok::<_, Infallible>(Bytes::from_static(first_chunk))]);
    Body::from_stream(first.chain(stream::pending()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
