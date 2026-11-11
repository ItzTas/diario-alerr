mod support;

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use diario_alerr::domain::Edition;
use diario_alerr::gazette::GazetteError;
use diario_alerr::monitor::{Gazette, Monitor, MonitorOptions};
use diario_alerr::scheduler::Scheduler;
use diario_alerr::store::Store;
use diario_alerr::text::Hits;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep, sleep_until};

use support::fake_gazette::FakeGazette;
use support::{fixed_clock, migrated_database, utc};

const PATIENCE: Duration = Duration::from_secs(5);
const HALF_HOUR: Duration = Duration::from_secs(30 * 60);

fn now() -> DateTime<Utc> {
    utc("2026-11-09T12:00:00Z")
}

fn millis(count: u64) -> Duration {
    Duration::from_millis(count)
}

async fn scheduler<G: Gazette>(url: &str, gazette: G, interval: Duration) -> Arc<Scheduler<G>> {
    let options = MonitorOptions {
        terms: vec!["TALES SABINI".to_owned()],
        interval,
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(now()),
    };
    let monitor = Monitor::new(Store::new(url).expect("store"), gazette, options);
    monitor.prepare().await.expect("prepare");
    Scheduler::new(Arc::new(monitor))
}

fn start<G: Gazette>(scheduler: &Arc<Scheduler<G>>) -> JoinHandle<()> {
    tokio::spawn(Arc::clone(scheduler).run())
}

async fn eventually(deadline: Instant, what: &str, check: impl Fn() -> bool) {
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        sleep(millis(10)).await;
    }
}

#[tokio::test]
async fn first_cycle_runs_at_start_and_the_next_after_the_interval() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    let scheduler = scheduler(&db.url, fake.clone(), Duration::from_secs(1)).await;
    let listings = || fake.listing_afters().len();

    let started = Instant::now();
    let _running = start(&scheduler);

    eventually(started + millis(300), "the first cycle lists", || {
        listings() == 1
    })
    .await;
    sleep_until(started + millis(600)).await;
    assert_eq!(listings(), 1);
    eventually(started + millis(2500), "the second cycle lists", || {
        listings() == 2
    })
    .await;
}

#[tokio::test]
async fn trigger_starts_a_cycle_and_is_refused_while_one_runs() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    fake.gated();
    let scheduler = scheduler(&db.url, fake.clone(), HALF_HOUR).await;
    let listings = || fake.listing_afters().len();
    let idle = || !scheduler.state().running;

    let _running = start(&scheduler);

    eventually(Instant::now() + PATIENCE, "the first cycle waits", || {
        scheduler.state().running && listings() == 1
    })
    .await;
    assert_eq!(scheduler.state().last_started, Some(now()));
    assert!(!scheduler.trigger());
    fake.open_gate();
    eventually(Instant::now() + PATIENCE, "the first cycle ends", idle).await;
    assert_eq!(listings(), 1);
    assert_eq!(scheduler.state().last_finished, Some(now()));
    assert_eq!(scheduler.state().problem, None);

    assert!(scheduler.trigger());
    eventually(
        Instant::now() + millis(300),
        "the triggered cycle lists",
        || listings() == 2,
    )
    .await;
    eventually(Instant::now() + PATIENCE, "the triggered cycle ends", idle).await;

    fake.failing_listing();
    assert!(scheduler.trigger());
    eventually(Instant::now() + PATIENCE, "the failure is reported", || {
        scheduler
            .state()
            .problem
            .is_some_and(|problem| problem.starts_with("could not list editions"))
    })
    .await;
    assert!(!scheduler.state().running);
}

#[tokio::test]
async fn the_interval_restarts_after_a_triggered_cycle() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    let scheduler = scheduler(&db.url, fake.clone(), Duration::from_secs(1)).await;
    let listings = || fake.listing_afters().len();

    let started = Instant::now();
    let _running = start(&scheduler);

    eventually(started + millis(300), "the first cycle ends", || {
        listings() == 1 && !scheduler.state().running
    })
    .await;
    sleep_until(started + millis(500)).await;
    assert!(scheduler.trigger());
    eventually(started + millis(800), "the triggered cycle lists", || {
        listings() == 2
    })
    .await;
    sleep_until(started + millis(1200)).await;
    assert_eq!(listings(), 2);
    eventually(started + millis(2500), "the next cycle lists", || {
        listings() == 3
    })
    .await;
}

struct PanickingListing {
    listings: Arc<AtomicUsize>,
}

impl Gazette for PanickingListing {
    fn list_editions(
        &self,
        _after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send {
        let listings = Arc::clone(&self.listings);
        async move {
            listings.fetch_add(1, Ordering::SeqCst);
            panic!("the listing exploded")
        }
    }

    async fn scan(
        &self,
        _edition: &Edition,
        _terms: &[String],
    ) -> Result<Option<Hits>, GazetteError> {
        Ok(None)
    }
}

#[tokio::test]
async fn a_panicking_cycle_is_reported_and_the_scheduler_keeps_going() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let listings = Arc::new(AtomicUsize::new(0));
    let gazette = PanickingListing {
        listings: Arc::clone(&listings),
    };
    let scheduler = scheduler(&db.url, gazette, HALF_HOUR).await;

    let running = start(&scheduler);

    eventually(Instant::now() + PATIENCE, "the crash is reported", || {
        let state = scheduler.state();
        !state.running && state.problem.as_deref() == Some("the check stopped unexpectedly")
    })
    .await;
    assert!(scheduler.trigger());
    eventually(Instant::now() + PATIENCE, "the next cycle runs", || {
        listings.load(Ordering::SeqCst) == 2 && !scheduler.state().running
    })
    .await;
    assert!(!running.is_finished());
}

struct FirstListingFails {
    inner: FakeGazette,
    failed: AtomicBool,
}

impl Gazette for FirstListingFails {
    fn list_editions(
        &self,
        after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send {
        let fail = !self.failed.swap(true, Ordering::SeqCst);
        let listing = self.inner.list_editions(after);
        async move {
            if fail {
                return Err(GazetteError::TooManyPages { limit: 1 });
            }
            listing.await
        }
    }

    fn scan(
        &self,
        edition: &Edition,
        terms: &[String],
    ) -> impl Future<Output = Result<Option<Hits>, GazetteError>> + Send {
        self.inner.scan(edition, terms)
    }
}

#[tokio::test]
async fn a_problem_stays_readable_until_a_complete_cycle_clears_it() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    let gazette = FirstListingFails {
        inner: fake.clone(),
        failed: AtomicBool::new(false),
    };
    let scheduler = scheduler(&db.url, gazette, HALF_HOUR).await;

    let _running = start(&scheduler);

    eventually(Instant::now() + PATIENCE, "the first cycle fails", || {
        let state = scheduler.state();
        !state.running && state.problem.is_some()
    })
    .await;
    let problem = scheduler.state().problem;
    fake.gated();
    assert!(scheduler.trigger());
    eventually(Instant::now() + PATIENCE, "the next cycle waits", || {
        scheduler.state().running && fake.listing_afters().len() == 1
    })
    .await;
    assert_eq!(scheduler.state().problem, problem);
    fake.open_gate();
    eventually(Instant::now() + PATIENCE, "the next cycle ends", || {
        !scheduler.state().running
    })
    .await;
    assert_eq!(scheduler.state().problem, None);
}

#[tokio::test]
async fn two_triggers_while_waiting_start_one_cycle() {
    let Some(db) = migrated_database().await else {
        return;
    };
    let fake = FakeGazette::new(vec![]);
    let scheduler = scheduler(&db.url, fake.clone(), HALF_HOUR).await;
    let listings = || fake.listing_afters().len();
    let idle = || !scheduler.state().running;

    let _running = start(&scheduler);

    eventually(Instant::now() + PATIENCE, "the first cycle ends", || {
        listings() == 1 && idle()
    })
    .await;
    assert!(scheduler.trigger());
    assert!(scheduler.trigger());
    eventually(
        Instant::now() + PATIENCE,
        "the triggered cycle ends",
        || listings() >= 2 && idle(),
    )
    .await;
    assert_eq!(listings(), 2);
}
