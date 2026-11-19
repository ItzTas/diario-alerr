mod support;

use std::error::Error;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Alert;
use diario_alerr::migration::{DROP_GO_TABLES, GOOSE_SCHEMA};
use diario_alerr::store::{
    EditionRecord, LOCK_CHECK_EVERY, LOCK_CHECK_LIMIT, SequencedAlert, Store, StoreError,
    WriterLock, acquire_writer_lock,
};
use reqwest::Url;
use tokio::net::TcpListener;
use tokio_postgres::{Client, NoTls};

use support::proxy::PausingProxy;
use support::schema::{goose_era_database, rows, seed_rows, table_names};
use support::{
    database, end_the_writer_locks_connection, migrated_database, next_signal, pending_signal,
};

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

async fn refuse_alerts_at_commit(url: &str) {
    connect(url)
        .await
        .batch_execute(
            "CREATE FUNCTION refuse_alerts() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN RAISE EXCEPTION 'refused at commit'; END $$;
             CREATE CONSTRAINT TRIGGER refuse_at_commit AFTER INSERT ON alerts
             DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION refuse_alerts();",
        )
        .await
        .expect("create the trigger that refuses alerts at commit");
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

fn host_and_port(url: &str) -> String {
    let url = Url::parse(url).expect("a postgres:// URL");
    let host = url.host_str().expect("a host in the URL");
    let port = url.port().expect("a port in the URL");
    format!("{host}:{port}")
}

fn with_query(url: &str, parameter: &str) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{parameter}")
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
async fn migrate_applies_the_migrations_once() {
    let Some(db) = database().await else { return };
    let lock = acquire_writer_lock(&db.url).await.expect("lock");

    assert_eq!(
        lock.migrate().await.expect("migrate"),
        [GOOSE_SCHEMA, DROP_GO_TABLES]
    );
    assert_eq!(
        lock.migrate().await.expect("migrate again"),
        Vec::<String>::new()
    );

    assert_eq!(
        table_names(&connect(&db.url).await).await,
        [
            "alerts",
            "monitor_state",
            "seaql_migrations",
            "seen_editions"
        ]
    );
}

#[tokio::test]
async fn a_migration_that_fails_leaves_the_database_as_it_was() {
    let Some(db) = database().await else { return };
    let admin = connect(&db.url).await;
    goose_era_database(&admin, &[1, 2], &[1, 2]).await;
    seed_rows(&admin, true).await;
    admin
        .batch_execute("CREATE VIEW deliveries AS SELECT * FROM alert_deliveries")
        .await
        .expect("make a view depend on a Go-era table");
    let tables = table_names(&admin).await;
    let goose_versions = rows(&admin, "goose_db_version").await;
    let alerts = rows(&admin, "alerts").await;
    let lock = acquire_writer_lock(&db.url).await.expect("lock");

    let error = lock.migrate().await.expect_err("the drop was refused");

    assert!(matches!(error, StoreError::Database(_)), "{error:?}");
    assert_eq!(table_names(&admin).await, tables);
    assert_eq!(rows(&admin, "goose_db_version").await, goose_versions);
    assert_eq!(rows(&admin, "alerts").await, alerts);
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
async fn record_edition_returns_the_seq_of_the_alert_it_inserted() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;

    let inserted = store
        .record_edition(&edition(
            10,
            start() + hours(1),
            Some(alert("edition-10", start() + hours(1))),
        ))
        .await
        .expect("record edition 10");
    let without_alert = store
        .record_edition(&edition(11, start() + hours(2), None))
        .await
        .expect("record edition 11");
    let repeated = store
        .record_edition(&edition(
            12,
            start() + hours(3),
            Some(alert("edition-10", start() + hours(3))),
        ))
        .await
        .expect("record edition 12");

    let alerts = all_alerts(&store).await;
    assert_eq!(ids(&alerts), ["welcome", "edition-10"]);
    assert_eq!(inserted, Some(alerts[1].seq));
    assert_eq!(without_alert, None);
    assert_eq!(repeated, None);
    assert_eq!(seen(&store, &[10, 11, 12]).await, [10, 11, 12]);
}

#[tokio::test]
async fn record_edition_signals_the_seq_it_committed() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let mut signals = store.notifier().subscribe();
    assert_eq!(signals.latest_seq(), 0);

    let inserted = store
        .record_edition(&edition(
            1,
            start(),
            Some(alert("edition-1", start() + hours(1))),
        ))
        .await
        .expect("record edition 1")
        .expect("edition 1 inserted its alert");
    assert_eq!(next_signal(&mut signals).await, inserted);

    store
        .record_edition(&edition(2, start(), None))
        .await
        .expect("record edition 2");
    store
        .record_edition(&edition(
            3,
            start(),
            Some(alert("edition-1", start() + hours(2))),
        ))
        .await
        .expect("record edition 3");
    assert_eq!(pending_signal(&mut signals).await, None);
}

#[tokio::test]
async fn a_record_refused_at_commit_signals_nothing() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    refuse_alerts_at_commit(&db.url).await;
    let mut signals = store.notifier().subscribe();
    let before = signals.latest_seq();

    let refused = store
        .record_edition(&edition(
            1,
            start(),
            Some(alert("edition-1", start() + hours(1))),
        ))
        .await;

    assert!(
        matches!(refused, Err(StoreError::Database(_))),
        "{refused:?}"
    );
    assert_eq!(pending_signal(&mut signals).await, None);
    assert_eq!(signals.latest_seq(), before);
    assert_eq!(ids(&all_alerts(&store).await), ["welcome"]);
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
async fn ack_writes_the_state_row_only_when_it_advances() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    let client = connect(&db.url).await;
    let state_row = async || -> (String, i64) {
        let row = client
            .query_one("SELECT ctid::text, acked_seq FROM monitor_state", &[])
            .await
            .expect("state row");
        (row.get(0), row.get(1))
    };

    store.ack_alerts(5).await.expect("ack 5");
    let (version, acked) = state_row().await;
    assert_eq!(acked, 5);

    store.ack_alerts(5).await.expect("ack 5 again");
    store.ack_alerts(3).await.expect("ack 3");
    assert_eq!(state_row().await, (version.clone(), 5));

    store.ack_alerts(7).await.expect("ack 7");
    let (advanced, acked) = state_row().await;
    assert_eq!(acked, 7);
    assert_ne!(advanced, version);
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
async fn newest_alert_seq_is_the_seq_of_the_newest_stored_alert() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = Store::new(&db.url).expect("store");
    assert_eq!(store.newest_alert_seq().await.expect("no alerts"), 0);

    store
        .initialize(start(), &welcome())
        .await
        .expect("initialize");
    store
        .record_edition(&edition(1, start(), Some(alert("a", start()))))
        .await
        .expect("record");
    store
        .record_edition(&edition(2, start(), Some(alert("a", start()))))
        .await
        .expect("record the same alert id");

    let stored = all_alerts(&store).await;
    assert_eq!(ids(&stored), ["welcome", "a"]);
    assert_eq!(
        store.last_issued_seq().await.expect("last issued"),
        stored[1].seq + 1
    );
    assert_eq!(
        store.newest_alert_seq().await.expect("newest alert"),
        stored[1].seq
    );
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
async fn pooled_connections_are_tested_before_use() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = initialized(&db.url).await;
    assert_eq!(store.cursor().await.expect("first read"), start());
    let admin = connect(&db.url).await;
    let terminated: Vec<bool> = admin
        .query(
            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
            &[],
        )
        .await
        .expect("terminate the pooled connections' backends")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(
        !terminated.is_empty() && terminated.iter().all(|ended| *ended),
        "{terminated:?}"
    );
    let started = Instant::now();
    loop {
        let left: i64 = admin
            .query_one(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid()",
                &[],
            )
            .await
            .expect("count the other backends")
            .get(0);
        if left == 0 {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the terminated backends were still running after 5 seconds"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let cursor = store
        .cursor()
        .await
        .expect("a read after the server ended the pooled connections");

    assert_eq!(cursor, start());
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
        reason.starts_with("the lock's connection failed: "),
        "{reason}"
    );
    assert!(!reason.contains(&db.url), "{reason}");
    let address = host_and_port(&db.url);
    assert!(db.url.contains(&address), "{address} is not in the URL");
    assert!(!reason.contains(&address), "{reason}");
}

#[tokio::test]
async fn writer_lock_notices_a_connection_that_stops_answering() {
    let Some(db) = database().await else { return };
    let (proxy, url) = PausingProxy::start(&db.url).await;
    let mut lock = acquire_writer_lock(&url)
        .await
        .expect("lock through the proxy");

    proxy.pause();
    let limit = LOCK_CHECK_LIMIT + LOCK_CHECK_EVERY + Duration::from_secs(2);
    tokio::time::timeout(limit, lock.closed())
        .await
        .expect("closed resolves once the connection stops answering");

    assert_eq!(
        lock.close_reason().as_deref(),
        Some("the lock's connection did not answer within 10s")
    );
}

#[tokio::test]
async fn writer_lock_notices_an_idle_connection_that_stops_answering() {
    let Some(db) = database().await else { return };
    let (proxy, url) = PausingProxy::start(&db.url).await;
    let mut lock = acquire_writer_lock(&url)
        .await
        .expect("lock through the proxy");
    let between_checks = 2 * LOCK_CHECK_EVERY + LOCK_CHECK_EVERY / 4;
    let settled = tokio::time::timeout(between_checks, lock.closed()).await;
    assert!(
        settled.is_err(),
        "closed resolved: {:?}",
        lock.close_reason()
    );

    proxy.pause();
    let limit = LOCK_CHECK_LIMIT + LOCK_CHECK_EVERY + Duration::from_secs(2);
    tokio::time::timeout(limit, lock.closed())
        .await
        .expect("closed resolves once the idle connection stops answering");

    assert_eq!(
        lock.close_reason().as_deref(),
        Some("the lock's connection did not answer within 10s")
    );
}

#[tokio::test]
async fn writer_lock_stays_while_its_connection_answers() {
    let Some(db) = database().await else { return };
    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");

    let waited = tokio::time::timeout(3 * LOCK_CHECK_EVERY, lock.closed()).await;

    assert!(
        waited.is_err(),
        "closed resolved: {:?}",
        lock.close_reason()
    );
    assert_eq!(lock.close_reason(), None);
}

#[tokio::test]
async fn a_lock_whose_connection_ended_refuses_to_migrate() {
    let Some(db) = database().await else { return };
    let lock = acquire_writer_lock(&db.url).await.expect("lock");
    let admin = connect(&db.url).await;
    end_the_writer_locks_connection(&admin).await;

    let error = lock
        .migrate()
        .await
        .expect_err("a lock without its connection migrated");

    let StoreError::LockLost(reason) = &error else {
        panic!("{error:?}");
    };
    assert!(
        reason.starts_with("the lock's connection failed: "),
        "{reason}"
    );
    assert!(
        error
            .to_string()
            .starts_with("the database lock was lost: the lock's connection failed: "),
        "{error}"
    );
    assert_hides_the_login(&error, &db.url);
    assert_eq!(table_names(&admin).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_lock_on_a_replaced_connection_neither_migrates_nor_stays() {
    let Some(db) = database().await else { return };
    let mut lock = acquire_writer_lock(&db.url).await.expect("lock");
    let admin = connect(&db.url).await;
    end_the_writer_locks_connection(&admin).await;
    lock.migrate()
        .await
        .expect_err("the ended connection let a migration through");

    let error = lock
        .migrate()
        .await
        .expect_err("a connection without the lock migrated");

    assert!(matches!(error, StoreError::LockLost(_)), "{error:?}");
    assert_eq!(
        error.to_string(),
        "the database lock was lost: the lock's connection was replaced"
    );
    assert_eq!(table_names(&admin).await, Vec::<String>::new());
    tokio::time::timeout(Duration::from_secs(5), lock.closed())
        .await
        .expect("closed resolves on a replaced connection");
    assert_eq!(
        lock.close_reason().as_deref(),
        Some("the lock's connection was replaced")
    );
}

#[tokio::test]
async fn invalid_url_never_echoes_the_address() {
    for url in [
        "postgres://diario:hunter2@[bad",
        "postgres://diario:hunter2@h1,h2/diario",
    ] {
        let Err(error) = Store::new(url) else {
            panic!("the store accepted {url}")
        };
        assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
        assert_hides_the_login(&error, url);

        let Err(error) = acquire_writer_lock(url).await else {
            panic!("the lock accepted {url}")
        };
        assert!(matches!(error, StoreError::InvalidUrl(_)), "{url}");
        assert_hides_the_login(&error, url);
    }
}

#[tokio::test]
async fn refused_connections_never_show_the_password() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("address").port();
    drop(listener);
    let url = format!("postgres://diario:{PASSWORD}@127.0.0.1:{port}/diario");
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let (lock, cursor) = tokio::join!(acquire_writer_lock(&url), store.cursor());
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(13), "took {elapsed:?}");
    let Err(lock) = lock else {
        panic!("a closed port gave a lock")
    };
    let cursor = cursor.expect_err("a closed port answered");
    assert_eq!(
        lock.to_string(),
        "connecting to the database for the writer lock timed out"
    );
    assert_eq!(
        cursor.to_string(),
        "getting a database connection timed out"
    );
    for error in [&lock, &cursor] {
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_hides_the_login(error, &url);
    }
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
    assert!(matches!(error, StoreError::Database(_)), "{error:?}");
    assert_hides_the_login(&error, &url);
}

#[tokio::test]
async fn a_url_that_requires_tls_is_refused_at_once() {
    let Some(db) = database().await else { return };
    let url = with_query(
        &with_login(&db.url, &format!("diario:{PASSWORD}")),
        "sslmode=require",
    );
    let store = Store::new(&url).expect("store");

    let started = Instant::now();
    let lock = acquire_writer_lock(&url).await;
    let cursor = store.cursor().await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
    let Err(lock) = lock else {
        panic!("an address that requires TLS gave a lock")
    };
    let cursor = cursor.expect_err("an address that requires TLS was let in");
    for error in [&lock, &cursor] {
        assert!(matches!(error, StoreError::Database(_)), "{error:?}");
        let printed = everything_printed(error);
        assert!(printed.contains("TLS"), "{printed}");
        assert_hides_the_login(error, &url);
    }
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

#[tokio::test]
async fn test_databases_are_reached_over_tcp() {
    let Some(database) = database().await else {
        return;
    };
    let (client, connection) = tokio_postgres::connect(&database.url, NoTls)
        .await
        .expect("connect to the test database");
    tokio::spawn(connection);
    let row = client
        .query_one("SELECT inet_server_addr() IS NOT NULL", &[])
        .await
        .expect("read the server address");
    assert!(row.get::<_, bool>(0));
}
