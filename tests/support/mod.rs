#![allow(dead_code)]

pub mod gazette_site;
pub mod pdf;

use std::env;
use std::process::{Command, Stdio};

use diario_alerr::store::MIGRATIONS;
use monitor_hub_plugin::migrate::up;
use monitor_hub_plugin_testkit::{Database, database};
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
