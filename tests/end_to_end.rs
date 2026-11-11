mod support;

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Alert;
use diario_alerr::gazette::{Client, HTTP_TIMEOUT};
use diario_alerr::monitor::{CycleOutcome, Monitor, MonitorOptions};
use diario_alerr::store::{EditionRecord, SequencedAlert, Store};
use tokio::time::Instant;
use tokio_postgres::NoTls;

use support::gazette_site::{GazetteSite, SiteEdition};
use support::grpc::{EventSummary, events};
use support::process::spawn_plugin;
use support::{fixed_clock, migrated_database, require_pdftotext, utc};

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
    monitor.store().alerts_after(0, 1000).await.expect("alerts")
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
            &["Sumario", "Nomeacao de TALES SABINI"],
        ),
    ])
    .await;
    let monitor = prepared(&db.url, &site, "Tales Sabini", now()).await;

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
        alerts[1].alert.body.contains("Tales Sabini: p. 2"),
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
        &["TALES SABINI"],
    )])
    .await;
    let first = prepared(&db.url, &site, "TALES SABINI", now()).await;
    assert_eq!(first.run_cycle().await, CycleOutcome::Complete);

    site.publish(site_edition(
        602,
        "2026-11-04T10:00:00Z",
        "ED 602",
        &["TALES SABINI aparece de novo"],
    ));
    let restarted = prepared(&db.url, &site, "TALES SABINI", now() + TimeDelta::hours(1)).await;
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

fn ids(events: &[EventSummary]) -> Vec<&str> {
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
    let cursor = utc("2026-11-02T10:00:00Z");
    let store = Store::new(&db.url).expect("store");
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
    let site = GazetteSite::start(vec![site_edition(
        702,
        "2026-11-03T10:00:00Z",
        "ED 702",
        &["Nomeacao de TALES SABINI"],
    )])
    .await;
    let sockets = tempfile::tempdir().expect("socket directory");
    let plugin = spawn_plugin(&[
        ("DATABASE_URL", &db.url),
        ("WATCH_TERMS", "TALES SABINI"),
        ("GAZETTE_API_URL", &site.api_url()),
        ("TMPDIR", sockets.path().to_str().expect("a UTF-8 path")),
    ]);

    wait_for_alert(&db.url, "edition-702").await;
    let socket = plugin.socket.clone();
    let (first, second) = tokio::task::spawn_blocking(move || {
        let first = events(&socket, 0, 500);
        let last = first.last().expect("history").seq;
        (first, events(&socket, last, 500))
    })
    .await
    .expect("read the events");

    assert_eq!(ids(&first), ["welcome", "edition-701"]);
    assert_eq!(ids(&second), ["edition-702"]);
}
