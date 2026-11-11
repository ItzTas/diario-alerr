mod support;

use std::error::Error;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Alert;
use diario_alerr::store::{
    EditionRecord, MIGRATIONS, SequencedAlert, Store, StoreError, WriterLock, acquire_writer_lock,
};
use monitor_hub_plugin::migrate::up;
use monitor_hub_plugin_testkit::database;
use tokio::net::TcpListener;
use tokio_postgres::{Client, NoTls};

use support::migrated_database;

const PASSWORD: &str = "hunter2";

fn start() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z")
        .expect("timestamp")
        .to_utc()
}

fn hours(count: i64) -> TimeDelta {
    TimeDelta::hours(count)
}

fn alert(id: &str, published: DateTime<Utc>) -> Alert {
    Alert {
        id: id.to_owned(),
        title: format!("title {id}"),
        link: format!("https://x/{id}"),
        body: format!("body {id}"),
        published,
    }
}

fn welcome() -> Alert {
    alert("welcome", start())
}

fn edition(id: i64, published: DateTime<Utc>, alert: Option<Alert>) -> EditionRecord {
    EditionRecord {
        id,
        published,
        alert,
    }
}

async fn initialized(url: &str) -> Store {
    let store = Store::new(url).expect("store");
    assert!(
        store
            .initialize(start(), &welcome())
            .await
            .expect("initialize")
    );
    store
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

async fn all_alerts(store: &Store) -> Vec<SequencedAlert> {
    store.alerts_after(0, 1000).await.expect("alerts")
}

fn ids(alerts: &[SequencedAlert]) -> Vec<&str> {
    alerts
        .iter()
        .map(|stored| stored.alert.id.as_str())
        .collect()
}

fn seqs(alerts: &[SequencedAlert]) -> Vec<i64> {
    alerts.iter().map(|stored| stored.seq).collect()
}

async fn seen(store: &Store, ids: &[i64]) -> Vec<i64> {
    let mut seen = store.seen_editions(ids).await.expect("seen editions");
    seen.sort_unstable();
    seen
}

fn everything_printed(error: &(dyn Error + 'static)) -> String {
    let mut printed = format!("{error:?}");
    let mut current = Some(error);
    while let Some(error) = current {
        printed.push('\n');
        printed.push_str(&error.to_string());
        current = error.source();
    }
    printed
}

fn assert_hides_the_login(error: &StoreError, url: &str) {
    let printed = everything_printed(error);
    assert!(!printed.contains(PASSWORD), "{printed}");
    assert!(!printed.contains(url), "{printed}");
}

fn with_login(url: &str, login: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("a postgres:// URL");
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let host_start = rest[..authority_end].rfind('@').map_or(0, |at| at + 1);
    format!("{scheme}://{login}@{}", &rest[host_start..])
}

async fn acquire_within(url: &str, limit: Duration) -> WriterLock {
    let started = Instant::now();
    loop {
        match acquire_writer_lock(url).await {
            Ok(lock) => return lock,
            Err(StoreError::Locked) if started.elapsed() < limit => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(err) => panic!("acquire the writer lock: {err}"),
        }
    }
}

#[tokio::test]
async fn migrations_apply_once_and_only_what_is_missing() {
    let Some(db) = database().await else { return };
    let mut client = connect(&db.url).await;
    assert_eq!(
        up(&mut client, &MIGRATIONS[..1]).await.expect("version 1"),
        vec![1]
    );

    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");
    assert_eq!(lock.migrate().await.expect("migrate"), vec![2]);
    assert_eq!(
        lock.migrate().await.expect("migrate again"),
        Vec::<i64>::new()
    );

    let versions: Vec<(i64, i64)> = client
        .query(
            "SELECT version_id, count(*) FROM goose_db_version GROUP BY version_id ORDER BY version_id",
            &[],
        )
        .await
        .expect("versions")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(versions, vec![(0, 1), (1, 1), (2, 1)]);

    let column = client
        .query_one(
            "SELECT data_type::text, is_nullable::text, column_default::text FROM information_schema.columns WHERE table_name = 'monitor_state' AND column_name = 'acked_seq'",
            &[],
        )
        .await
        .expect("acked_seq column");
    let column: (String, String, String) = (column.get(0), column.get(1), column.get(2));
    assert_eq!(
        column,
        ("bigint".to_owned(), "NO".to_owned(), "0".to_owned())
    );
}

#[tokio::test]
async fn initialize_creates_the_state_and_the_welcome_only_once() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    assert!(matches!(
        store.cursor().await,
        Err(StoreError::NotInitialized)
    ));

    assert!(
        store
            .initialize(start(), &welcome())
            .await
            .expect("initialize")
    );
    assert!(
        !store
            .initialize(start() + hours(1), &alert("other", start()))
            .await
            .expect("initialize again")
    );

    assert_eq!(store.cursor().await.expect("cursor"), start());
    let alerts = store.alerts_after(0, 100).await.expect("alerts");
    assert_eq!(
        alerts
            .iter()
            .map(|stored| &stored.alert)
            .collect::<Vec<_>>(),
        vec![&welcome()]
    );
}

#[tokio::test]
async fn record_edition_moves_the_cursor_forward_only() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let records = [
        edition(1, start() - hours(1), None),
        edition(
            2,
            start() + hours(48),
            Some(alert("edition-2", start() + hours(48))),
        ),
        edition(3, start() + hours(24), None),
    ];
    for record in &records {
        store.record_edition(record).await.expect("record");
    }

    assert_eq!(store.cursor().await.expect("cursor"), start() + hours(48));
    assert_eq!(seen(&store, &[1, 2, 3]).await, vec![1, 2, 3]);
    let alerts = store.alerts_after(0, 100).await.expect("alerts");
    assert_eq!(ids(&alerts), vec!["welcome", "edition-2"]);
}

#[tokio::test]
async fn a_failed_write_applies_nothing() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    let unstorable = Alert {
        body: "nul \0 byte".to_owned(),
        ..alert("unstorable", start())
    };

    assert!(matches!(
        store.initialize(start(), &unstorable).await,
        Err(StoreError::Database(_))
    ));
    assert!(matches!(
        store.cursor().await,
        Err(StoreError::NotInitialized)
    ));

    assert!(
        store
            .initialize(start(), &welcome())
            .await
            .expect("initialize")
    );
    let failed = edition(7, start() + hours(5), Some(unstorable));
    assert!(matches!(
        store.record_edition(&failed).await,
        Err(StoreError::Database(_))
    ));
    assert_eq!(store.cursor().await.expect("cursor"), start());
    assert_eq!(seen(&store, &[7]).await, Vec::<i64>::new());
    assert_eq!(ids(&all_alerts(&store).await), vec!["welcome"]);
}

async fn wait_for_a_lock_waiter(client: &Client) {
    let started = Instant::now();
    loop {
        let waiting: i64 = client
            .query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
                &[],
            )
            .await
            .expect("lock waiters")
            .get(0);
        if waiting == 1 {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "nobody waited on the state row"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn alert_seqs_follow_commit_order() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let mut outside = connect(&db.url).await;
    let first = outside.transaction().await.expect("begin");
    first
        .execute(
            "INSERT INTO seen_editions (edition_id) VALUES ($1) ON CONFLICT (edition_id) DO NOTHING",
            &[&1_i64],
        )
        .await
        .expect("mark seen");
    first
        .execute(
            "UPDATE monitor_state SET cursor_at = GREATEST(cursor_at, $1)",
            &[&start()],
        )
        .await
        .expect("advance the cursor");

    let second = store.clone();
    let recording = tokio::spawn(async move {
        second
            .record_edition(&edition(2, start(), Some(alert("second", start()))))
            .await
    });
    wait_for_a_lock_waiter(&connect(&db.url).await).await;
    assert!(!recording.is_finished());

    let published = start();
    first
        .execute(
            "INSERT INTO alerts (id, title, link, body, published_at) VALUES ('first', 't', 'l', 'b', $1)",
            &[&published],
        )
        .await
        .expect("insert the first alert");
    first.commit().await.expect("commit the first");
    recording
        .await
        .expect("join")
        .expect("the second record_edition");

    let alerts = all_alerts(&store).await;
    assert_eq!(ids(&alerts), vec!["welcome", "first", "second"]);
    assert!(alerts[1].seq < alerts[2].seq);
}

#[tokio::test]
async fn prune_keeps_the_newest_editions_and_acknowledged_alerts() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let records = [
        edition(1, start(), Some(alert("a", start()))),
        edition(2, start(), Some(alert("b", start()))),
        edition(3, start(), Some(alert("c", start()))),
        edition(4, start(), None),
    ];
    for record in &records {
        store.record_edition(record).await.expect("record");
    }
    let before = all_alerts(&store).await;
    assert_eq!(ids(&before), vec!["welcome", "a", "b", "c"]);

    store.ack_alerts(before[3].seq).await.expect("ack");
    store.prune(2, 2).await.expect("prune");

    assert_eq!(seen(&store, &[1, 2, 3, 4]).await, vec![3, 4]);
    assert_eq!(ids(&all_alerts(&store).await), vec!["b", "c"]);
}

#[tokio::test]
async fn prune_keeps_unacked_alerts() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    for id in 1..=150 {
        let record = edition(id, start(), Some(alert(&format!("alert-{id}"), start())));
        store.record_edition(&record).await.expect("record");
    }
    let before = seqs(&all_alerts(&store).await);
    assert_eq!(before.len(), 151);
    assert!(before.is_sorted());

    store.ack_alerts(before[19]).await.expect("ack");
    store.prune(1000, 100).await.expect("prune");

    assert_eq!(seqs(&all_alerts(&store).await), before[20..].to_vec());
}

#[tokio::test]
async fn alerts_after_returns_increasing_seqs_up_to_the_limit() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let published = start() + TimeDelta::milliseconds(5_400_250);
    let first = alert("first", published);
    let second = alert("second", published + hours(1));
    store
        .record_edition(&edition(1, published, Some(first.clone())))
        .await
        .expect("record");
    store
        .record_edition(&edition(2, published, Some(second.clone())))
        .await
        .expect("record");

    let page = store.alerts_after(0, 2).await.expect("first page");
    assert_eq!(
        page.iter().map(|stored| &stored.alert).collect::<Vec<_>>(),
        vec![&welcome(), &first]
    );
    assert!(page[0].seq < page[1].seq);

    let rest = store.alerts_after(page[0].seq, 100).await.expect("rest");
    assert_eq!(
        rest.iter().map(|stored| &stored.alert).collect::<Vec<_>>(),
        vec![&first, &second]
    );
    assert_eq!(rest[0].seq, page[1].seq);
    assert!(rest[0].seq < rest[1].seq);
}

#[tokio::test]
async fn ack_never_moves_backwards() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    store.ack_alerts(5).await.expect("ack 5");
    store.ack_alerts(3).await.expect("ack 3");

    let client = connect(&db.url).await;
    let acked: i64 = client
        .query_one("SELECT acked_seq FROM monitor_state", &[])
        .await
        .expect("acked_seq")
        .get(0);
    assert_eq!(acked, 5);
}

#[tokio::test]
async fn last_issued_seq_follows_the_alert_sequence() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    assert_eq!(store.last_issued_seq().await.expect("unused sequence"), 0);

    store
        .initialize(start(), &welcome())
        .await
        .expect("initialize");
    store
        .record_edition(&edition(1, start(), Some(alert("a", start()))))
        .await
        .expect("record");
    let issued = all_alerts(&store).await;
    assert_eq!(seqs(&issued).len(), 2);
    let newest = issued[1].seq;
    assert_eq!(
        store.last_issued_seq().await.expect("after inserts"),
        newest
    );

    store.ack_alerts(newest).await.expect("ack");
    store.prune(1000, 0).await.expect("prune");
    assert!(all_alerts(&store).await.is_empty());
    assert_eq!(store.last_issued_seq().await.expect("after prune"), newest);
}

#[tokio::test]
async fn mark_success_records_the_last_success() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    assert_eq!(store.last_success().await.expect("before state"), None);

    store
        .initialize(start(), &welcome())
        .await
        .expect("initialize");
    assert_eq!(store.last_success().await.expect("after initialize"), None);

    store.mark_success(start() + hours(2)).await.expect("mark");
    assert_eq!(
        store.last_success().await.expect("after mark"),
        Some(start() + hours(2))
    );
}

#[tokio::test]
async fn writer_lock_admits_one_holder() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let first = acquire_writer_lock(&db.url).await.expect("first lock");
    assert!(matches!(
        acquire_writer_lock(&db.url).await,
        Err(StoreError::Locked)
    ));

    drop(first);
    acquire_within(&db.url, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn writer_lock_notices_its_connection_ending() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");
    assert_eq!(lock.close_reason(), None);
    let admin = connect(&db.url).await;
    admin
        .execute(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
            &[],
        )
        .await
        .expect("terminate the lock's backend");

    tokio::time::timeout(Duration::from_secs(5), lock.closed())
        .await
        .expect("closed resolves once the connection ends");
    let reason = lock
        .close_reason()
        .expect("a terminated connection has a reason");
    assert!(
        reason.contains("terminating connection due to administrator command"),
        "{reason}"
    );
    assert!(!reason.contains(&db.url), "{reason}");
}

#[tokio::test]
async fn invalid_url_never_echoes_the_address() {
    let url = "postgres://diario:hunter2@[bad";
    let Err(error) = Store::new(url) else {
        panic!("the store accepted {url}")
    };
    assert!(matches!(error, StoreError::InvalidUrl));
    assert_hides_the_login(&error, url);

    let Err(error) = acquire_writer_lock(url).await else {
        panic!("the lock accepted {url}")
    };
    assert!(matches!(error, StoreError::InvalidUrl));
    assert_hides_the_login(&error, url);
}

#[tokio::test]
async fn refused_connections_never_show_the_password() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let url = format!("postgres://diario:{PASSWORD}@127.0.0.1:{port}/diario");

    let Err(error) = acquire_writer_lock(&url).await else {
        panic!("a closed port gave a lock")
    };
    assert!(matches!(error, StoreError::Database(_)), "{error:?}");
    assert_hides_the_login(&error, &url);

    let store = Store::new(&url).expect("store");
    let error = store.cursor().await.expect_err("a closed port answered");
    assert!(matches!(error, StoreError::Pool(_)), "{error:?}");
    assert_hides_the_login(&error, &url);
}

#[tokio::test]
async fn rejected_logins_never_show_the_password() {
    let Some(db) = database().await else { return };
    let url = with_login(&db.url, &format!("intruder:{PASSWORD}"));

    let Err(error) = acquire_writer_lock(&url).await else {
        panic!("an unknown role got a lock")
    };
    assert!(matches!(error, StoreError::Database(_)), "{error:?}");
    assert_hides_the_login(&error, &url);

    let store = Store::new(&url).expect("store");
    let error = store
        .cursor()
        .await
        .expect_err("an unknown role was let in");
    assert!(matches!(error, StoreError::Pool(_)), "{error:?}");
    assert_hides_the_login(&error, &url);
}

#[tokio::test]
async fn silent_servers_time_out_without_showing_the_address() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let silent = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let url = format!("postgres://diario:{PASSWORD}@{address}/diario");
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let (lock, cursor) = tokio::join!(acquire_writer_lock(&url), store.cursor());
    let elapsed = started.elapsed();
    silent.abort();

    assert!(elapsed < Duration::from_secs(13), "took {elapsed:?}");
    let Err(lock) = lock else {
        panic!("a silent server gave a lock")
    };
    let cursor = cursor.expect_err("a silent server answered");
    for error in [&lock, &cursor] {
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_hides_the_login(error, &url);
        let printed = everything_printed(error);
        assert!(!printed.contains(&address.to_string()), "{printed}");
    }
}
