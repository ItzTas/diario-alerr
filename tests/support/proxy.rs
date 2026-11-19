use std::future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};

const DEFAULT_PORT: u16 = 5432;

pub struct PausingProxy {
    paused: Arc<AtomicBool>,
    accepting: JoinHandle<()>,
}

impl PausingProxy {
    pub async fn start(target_url: &str) -> (PausingProxy, String) {
        let mut url = Url::parse(target_url).expect("a postgres:// URL");
        let target = format!(
            "{}:{}",
            url.host_str().expect("a host in the URL"),
            url.port().unwrap_or(DEFAULT_PORT)
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the proxy");
        let port = listener.local_addr().expect("the proxy's address").port();
        url.set_host(Some("127.0.0.1"))
            .expect("point the URL at the proxy");
        url.set_port(Some(port))
            .expect("point the URL at the proxy's port");
        let paused = Arc::new(AtomicBool::new(false));
        let accepting = tokio::spawn(accept(listener, target, Arc::clone(&paused)));
        (PausingProxy { paused, accepting }, url.to_string())
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }
}

impl Drop for PausingProxy {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

async fn accept(listener: TcpListener, target: String, paused: Arc<AtomicBool>) {
    let mut links = JoinSet::new();
    while let Ok((client, _)) = listener.accept().await {
        let server = TcpStream::connect(&target)
            .await
            .expect("connect the proxy to the database");
        let (client_read, client_write) = client.into_split();
        let (server_read, server_write) = server.into_split();
        links.spawn(forward(client_read, server_write, Arc::clone(&paused)));
        links.spawn(forward(server_read, client_write, Arc::clone(&paused)));
    }
}

async fn forward(mut from: OwnedReadHalf, mut to: OwnedWriteHalf, paused: Arc<AtomicBool>) {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let Ok(read) = from.read(&mut buffer).await else {
            return;
        };
        if read == 0 {
            return;
        }
        if paused.load(Ordering::SeqCst) {
            // Holds both halves, so the connection stays open and silent.
            future::pending::<()>().await;
        }
        if to.write_all(&buffer[..read]).await.is_err() {
            return;
        }
    }
}
