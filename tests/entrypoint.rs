mod support;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use diario_alerr::domain::Alert;
use diario_alerr::store::{Store, WRITER_LOCK_ID, acquire_writer_lock};
use monitor_hub_sdk::NOT_LAUNCHED_BY_HUB;
use serde_json::Value;
use tempfile::TempDir;
use tokio_postgres::{Client, NoTls};

use support::gazette_site::GazetteSite;
use support::process::{
    COOKIE, Exited, error_lines, require_sh, run_monitor, spawn_monitor, terminate,
};
use support::{eventually, migrated_database, require_pdftotext, utc};

const EXIT_LIMIT: Duration = Duration::from_secs(30);
const STOP_LIMIT: Duration = Duration::from_secs(2);
const UNREACHABLE_GAZETTE: &str = "http://127.0.0.1:9/media";
const UNREACHABLE_DATABASE: &str = "postgres://diario:hunter2@127.0.0.1:1/diario";

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

async fn applied_migrations(client: &Client) -> Vec<(String, i64)> {
    client
        .query(
            "SELECT version, applied_at FROM seaql_migrations ORDER BY version",
            &[],
        )
        .await
        .expect("seaql_migrations")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

#[test]
fn without_the_cookie_explains_and_exits_1() {
    let exited = run_monitor(&["monitor"], &[], EXIT_LIMIT);

    assert_refused_before_the_handshake(&exited);
    assert_eq!(exited.stderr, format!("{NOT_LAUNCHED_BY_HUB}\n"));
}

#[test]
fn without_the_monitor_argument_explains_and_exits_1() {
    let exited = run_monitor(&[], &[COOKIE], EXIT_LIMIT);

    assert_refused_before_the_handshake(&exited);
    assert_eq!(exited.stderr, format!("{NOT_LAUNCHED_BY_HUB}\n"));
}

#[test]
fn config_error_exits_before_the_handshake() {
    let exited = run_monitor(
        &["monitor"],
        &[COOKIE, ("WATCH_TERMS", "PAULO SANTOS")],
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

#[test]
fn a_term_too_long_to_match_exits_before_the_handshake() {
    let terms = format!("PAULO SANTOS, {}", vec!["PAULO"; 32].join(" "));
    let exited = run_monitor(
        &["monitor"],
        &[
            COOKIE,
            ("DATABASE_URL", UNREACHABLE_DATABASE),
            ("WATCH_TERMS", &terms),
        ],
        EXIT_LIMIT,
    );

    assert_refused_before_the_handshake(&exited);
    let error = single_error(&exited);
    assert_eq!(error["@message"], "fatal", "{}", exited.stderr);
    assert_eq!(
        error["error"], "WATCH_TERMS term 2 (32 words) is too long to match: shorten it",
        "{}",
        exited.stderr
    );
}

#[test]
fn a_missing_pdftotext_stops_the_start_up() {
    let empty = tempfile::tempdir().expect("an empty PATH directory");

    let exited = run_monitor(
        &["monitor"],
        &[
            COOKIE,
            ("DATABASE_URL", UNREACHABLE_DATABASE),
            ("WATCH_TERMS", "PAULO SANTOS"),
            ("PATH", path_text(&empty)),
        ],
        EXIT_LIMIT,
    );

    assert_refused_before_the_handshake(&exited);
    let error = single_error(&exited);
    assert_eq!(error["@message"], "fatal", "{}", exited.stderr);
    let text = error["error"].as_str().expect("an error field");
    assert!(
        text.starts_with("checking pdftotext: "),
        "{}",
        exited.stderr
    );
    assert!(!exited.stderr.contains("hunter2"), "{}", exited.stderr);
}

#[test]
fn a_url_without_a_password_stops_the_start_before_pgpass_is_read() {
    if !require_pdftotext() {
        return;
    }
    let marker = "PwLeakMarker77";
    let dir = tempfile::tempdir().expect("a directory for the pgpass file");
    let pgpass = dir.path().join("pgpass");
    fs::write(&pgpass, format!("*:*:*:{marker}\n")).expect("write the pgpass file");
    fs::set_permissions(&pgpass, fs::Permissions::from_mode(0o600))
        .expect("restrict the pgpass file");

    let exited = run_monitor(
        &["monitor"],
        &[
            COOKIE,
            (
                "DATABASE_URL",
                "postgres://diario@127.0.0.1:1/diario?sslmode=disable",
            ),
            ("WATCH_TERMS", "PAULO SANTOS"),
            ("PGPASSFILE", pgpass.to_str().expect("a UTF-8 path")),
        ],
        EXIT_LIMIT,
    );

    assert_refused_before_the_handshake(&exited);
    assert!(!exited.stderr.contains(marker), "{}", exited.stderr);
    let error = single_error(&exited);
    assert_eq!(
        error["error"],
        "acquiring the database writer lock: DATABASE_URL has no password: add the role's password to the URL",
        "{}",
        exited.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn second_process_refuses_to_start() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let _held = acquire_writer_lock(&db.url).await.expect("hold the lock");
    let admin = connect(&db.url).await;
    let before = applied_migrations(&admin).await;
    let sockets = socket_dir();

    let exited = run_monitor(
        &["monitor"],
        &[
            COOKIE,
            ("DATABASE_URL", &db.url),
            ("WATCH_TERMS", "PAULO SANTOS"),
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
    assert_eq!(applied_migrations(&admin).await, before);
}

#[tokio::test(flavor = "multi_thread")]
async fn sigterm_during_a_cycle_exits_within_two_seconds() {
    if !require_sh() || !require_pdftotext() {
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

    let mut spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TZ", "America/Boa_Vista"),
        ("TMPDIR", path_text(&sockets)),
    ]);
    assert!(spawned.socket.exists(), "{}", spawned.socket.display());
    eventually(
        tokio::time::Instant::now() + Duration::from_secs(10),
        "the cycle asks the site for editions",
        || !site.queries().is_empty(),
    )
    .await;
    let query = &site.queries()[0];
    assert!(query.contains("after=2026-11-01T10%3A00%3A00"), "{query}");

    let signalled = Instant::now();
    terminate(&mut spawned.child);
    let exited = spawned.exit_within(STOP_LIMIT);

    assert!(
        signalled.elapsed() < STOP_LIMIT,
        "{:?}",
        signalled.elapsed()
    );
    assert_eq!(exited.status.code(), Some(0), "{}", exited.stderr);
    assert!(!spawned.socket.exists(), "{}", spawned.socket.display());
    assert_eq!(exited.stdout.lines().count(), 1, "{}", exited.stdout);
    assert!(error_lines(&exited.stderr).is_empty(), "{}", exited.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_up_removes_its_own_leftover_pdfs() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let temp = socket_dir();
    let elsewhere = tempfile::tempdir().expect("a directory for the link target");
    let leftover = temp.path().join("edition-Ab3xY9.pdf");
    let unrelated = temp.path().join("report.pdf");
    let link = temp.path().join("edition-Zz9Qq1.pdf");
    let target = elsewhere.path().join("edition-Kk2Pp8.pdf");
    for file in [&leftover, &unrelated, &target] {
        fs::write(file, b"%PDF-1.4\n").expect("write a file");
    }
    std::os::unix::fs::symlink(&target, &link).expect("link");

    let _spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
        ("GAZETTE_API_URL", UNREACHABLE_GAZETTE),
        ("TMPDIR", path_text(&temp)),
    ]);

    assert!(!leftover.exists(), "{}", leftover.display());
    assert!(unrelated.is_file(), "{}", unrelated.display());
    assert!(link.is_symlink(), "{}", link.display());
    assert!(target.is_file(), "{}", target.display());
}

#[tokio::test(flavor = "multi_thread")]
async fn losing_the_database_lock_exits_1() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![]).await;
    let sockets = socket_dir();
    let mut spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
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

    let exited = spawned.exit_within(Duration::from_secs(5));

    assert_eq!(exited.status.code(), Some(1), "{}", exited.stderr);
    let error = single_error(&exited);
    assert_eq!(error["@message"], "fatal", "{}", exited.stderr);
    let text = error["error"].as_str().expect("an error field");
    assert!(
        text.starts_with("the database lock was lost: the lock's connection"),
        "{}",
        exited.stderr
    );
    assert!(!spawned.socket.exists(), "{}", spawned.socket.display());
}
