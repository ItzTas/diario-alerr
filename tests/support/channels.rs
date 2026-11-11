use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri};
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const NTFY_PATH: &str = "/ntfy";
const TELEGRAM_PATH: &str = "/telegram";

pub struct Channels {
    base_url: String,
    received: Arc<Received>,
    server: JoinHandle<()>,
}

#[derive(Default)]
struct Received {
    ntfy_titles: Mutex<Vec<String>>,
    telegram_calls: AtomicUsize,
}

impl Channels {
    pub async fn start() -> Channels {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the fake notification channels");
        let address = listener.local_addr().expect("fake channels address");
        let received = Arc::new(Received::default());
        let router = Router::new()
            .fallback(receive)
            .with_state(Arc::clone(&received));
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("serve the fake notification channels");
        });
        Channels {
            base_url: format!("http://{address}"),
            received,
            server,
        }
    }

    pub fn ntfy_url(&self) -> String {
        format!("{}{NTFY_PATH}", self.base_url)
    }

    pub fn telegram_api_url(&self) -> String {
        format!("{}{TELEGRAM_PATH}", self.base_url)
    }

    pub fn ntfy_titles(&self) -> Vec<String> {
        lock(&self.received.ntfy_titles).clone()
    }

    pub fn telegram_count(&self) -> usize {
        self.received.telegram_calls.load(Ordering::SeqCst)
    }
}

impl Drop for Channels {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn receive(
    State(received): State<Arc<Received>>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> StatusCode {
    if method != Method::POST {
        return StatusCode::METHOD_NOT_ALLOWED;
    }
    let path = uri.path();
    if path == NTFY_PATH {
        lock(&received.ntfy_titles).push(ntfy_title(&body));
    }
    if path.starts_with(&format!("{TELEGRAM_PATH}/")) {
        received.telegram_calls.fetch_add(1, Ordering::SeqCst);
    }
    StatusCode::OK
}

fn ntfy_title(body: &[u8]) -> String {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return format!("<not JSON: {}>", String::from_utf8_lossy(body));
    };
    message["title"]
        .as_str()
        .map_or_else(|| format!("<no title: {message}>"), str::to_owned)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
