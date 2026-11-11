mod support;

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::domain::Edition;
use diario_alerr::gazette::GazetteError;
use diario_alerr::monitor::{CycleOutcome, Gazette, Monitor, MonitorOptions};
use diario_alerr::store::{SequencedAlert, Store};
use diario_alerr::text::Hits;

use support::fake_gazette::FakeGazette;
use support::{fixed_clock, migrated_database, utc};

fn now() -> DateTime<Utc> {
    utc("2026-11-09T12:00:00Z")
}

fn options(start_after: DateTime<Utc>) -> MonitorOptions {
    MonitorOptions {
        terms: vec!["TALES SABINI".to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after,
        clock: fixed_clock(now()),
    }
}

fn start_after() -> DateTime<Utc> {
    utc("2026-11-01T00:00:00Z")
}

async fn prepared<G: Gazette>(url: &str, gazette: G) -> Monitor<G> {
    let store = Store::new(url).expect("store");
    let monitor = Monitor::new(store, gazette, options(start_after()));
    monitor.prepare().await.expect("prepare");
    monitor
}

fn edition(id: i64, published: DateTime<Utc>) -> Edition {
    Edition {
        id,
        published,
        url: format!("https://x/{id}.pdf"),
        title: format!("ED {id}"),
    }
}

fn hits_on(page: usize) -> Hits {
    Hits {
        pages: HashMap::from([("TALES SABINI".to_owned(), vec![page])]),
        order: vec!["TALES SABINI".to_owned()],
        first_page: page,
        snippet: "S".to_owned(),
    }
}

async fn alerts<G: Gazette>(monitor: &Monitor<G>) -> Vec<SequencedAlert> {
    monitor.store().alerts_after(0, 1000).await.expect("alerts")
}

fn ids(alerts: &[SequencedAlert]) -> Vec<&str> {
    alerts
        .iter()
        .map(|stored| stored.alert.id.as_str())
        .collect()
}

async fn seen<G: Gazette>(monitor: &Monitor<G>, ids: &[i64]) -> Vec<i64> {
    let mut seen = monitor.store().seen_editions(ids).await.expect("seen");
    seen.sort_unstable();
    seen
}

#[tokio::test]
async fn prepare_creates_the_state_and_the_welcome_once() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let monitor = prepared(&db.url, FakeGazette::new(vec![])).await;

    let created = alerts(&monitor).await;
    assert_eq!(ids(&created), ["welcome"]);
    assert_eq!(
        created[0].alert.body,
        "Watching: TALES SABINI\nChecking every 30 min."
    );
    assert_eq!(created[0].alert.published, now());
    assert_eq!(monitor.store().cursor().await.unwrap(), start_after());

    let store = Store::new(&db.url).expect("store");
    let restarted = Monitor::new(
        store,
        FakeGazette::new(vec![]),
        options(utc("2026-11-05T00:00:00Z")),
    );
    restarted.prepare().await.expect("second prepare");

    assert_eq!(alerts(&restarted).await, created);
    assert_eq!(restarted.store().cursor().await.unwrap(), start_after());
}

#[tokio::test]
async fn run_cycle_alerts_on_matches() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let published = utc("2026-11-06T20:00:00Z");
    let fake = FakeGazette::new(vec![
        edition(10, published),
        edition(11, published + TimeDelta::hours(1)),
    ]);
    fake.with_hits(10, hits_on(4));
    let monitor = prepared(&db.url, fake.clone()).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

    assert_eq!(ids(&alerts(&monitor).await), ["welcome", "edition-10"]);
    assert_eq!(fake.listing_afters()[0], utc("2026-10-31T00:00:00Z"));
    assert_eq!(
        monitor.store().cursor().await.unwrap(),
        utc("2026-11-06T21:00:00Z")
    );
    assert_eq!(monitor.store().last_success().await.unwrap(), Some(now()));

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);
    assert_eq!(alerts(&monitor).await.len(), 2);
}

#[tokio::test]
async fn run_cycle_keeps_unreadable_editions_for_the_next_cycle() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let published = utc("2026-11-06T20:00:00Z");
    let fake = FakeGazette::new(vec![
        edition(1, published),
        edition(2, published + TimeDelta::hours(1)),
    ]);
    fake.failing_scan(2);
    let monitor = prepared(&db.url, fake).await;

    let outcome = monitor.run_cycle().await;

    assert_eq!(
        outcome,
        CycleOutcome::Incomplete(
            "could not read edition ED 2: pdftotext: pdf unavailable".to_owned()
        )
    );
    assert_eq!(seen(&monitor, &[1, 2]).await, [1]);
    assert_eq!(monitor.store().last_success().await.unwrap(), None);
}

#[tokio::test]
async fn a_failed_scan_stops_recording_but_every_edition_is_scanned() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let published = utc("2026-11-06T20:00:00Z");
    let editions = (1..=5)
        .map(|id| edition(id, published + TimeDelta::hours(id)))
        .collect();
    let fake = FakeGazette::new(editions);
    fake.failing_scan(1);
    let monitor = prepared(&db.url, fake.clone()).await;

    let outcome = monitor.run_cycle().await;

    assert!(
        matches!(outcome, CycleOutcome::Incomplete(_)),
        "{outcome:?}"
    );
    assert_eq!(fake.scans(), 5);
    assert!(seen(&monitor, &[1, 2, 3, 4, 5]).await.is_empty());
    assert_eq!(fake.max_concurrent_scans(), 2);
}

#[tokio::test]
async fn a_failed_record_stops_recording() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let published = utc("2026-11-06T20:00:00Z");
    let mut editions: Vec<Edition> = (1..=3)
        .map(|id| edition(id, published + TimeDelta::hours(id)))
        .collect();
    editions[1].title = "ED \0 2".to_owned();
    let fake = FakeGazette::new(editions);
    for id in 1..=3 {
        fake.with_hits(id, hits_on(1));
    }
    let monitor = prepared(&db.url, fake.clone()).await;

    let outcome = monitor.run_cycle().await;

    let CycleOutcome::Incomplete(problem) = outcome else {
        panic!("the cycle completed");
    };
    assert!(
        problem.starts_with("could not record edition ED \0 2: "),
        "{problem:?}"
    );
    assert_eq!(fake.scans(), 3);
    assert_eq!(seen(&monitor, &[1, 2, 3]).await, [1]);
    assert_eq!(monitor.store().last_success().await.unwrap(), None);
    assert_eq!(ids(&alerts(&monitor).await), ["welcome", "edition-1"]);
}

struct LaterEditionsScanFaster {
    inner: FakeGazette,
}

impl Gazette for LaterEditionsScanFaster {
    fn list_editions(
        &self,
        after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send {
        self.inner.list_editions(after)
    }

    fn scan(
        &self,
        edition: &Edition,
        terms: &[String],
    ) -> impl Future<Output = Result<Option<Hits>, GazetteError>> + Send {
        let delay = Duration::from_millis(60 - 15 * edition.id.unsigned_abs());
        let scan = self.inner.scan(edition, terms);
        async move {
            tokio::time::sleep(delay).await;
            scan.await
        }
    }
}

#[tokio::test]
async fn scan_results_reach_their_own_editions_when_later_scans_finish_first() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let published = utc("2026-11-06T20:00:00Z");
    let editions = (1..=3)
        .map(|id| edition(id, published + TimeDelta::hours(id)))
        .collect();
    let fake = FakeGazette::new(editions);
    for id in 1..=3 {
        fake.with_hits(id, hits_on(id.unsigned_abs() as usize));
    }
    let monitor = prepared(&db.url, LaterEditionsScanFaster { inner: fake }).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

    let recorded: Vec<(String, String)> = alerts(&monitor)
        .await
        .into_iter()
        .skip(1)
        .map(|stored| (stored.alert.id, stored.alert.link))
        .collect();
    let expected: Vec<(String, String)> = (1..=3)
        .map(|id| {
            (
                format!("edition-{id}"),
                format!("https://x/{id}.pdf#page={id}"),
            )
        })
        .collect();
    assert_eq!(recorded, expected);
}
