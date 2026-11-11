use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use chrono::{DateTime, Utc};
use diario_alerr::domain::Edition;
use diario_alerr::gazette::GazetteError;
use diario_alerr::monitor::Gazette;
use diario_alerr::text::Hits;
use tokio::sync::watch;

const SCAN_TIME: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub struct FakeGazette {
    state: Arc<State>,
}

struct State {
    editions: Vec<Edition>,
    hits: Mutex<HashMap<i64, Hits>>,
    failing_scans: Mutex<HashSet<i64>>,
    failing_listing: AtomicBool,
    gate_open: watch::Sender<bool>,
    held_scans: Mutex<HashSet<i64>>,
    scans_released: watch::Sender<bool>,
    listing_afters: Mutex<Vec<DateTime<Utc>>>,
    scans: AtomicUsize,
    running_scans: AtomicUsize,
    max_concurrent_scans: AtomicUsize,
}

impl FakeGazette {
    pub fn new(editions: Vec<Edition>) -> FakeGazette {
        FakeGazette {
            state: Arc::new(State {
                editions,
                hits: Mutex::default(),
                failing_scans: Mutex::default(),
                failing_listing: AtomicBool::new(false),
                gate_open: watch::Sender::new(true),
                held_scans: Mutex::default(),
                scans_released: watch::Sender::new(true),
                listing_afters: Mutex::default(),
                scans: AtomicUsize::new(0),
                running_scans: AtomicUsize::new(0),
                max_concurrent_scans: AtomicUsize::new(0),
            }),
        }
    }

    pub fn with_hits(&self, id: i64, hits: Hits) {
        lock(&self.state.hits).insert(id, hits);
    }

    pub fn failing_scan(&self, id: i64) {
        lock(&self.state.failing_scans).insert(id);
    }

    pub fn failing_listing(&self) {
        self.state.failing_listing.store(true, Ordering::SeqCst);
    }

    pub fn gated(&self) {
        self.state.gate_open.send_replace(false);
    }

    pub fn open_gate(&self) {
        self.state.gate_open.send_replace(true);
    }

    pub fn hold_scan(&self, id: i64) {
        lock(&self.state.held_scans).insert(id);
        self.state.scans_released.send_replace(false);
    }

    pub fn release_scans(&self) {
        self.state.scans_released.send_replace(true);
    }

    pub fn listing_afters(&self) -> Vec<DateTime<Utc>> {
        lock(&self.state.listing_afters).clone()
    }

    pub fn scans(&self) -> usize {
        self.state.scans.load(Ordering::SeqCst)
    }

    pub fn max_concurrent_scans(&self) -> usize {
        self.state.max_concurrent_scans.load(Ordering::SeqCst)
    }
}

impl Gazette for FakeGazette {
    fn list_editions(
        &self,
        after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send {
        let state = Arc::clone(&self.state);
        async move {
            lock(&state.listing_afters).push(after);
            let mut gate = state.gate_open.subscribe();
            let _ = gate.wait_for(|open| *open).await;
            if state.failing_listing.load(Ordering::SeqCst) {
                return Err(GazetteError::TooManyPages { limit: 1 });
            }
            Ok(state.editions.clone())
        }
    }

    fn scan(
        &self,
        edition: &Edition,
        _terms: &[String],
    ) -> impl Future<Output = Result<Option<Hits>, GazetteError>> + Send {
        let state = Arc::clone(&self.state);
        let id = edition.id;
        async move {
            state.scans.fetch_add(1, Ordering::SeqCst);
            let running = state.running_scans.fetch_add(1, Ordering::SeqCst) + 1;
            state
                .max_concurrent_scans
                .fetch_max(running, Ordering::SeqCst);
            tokio::time::sleep(SCAN_TIME).await;
            let held = lock(&state.held_scans).contains(&id);
            if held {
                let mut released = state.scans_released.subscribe();
                let _ = released.wait_for(|released| *released).await;
            }
            state.running_scans.fetch_sub(1, Ordering::SeqCst);
            if lock(&state.failing_scans).contains(&id) {
                return Err(GazetteError::Pdftotext("pdf unavailable".to_owned()));
            }
            Ok(lock(&state.hits).get(&id).cloned())
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
