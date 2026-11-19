#![allow(dead_code)]

pub mod fake_gazette;
pub mod gazette_site;
pub mod pdf;
pub mod postgres;
pub mod process;
pub mod proxy;
pub mod schema;

pub use postgres::{Database, database};

use std::env;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use diario_alerr::store::{WRITER_LOCK_ID, acquire_writer_lock};
use diario_alerr::timestamp::Clock;
use monitor_hub_plugin::Signals;
use sea_orm::{ConnectOptions, DatabaseConnection};
use tokio::time::{Instant, sleep, timeout};
use tokio_postgres::{Client, NoTls};

pub const SIGNAL_WAIT: Duration = Duration::from_secs(5);
const BACKEND_WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);
const WRITER_LOCK_HOLDERS: &str = "SELECT pid FROM pg_locks
WHERE locktype = 'advisory'
  AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
  AND objsubid = 1
  AND ((classid::bigint << 32) | objid::bigint) = $1";
const BACKEND_ALIVE: &str = "SELECT count(*) FROM pg_stat_activity WHERE pid = $1";

pub async fn orm(url: &str) -> DatabaseConnection {
    let mut options = ConnectOptions::new(url);
    options.sqlx_logging(false);
    sea_orm::Database::connect(options)
        .await
        .expect("connect to the test database through SeaORM")
}

pub async fn migrated_database() -> Option<Database> {
    let database = database().await?;
    let lock = acquire_writer_lock(&database.url)
        .await
        .expect("take the test database's writer lock");
    lock.migrate().await.expect("migrate the test database");
    drop(lock);
    let admin = admin(&database.url).await;
    let deadline = Instant::now() + BACKEND_WAIT;
    while !writer_lock_holders(&admin).await.is_empty() {
        assert!(
            Instant::now() < deadline,
            "the migration's writer lock was still held after 5 seconds"
        );
        sleep(POLL).await;
    }
    Some(database)
}

pub async fn admin(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect the admin client");
    tokio::spawn(connection);
    client
}

pub async fn writer_lock_holders(client: &Client) -> Vec<i32> {
    client
        .query(WRITER_LOCK_HOLDERS, &[&WRITER_LOCK_ID])
        .await
        .expect("list the writer lock's holders")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

pub async fn end_the_writer_locks_connection(client: &Client) {
    let [holder] = writer_lock_holders(client).await[..] else {
        panic!("the writer lock has no single holder");
    };
    let ended: bool = client
        .query_one("SELECT pg_terminate_backend($1)", &[&holder])
        .await
        .expect("terminate the writer lock's backend")
        .get(0);
    assert!(ended, "the writer lock's backend was not terminated");
    let deadline = Instant::now() + BACKEND_WAIT;
    loop {
        let alive: i64 = client
            .query_one(BACKEND_ALIVE, &[&holder])
            .await
            .expect("look for the writer lock's backend")
            .get(0);
        if alive == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the writer lock's backend was still running after 5 seconds"
        );
        sleep(POLL).await;
    }
}

pub fn require_pdftotext() -> bool {
    let installed = Command::new("pdftotext")
        .arg("-v")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if installed {
        return true;
    }
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("pdftotext is required when CI is set");
    }
    eprintln!("skipping: pdftotext is not installed");
    false
}

pub fn utc(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("bad timestamp {raw:?}"))
        .to_utc()
}

pub fn fixed_clock(at: DateTime<Utc>) -> Clock {
    Arc::new(move || at)
}

pub async fn eventually(deadline: Instant, what: &str, check: impl Fn() -> bool) {
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        sleep(POLL).await;
    }
}

pub async fn next_signal(signals: &mut Signals) -> i64 {
    timeout(SIGNAL_WAIT, signals.changed())
        .await
        .expect("a signal within 5 seconds")
}

pub async fn pending_signal(signals: &mut Signals) -> Option<i64> {
    timeout(Duration::ZERO, signals.changed()).await.ok()
}
