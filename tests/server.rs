mod support;

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Alert;
use diario_alerr::monitor::{Monitor, MonitorOptions};
use diario_alerr::scheduler::Scheduler;
use diario_alerr::server::DiarioPlugin;
use diario_alerr::store::{EditionRecord, Store};
use monitor_hub_plugin::proto::plugin_server::Plugin;
use monitor_hub_plugin::proto::{
    CheckRequest, CheckResponse, DescribeRequest, Event, EventKind, EventsRequest, Severity, State,
    StatusRequest, StatusResponse,
};
use prost_types::Timestamp;
use tokio::time::Instant;
use tokio_postgres::{Client, NoTls};
use tonic::{Code, Request};

use support::fake_gazette::FakeGazette;
use support::{eventually, fixed_clock, migrated_database, utc};

const PATIENCE: Duration = Duration::from_secs(5);
const UNREACHABLE_DATABASE: &str = "postgres://diario:hunter2@127.0.0.1:1/diario";

fn now() -> DateTime<Utc> {
    utc("2026-11-09T12:00:00Z")
}

fn options() -> MonitorOptions {
    MonitorOptions {
        terms: vec!["TALES SABINI".to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(now()),
    }
}

fn timestamp(at: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: at.timestamp(),
        nanos: i32::try_from(at.timestamp_subsec_nanos()).expect("nanos"),
    }
}

fn unprepared(url: &str, fake: FakeGazette) -> Arc<Scheduler<FakeGazette>> {
    let monitor = Monitor::new(Store::new(url).expect("store"), fake, options());
    Scheduler::new(Arc::new(monitor))
}

async fn prepared(url: &str, fake: FakeGazette) -> Arc<Scheduler<FakeGazette>> {
    let scheduler = unprepared(url, fake);
    scheduler.monitor().prepare().await.expect("prepare");
    scheduler
}

fn plugin(scheduler: &Arc<Scheduler<FakeGazette>>) -> DiarioPlugin<FakeGazette> {
    DiarioPlugin::new(Arc::clone(scheduler))
}

async fn events(
    plugin: &DiarioPlugin<FakeGazette>,
    after: i64,
    limit: i32,
) -> Result<Vec<Event>, tonic::Status> {
    plugin
        .events(Request::new(EventsRequest { after, limit }))
        .await
        .map(|response| response.into_inner().events)
}

async fn page(plugin: &DiarioPlugin<FakeGazette>, after: i64, limit: i32) -> Vec<Event> {
    events(plugin, after, limit).await.expect("events")
}

async fn status(plugin: &DiarioPlugin<FakeGazette>) -> StatusResponse {
    plugin
        .status(Request::new(StatusRequest {}))
        .await
        .expect("status")
        .into_inner()
}

async fn check(plugin: &DiarioPlugin<FakeGazette>) -> CheckResponse {
    plugin
        .check(Request::new(CheckRequest {}))
        .await
        .expect("check")
        .into_inner()
}

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

async fn acked(url: &str) -> i64 {
    connect(url)
        .await
        .query_one("SELECT acked_seq FROM monitor_state", &[])
        .await
        .expect("acked_seq")
        .get(0)
}

async fn add_alerts(url: &str, count: i32) {
    connect(url)
        .await
        .execute(
            "INSERT INTO alerts (id, title, link, body, published_at)
             SELECT 'edition-' || n, 'title ' || n, 'https://x/' || n, 'body', $1
             FROM generate_series(1, $2::int) AS n",
            &[&now(), &count],
        )
        .await
        .expect("insert alerts");
}

fn seqs(events: &[Event]) -> Vec<i64> {
    events.iter().map(|event| event.seq).collect()
}

fn assert_increasing(seqs: &[i64]) {
    assert!(seqs.windows(2).all(|pair| pair[0] < pair[1]), "{seqs:?}");
}

#[tokio::test]
async fn describe_names_the_plugin_and_its_kinds() {
    let scheduler = unprepared(UNREACHABLE_DATABASE, FakeGazette::new(vec![]));

    let described = plugin(&scheduler)
        .describe(Request::new(DescribeRequest {}))
        .await
        .expect("describe")
        .into_inner();

    assert_eq!(described.name, "diario");
    assert_eq!(described.display_name, "ALE/RR Official Gazette");
    assert_eq!(described.version, "2.0.0");
    assert_eq!(
        described.kinds,
        [
            EventKind {
                id: "term_found".to_owned(),
                description: "A watched term appears in a new edition".to_owned(),
            },
            EventKind {
                id: "welcome".to_owned(),
                description: "Monitoring started".to_owned(),
            },
        ]
    );
}

#[tokio::test]
async fn events_maps_alerts_and_acknowledges() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakeGazette::new(vec![])).await;
    let published = utc("2026-11-06T20:23:18.5Z");
    let found = Alert {
        id: "edition-10".to_owned(),
        title: "Your name appeared: ED 10".to_owned(),
        link: "https://x/10.pdf#page=2".to_owned(),
        body: "TALES SABINI: p. 2".to_owned(),
        published,
    };
    scheduler
        .monitor()
        .store()
        .record_edition(&EditionRecord {
            id: 10,
            published,
            alert: Some(found.clone()),
        })
        .await
        .expect("record");
    let plugin = plugin(&scheduler);

    let first = page(&plugin, 0, 0).await;

    let [welcome, edition] = &first[..] else {
        panic!("expected two events: {first:?}");
    };
    assert_eq!(welcome.id, "welcome");
    assert_eq!(welcome.kind, "welcome");
    assert_eq!(welcome.severity, i32::from(Severity::Info));
    assert_eq!(welcome.url, "https://diarios.al.rr.leg.br");
    assert_eq!(welcome.occurred_at, Some(timestamp(now())));
    assert_eq!(welcome.title, "ALE/RR Official Gazette monitor started");
    assert_eq!(welcome.payload, None);
    assert_eq!(
        edition,
        &Event {
            seq: edition.seq,
            id: found.id,
            kind: "term_found".to_owned(),
            severity: Severity::Urgent.into(),
            occurred_at: Some(timestamp(published)),
            title: found.title,
            body: found.body,
            url: found.link,
            payload: None,
        }
    );
    assert!(welcome.seq < edition.seq);
    assert_eq!(acked(&db.url).await, 0);

    assert_eq!(page(&plugin, welcome.seq, 0).await, first[1..]);
    assert_eq!(acked(&db.url).await, welcome.seq);

    assert_eq!(page(&plugin, 0, 0).await, first);
    assert_eq!(acked(&db.url).await, welcome.seq);
}

#[tokio::test]
async fn events_applies_the_limit() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakeGazette::new(vec![])).await;
    add_alerts(&db.url, 149).await;
    let plugin = plugin(&scheduler);

    let default = page(&plugin, 0, 0).await;
    assert_eq!(default.len(), 100);
    assert_increasing(&seqs(&default));
    assert_eq!(default[0].id, "welcome");

    let seven = page(&plugin, 0, 7).await;
    assert_eq!(seqs(&seven), seqs(&default)[..7]);

    assert_eq!(page(&plugin, 0, -3).await.len(), 100);
}

#[tokio::test]
async fn events_never_returns_more_than_500() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakeGazette::new(vec![])).await;
    add_alerts(&db.url, 600).await;
    let plugin = plugin(&scheduler);

    let full = page(&plugin, 0, 1000).await;
    assert_eq!(full.len(), 500);
    assert_increasing(&seqs(&full));
    assert_eq!(page(&plugin, 0, i32::MAX).await, full);
    assert_eq!(page(&plugin, 0, 500).await, full);

    let rest = page(&plugin, full[499].seq, 500).await;
    assert_eq!(rest.len(), 101);
    assert!(rest[0].seq > full[499].seq);
}

#[tokio::test]
async fn events_after_the_newest_seq_answers_empty_and_keeps_the_alerts() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let scheduler = prepared(&db.url, FakeGazette::new(vec![])).await;
    add_alerts(&db.url, 2).await;
    let store = scheduler.monitor().store();
    let newest = store.last_issued_seq().await.expect("newest seq");
    let plugin = plugin(&scheduler);

    assert_eq!(page(&plugin, newest + 50, 0).await, []);
    assert_eq!(acked(&db.url).await, 0);

    store.prune(1000, 0).await.expect("prune");
    let kept = store.alerts_after(0, 100).await.expect("alerts");
    assert_eq!(kept.len(), 3);
    assert_eq!(page(&plugin, newest, 0).await, []);
    assert_eq!(acked(&db.url).await, newest);
}

#[tokio::test]
async fn database_failures_never_show_the_password() {
    let scheduler = unprepared(UNREACHABLE_DATABASE, FakeGazette::new(vec![]));
    let plugin = plugin(&scheduler);

    let failure = events(&plugin, 0, 0)
        .await
        .expect_err("an unreachable database gave events");
    assert_eq!(failure.code(), Code::Unavailable);
    assert_eq!(failure.message(), "the database is unavailable");

    let status = status(&plugin).await;
    assert_eq!(status.state, i32::from(State::Failing));
    assert_eq!(status.message, "the database is unavailable");
    assert_eq!(status.last_success_at, None);

    let accepted = check(&plugin).await;
    assert!(accepted.accepted);
    for shown in [format!("{failure:?}"), format!("{status:?}")] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("127.0.0.1"), "{shown}");
    }
}

#[tokio::test]
async fn a_cycle_stopped_by_the_database_shows_the_fixed_phrase() {
    let scheduler = unprepared(UNREACHABLE_DATABASE, FakeGazette::new(vec![]));
    let plugin = plugin(&scheduler);
    let cycles = tokio::spawn(Arc::clone(&scheduler).run());

    eventually(Instant::now() + PATIENCE, "the first cycle fails", || {
        let state = scheduler.state();
        !state.running && state.problem.is_some()
    })
    .await;

    let status = status(&plugin).await;
    assert_eq!(status.state, i32::from(State::Failing));
    assert_eq!(
        status.message,
        "could not list editions: the database is unavailable"
    );
    cycles.abort();
}

#[tokio::test]
async fn status_follows_the_cycle() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    fake.gated();
    let scheduler = prepared(&db.url, fake.clone()).await;
    let plugin = plugin(&scheduler);
    let idle = || !scheduler.state().running;
    let cycles = tokio::spawn(Arc::clone(&scheduler).run());

    eventually(Instant::now() + PATIENCE, "the first cycle waits", || {
        scheduler.state().running && fake.listing_afters().len() == 1
    })
    .await;
    let checking = status(&plugin).await;
    assert_eq!(
        checking,
        StatusResponse {
            state: State::Checking.into(),
            message: String::new(),
            last_check_started_at: Some(timestamp(now())),
            last_success_at: None,
            next_check_at: None,
        }
    );
    assert_eq!(
        check(&plugin).await,
        CheckResponse {
            accepted: false,
            message: "a check is already running".to_owned(),
        }
    );

    fake.open_gate();
    eventually(Instant::now() + PATIENCE, "the first cycle ends", idle).await;
    assert_eq!(
        status(&plugin).await,
        StatusResponse {
            state: State::Idle.into(),
            message: String::new(),
            last_check_started_at: Some(timestamp(now())),
            last_success_at: Some(timestamp(now())),
            next_check_at: Some(timestamp(now() + TimeDelta::minutes(30))),
        }
    );

    fake.failing_listing();
    assert_eq!(
        check(&plugin).await,
        CheckResponse {
            accepted: true,
            message: String::new(),
        }
    );
    eventually(Instant::now() + PATIENCE, "the failure is reported", || {
        let state = scheduler.state();
        !state.running && state.problem.is_some()
    })
    .await;
    let failing = status(&plugin).await;
    assert_eq!(failing.state, i32::from(State::Failing));
    assert!(
        failing.message.starts_with("could not list editions"),
        "{}",
        failing.message
    );
    assert_eq!(failing.last_success_at, Some(timestamp(now())));
    cycles.abort();
}
