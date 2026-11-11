#![allow(dead_code)]

pub mod channels;
pub mod fake_gazette;
pub mod gazette_site;
pub mod go;
pub mod grpc;
pub mod pdf;
pub mod process;

use std::env;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use diario_alerr::store::MIGRATIONS;
use diario_alerr::timestamp::Clock;
use monitor_hub_plugin::migrate::up;
use monitor_hub_plugin_testkit::{Database, database};
use tokio::time::{Instant, sleep};
use tokio_postgres::NoTls;

pub async fn migrated_database() -> Option<Database> {
    let database = database().await?;
    let (mut client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("connect to the test database");
    let driver = tokio::spawn(connection);
    up(&mut client, MIGRATIONS)
        .await
        .expect("migrate the test database");
    drop(client);
    driver
        .await
        .expect("join the migration connection")
        .expect("close the migration connection");
    Some(database)
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
        sleep(Duration::from_millis(10)).await;
    }
}
