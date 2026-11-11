mod support;

use std::time::{Duration, Instant};

use diario_alerr::domain::Alert;
use diario_alerr::store::{Store, WRITER_LOCK_ID, acquire_writer_lock};
use monitor_hub_plugin::NOT_LAUNCHED_BY_HUB;
use serde_json::Value;
use tempfile::TempDir;
use tokio_postgres::{Client, NoTls};

use support::gazette_site::GazetteSite;
use support::process::{
    COOKIE, Exited, error_lines, require_sh, run_plugin, spawn_plugin, terminate,
};
use support::{eventually, migrated_database, utc};

const EXIT_LIMIT: Duration = Duration::from_secs(30);
const STOP_LIMIT: Duration = Duration::from_secs(2);
const UNREACHABLE_GAZETTE: &str = "http://127.0.0.1:9/media";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

fn socket_dir() -> TempDir {
    tempfile::tempdir().expect("socket directory")
}

fn path_text(dir: &TempDir) -> &str {
    dir.path().to_str().expect("a UTF-8 temporary directory")
}

fn assert_refused_before_the_handshake(exited: &Exited) {
    assert_eq!(exited.status.code(), Some(1), "{}", exited.stderr);
    assert_eq!(exited.stdout, "");
}

fn single_error(exited: &Exited) -> Value {
    let errors = error_lines(&exited.stderr);
    let [error] = &errors[..] else {
        panic!("expected one error line:\n{}", exited.stderr);
    };
    error.clone()
}

async fn goose_versions(client: &Client) -> Vec<(i64, bool)> {
    client
        .query(
            "SELECT version_id, is_applied FROM goose_db_version ORDER BY id",
            &[],
        )
        .await
        .expect("goose_db_version")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

#[test]
fn without_the_cookie_explains_and_exits_1() {
    let exited = run_plugin(&["plugin"], &[], EXIT_LIMIT);

    assert_refused_before_the_handshake(&exited);
    assert_eq!(exited.stderr, format!("{NOT_LAUNCHED_BY_HUB}\n"));
}

#[test]
fn without_the_plugin_argument_explains_and_exits_1() {
    let exited = run_plugin(&[], &[COOKIE], EXIT_LIMIT);

    assert_refused_before_the_handshake(&exited);
    assert_eq!(exited.stderr, format!("{NOT_LAUNCHED_BY_HUB}\n"));
}

#[test]
fn config_error_exits_before_the_handshake() {
    let exited = run_plugin(
        &["plugin"],
        &[COOKIE, ("WATCH_TERMS", "TALES SABINI")],
        EXIT_LIMIT,
    );

    assert_refused_before_the_handshake(&exited);
    let error = single_error(&exited);
    assert_eq!(
        error["error"], "DATABASE_URL is empty: set the Postgres address",
        "{}",
        exited.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_process_refuses_to_start() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let _held = acquire_writer_lock(&db.url).await.expect("hold the lock");
    let admin = connect(&db.url).await;
    let before = goose_versions(&admin).await;
    let sockets = socket_dir();

    let exited = run_plugin(
        &["plugin"],
        &[
            COOKIE,
            ("DATABASE_URL", &db.url),
            ("WATCH_TERMS", "TALES SABINI"),
            ("GAZETTE_API_URL", UNREACHABLE_GAZETTE),
            ("TMPDIR", path_text(&sockets)),
        ],
        EXIT_LIMIT,
    );

    assert_refused_before_the_handshake(&exited);
    let error = single_error(&exited);
    let text = error["error"].as_str().expect("an error field");
    assert!(
        text.ends_with("another diario-alerr process holds the database lock"),
        "{}",
        exited.stderr
    );
    assert!(!exited.stderr.contains(&db.url), "{}", exited.stderr);
    assert_eq!(goose_versions(&admin).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_during_a_cycle_exits_within_two_seconds() {
    if !require_sh() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let cursor = utc("2026-11-02T10:00:00Z");
    let welcome = Alert {
        id: "welcome".to_owned(),
        title: "started".to_owned(),
        link: "https://diarios.al.rr.leg.br".to_owned(),
        body: "Watching".to_owned(),
        published: cursor,
    };
    Store::new(&db.url)
        .expect("store")
        .initialize(cursor, &welcome)
        .await
        .expect("seed the cursor");
    let site = GazetteSite::start(vec![]).await;
    site.stall();
    let sockets = socket_dir();

    let mut plugin = spawn_plugin(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "TALES SABINI"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TZ", "America/Boa_Vista"),
        ("TMPDIR", path_text(&sockets)),
    ]);
    assert!(plugin.socket.exists(), "{}", plugin.socket.display());
    eventually(
        tokio::time::Instant::now() + Duration::from_secs(10),
        "the cycle asks the site for editions",
        || !site.queries().is_empty(),
    )
    .await;
    let query = &site.queries()[0];
    assert!(query.contains("after=2026-11-01T10%3A00%3A00"), "{query}");

    let signalled = Instant::now();
    terminate(&mut plugin.child);
    let exited = plugin.exit_within(STOP_LIMIT);

    assert!(
        signalled.elapsed() < STOP_LIMIT,
        "{:?}",
        signalled.elapsed()
    );
    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
    assert!(!plugin.socket.exists(), "{}", plugin.socket.display());
    assert_eq!(exited.stdout.lines().count(), 1, "{}", exited.stdout);
    assert!(error_lines(&exited.stderr).is_empty(), "{}", exited.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_database_lock_exits_1() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![]).await;
    let sockets = socket_dir();
    let mut plugin = spawn_plugin(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "TALES SABINI"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TMPDIR", path_text(&sockets)),
    ]);

    let admin = connect(&db.url).await;
    let terminated: Vec<bool> = admin
        .query(
            "SELECT pg_terminate_backend(pid) FROM pg_locks
             WHERE locktype = 'advisory'
               AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
               AND objsubid = 1
               AND ((classid::bigint << 32) | objid::bigint) = $1",
            &[&WRITER_LOCK_ID],
        )
        .await
        .expect("terminate the lock's backend")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(terminated, [true]);

    let exited = plugin.exit_within(Duration::from_secs(5));

    assert_eq!(exited.status.code(), Some(1), "{}", exited.stderr);
    let lost: Vec<Value> = error_lines(&exited.stderr)
        .into_iter()
        .filter(|line| line["@message"] == "the database lock was lost")
        .collect();
    assert_eq!(lost.len(), 1, "{}", exited.stderr);
    assert!(
        lost[0]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("terminating connection")),
        "{}",
        exited.stderr
    );
    assert!(!plugin.socket.exists(), "{}", plugin.socket.display());
}
