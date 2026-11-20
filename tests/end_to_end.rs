mod support;

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Alert;
use diario_alerr::gazette::{Client, HTTP_TIMEOUT};
use diario_alerr::migration::{DROP_GO_TABLES, GOOSE_SCHEMA};
use diario_alerr::monitor::{CycleOutcome, Monitor, MonitorOptions};
use diario_alerr::store::{EditionRecord, SequencedAlert, Store};
use monitor_hub_sdk::proto::{
    CheckRequest, CheckResponse, Event, EventsRequest, EventsResponse, WatchRequest, WatchResponse,
};
use tokio::time::{Instant, timeout_at};
use tokio_postgres::NoTls;
use tonic::client::Grpc;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint};
use tonic::{Request, Streaming};
use tonic_prost::ProstCodec;

use support::gazette_site::{GazetteSite, SiteEdition};
use support::process::spawn_monitor;
use support::schema::{goose_era_database, seed_rows, table_names};
use support::{
    SIGNAL_WAIT, admin, database, fixed_clock, migrated_database, require_pdftotext, utc,
};

const CHECK_ATTEMPTS: usize = 5;
const ANNOUNCEMENT_WAIT: Duration = Duration::from_secs(20);

fn now() -> DateTime<Utc> {
    utc("2026-11-09T12:00:00Z")
}

fn site_edition(id: i64, published: &str, title: &str, pages: &[&str]) -> SiteEdition {
    SiteEdition {
        id,
        published: utc(published),
        title: title.to_owned(),
        pages: pages.iter().map(|page| (*page).to_owned()).collect(),
    }
}

async fn prepared(
    url: &str,
    site: &GazetteSite,
    term: &str,
    now: DateTime<Utc>,
) -> Monitor<Client> {
    let options = MonitorOptions {
        terms: vec![term.to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(now),
    };
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).expect("client");
    let monitor = Monitor::new(Store::new(url).expect("store"), client, options);
    monitor.prepare().await.expect("prepare");
    monitor
}

async fn alerts(monitor: &Monitor<Client>) -> Vec<SequencedAlert> {
    alerts_in(monitor.store()).await
}

async fn alerts_in(store: &Store) -> Vec<SequencedAlert> {
    store.alerts_after(0, 1000).await.expect("alerts")
}

#[tokio::test]
async fn monitor_alerts_when_a_name_appears() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![
        site_edition(501, "2026-11-02T10:00:00Z", "ED 501", &["Nada aqui"]),
        site_edition(
            502,
            "2026-11-03T10:00:00Z",
            "ED 502",
            &["Sumario", "Nomeacao de PAULO SANTOS"],
        ),
    ])
    .await;
    let monitor = prepared(&db.url, &site, "Paulo Santos", now()).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

    let alerts = alerts(&monitor).await;
    let titles: Vec<&str> = alerts
        .iter()
        .map(|stored| stored.alert.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "ALE/RR Official Gazette monitor started",
            "Your name appeared in the ALE/RR Official Gazette: ED 502",
        ]
    );
    assert_eq!(
        alerts[1].alert.link,
        format!("{}#page=2", site.pdf_url(502))
    );
    assert!(
        alerts[1].alert.body.contains("Paulo Santos: p. 2"),
        "{:?}",
        alerts[1].alert.body
    );
}

#[tokio::test]
async fn monitor_remembers_what_it_saw_across_restarts() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![site_edition(
        601,
        "2026-11-02T10:00:00Z",
        "ED 601",
        &["PAULO SANTOS"],
    )])
    .await;
    let first = prepared(&db.url, &site, "PAULO SANTOS", now()).await;
    assert_eq!(first.run_cycle().await, CycleOutcome::Complete);

    site.publish(site_edition(
        602,
        "2026-11-04T10:00:00Z",
        "ED 602",
        &["PAULO SANTOS aparece de novo"],
    ));
    let restarted = prepared(&db.url, &site, "PAULO SANTOS", now() + TimeDelta::hours(1)).await;
    assert_eq!(restarted.run_cycle().await, CycleOutcome::Complete);

    let ids: Vec<String> = alerts(&restarted)
        .await
        .into_iter()
        .map(|stored| stored.alert.id)
        .collect();
    assert_eq!(ids, ["welcome", "edition-601", "edition-602"]);
    let last_query = site.queries().pop().expect("a listing query");
    assert!(
        last_query.contains("after=2026-11-01T10%3A00%3A00"),
        "{last_query}"
    );
}

fn alert(id: &str, published: DateTime<Utc>) -> Alert {
    Alert {
        id: id.to_owned(),
        title: format!("title of {id}"),
        link: format!("https://x/{id}"),
        body: "body".to_owned(),
        published,
    }
}

async fn wait_for_alert(url: &str, id: &str) {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let found = client
            .query_opt("SELECT 1 FROM alerts WHERE id = $1", &[&id])
            .await
            .expect("look for the alert");
        if found.is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "{id} was never recorded");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn grpc(socket: &Path) -> Grpc<Channel> {
    let channel = Endpoint::from_shared(format!("unix:{}", socket.display()))
        .expect("a socket endpoint")
        .connect()
        .await
        .expect("connect to the monitor");
    let mut grpc = Grpc::new(channel);
    grpc.ready().await.expect("the channel is ready");
    grpc
}

async fn events(socket: &Path, after: i64, limit: i32) -> Vec<Event> {
    grpc(socket)
        .await
        .unary(
            Request::new(EventsRequest { after, limit }),
            PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Events"),
            ProstCodec::<EventsRequest, EventsResponse>::default(),
        )
        .await
        .expect("events")
        .into_inner()
        .events
}

async fn watch(socket: &Path) -> Streaming<WatchResponse> {
    grpc(socket)
        .await
        .server_streaming(
            Request::new(WatchRequest {}),
            PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Watch"),
            ProstCodec::<WatchRequest, WatchResponse>::default(),
        )
        .await
        .expect("open Watch")
        .into_inner()
}

async fn check(socket: &Path) -> CheckResponse {
    grpc(socket)
        .await
        .unary(
            Request::new(CheckRequest {}),
            PathAndQuery::from_static("/monitorhub.monitor.v1.Monitor/Check"),
            ProstCodec::<CheckRequest, CheckResponse>::default(),
        )
        .await
        .expect("check")
        .into_inner()
}

async fn next_watch_signal(stream: &mut Streaming<WatchResponse>, deadline: Instant) -> i64 {
    let limit = deadline.min(Instant::now() + SIGNAL_WAIT);
    timeout_at(limit, stream.message())
        .await
        .expect("a Watch signal in time")
        .expect("an OK Watch signal")
        .expect("the Watch call stays open")
        .latest_seq
}

async fn signal_after_a_cycle(
    socket: &Path,
    stream: &mut Streaming<WatchResponse>,
    seeded: i64,
) -> i64 {
    let deadline = Instant::now() + ANNOUNCEMENT_WAIT;
    for _ in 0..CHECK_ATTEMPTS {
        if check(socket).await.accepted {
            return signal_above(stream, seeded, deadline).await;
        }
        let seq = next_watch_signal(stream, deadline).await;
        if seq != seeded {
            return seq;
        }
    }
    panic!("the monitor refused {CHECK_ATTEMPTS} checks in a row");
}

async fn signal_above(
    stream: &mut Streaming<WatchResponse>,
    seeded: i64,
    deadline: Instant,
) -> i64 {
    loop {
        let seq = next_watch_signal(stream, deadline).await;
        if seq != seeded {
            return seq;
        }
    }
}

async fn insert_again(url: &str, id: &str) {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
        .execute(
            "INSERT INTO alerts (id, title, link, body, published_at)
             VALUES ($1, 'title', 'https://x/again', 'body', now())
             ON CONFLICT (id) DO NOTHING",
            &[&id],
        )
        .await
        .expect("insert an alert id that exists");
}

async fn seeded_store(url: &str) -> Store {
    let cursor = utc("2026-11-02T10:00:00Z");
    let store = Store::new(url).expect("store");
    store
        .initialize(cursor, &alert("welcome", cursor))
        .await
        .expect("seed the state");
    store
        .record_edition(&EditionRecord {
            id: 701,
            published: cursor,
            alert: Some(alert("edition-701", cursor)),
        })
        .await
        .expect("seed an older alert");
    store
}

fn ids(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.id.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_hubs_first_read_leaves_the_first_cycle_for_the_next_one() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    seeded_store(&db.url).await;
    let site = GazetteSite::start(vec![site_edition(
        702,
        "2026-11-03T10:00:00Z",
        "ED 702",
        &["Nomeacao de PAULO SANTOS"],
    )])
    .await;
    let sockets = tempfile::tempdir().expect("socket directory");
    let spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TMPDIR", sockets.path().to_str().expect("a UTF-8 path")),
    ]);

    wait_for_alert(&db.url, "edition-702").await;
    let first = events(&spawned.socket, 0, 500).await;
    let last = first.last().expect("history").seq;
    let second = events(&spawned.socket, last, 500).await;

    assert_eq!(ids(&first), ["welcome", "edition-701"]);
    assert_eq!(ids(&second), ["edition-702"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_signals_at_once_and_after_each_recorded_alert() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let store = seeded_store(&db.url).await;
    insert_again(&db.url, "edition-701").await;
    let stored = alerts_in(&store).await;
    let seeded = stored.last().expect("the seeded alerts").seq;
    let issued = store.last_issued_seq().await.expect("last issued seq");
    assert_eq!(issued, seeded + 1);
    let site = GazetteSite::start(vec![]).await;
    let sockets = tempfile::tempdir().expect("socket directory");
    let spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TMPDIR", sockets.path().to_str().expect("a UTF-8 path")),
    ]);

    let mut stream = watch(&spawned.socket).await;
    let first = next_watch_signal(&mut stream, Instant::now() + SIGNAL_WAIT).await;
    assert_eq!(first, seeded);

    site.publish(site_edition(
        702,
        "2026-11-03T10:00:00Z",
        "ED 702",
        &["Nomeacao de PAULO SANTOS"],
    ));
    let announced = signal_after_a_cycle(&spawned.socket, &mut stream, seeded).await;

    let recorded: Vec<(String, i64)> = store
        .alerts_after(seeded, 10)
        .await
        .expect("alerts")
        .into_iter()
        .map(|stored| (stored.alert.id, stored.seq))
        .collect();
    assert_eq!(recorded, [("edition-702".to_owned(), announced)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_database_left_by_2_1_keeps_its_alerts_and_positions() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = database().await else {
        return;
    };
    let admin = admin(&db.url).await;
    goose_era_database(&admin, &[1, 2], &[1, 2]).await;
    seed_rows(&admin, true).await;
    let seeded: Vec<(i64, String)> = admin
        .query("SELECT seq, id FROM alerts ORDER BY seq", &[])
        .await
        .expect("the seeded alerts")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(seeded.len(), 3, "{seeded:?}");
    let site = GazetteSite::start(vec![]).await;
    let sockets = tempfile::tempdir().expect("socket directory");
    let spawned = spawn_monitor(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "PAULO SANTOS"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TMPDIR", sockets.path().to_str().expect("a UTF-8 path")),
    ]);

    let read: Vec<(i64, String)> = events(&spawned.socket, 0, 0)
        .await
        .into_iter()
        .map(|event| (event.seq, event.id))
        .collect();

    assert_eq!(read, seeded);
    let applied: Vec<String> = admin
        .query("SELECT version FROM seaql_migrations ORDER BY version", &[])
        .await
        .expect("seaql_migrations")
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(applied, [GOOSE_SCHEMA, DROP_GO_TABLES]);
    assert_eq!(
        table_names(&admin).await,
        [
            "alerts",
            "monitor_state",
            "seaql_migrations",
            "seen_editions"
        ]
    );
}
