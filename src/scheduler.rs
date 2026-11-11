use std::error::Error;
use std::pin::pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::{DateTime, Utc};
use tokio::sync::Notify;
use tokio::task::{AbortHandle, JoinError};
use tracing::error;

use crate::monitor::{CycleOutcome, Gazette, Monitor};

const CYCLE_CRASHED: &str = "the check stopped unexpectedly";

#[derive(Clone, Debug, Default)]
pub struct CycleState {
    pub running: bool,
    pub last_started: Option<DateTime<Utc>>,
    pub last_finished: Option<DateTime<Utc>>,
    pub problem: Option<String>,
}

pub struct Scheduler<G> {
    monitor: Arc<Monitor<G>>,
    state: Mutex<CycleState>,
    wake: Notify,
}

struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl<G: Gazette> Scheduler<G> {
    pub fn new(monitor: Arc<Monitor<G>>) -> Arc<Scheduler<G>> {
        Arc::new(Scheduler {
            monitor,
            state: Mutex::default(),
            wake: Notify::new(),
        })
    }

    pub async fn run(self: Arc<Self>) {
        loop {
            self.begin();
            let outcome = self.cycle().await;
            self.finish(outcome);
            self.wait_for_next().await;
        }
    }

    pub fn trigger(&self) -> bool {
        let state = self.lock_state();
        if state.running {
            return false;
        }
        self.wake.notify_one();
        true
    }

    pub fn state(&self) -> CycleState {
        self.lock_state().clone()
    }

    pub fn monitor(&self) -> &Monitor<G> {
        &self.monitor
    }

    fn begin(&self) {
        let mut state = self.lock_state();
        state.running = true;
        state.last_started = Some(self.monitor.now());
        let _served_by_this_cycle = pin!(self.wake.notified()).enable();
    }

    async fn cycle(&self) -> CycleOutcome {
        let monitor = Arc::clone(&self.monitor);
        let cycle = tokio::spawn(async move { monitor.run_cycle().await });
        let _abort = AbortOnDrop(cycle.abort_handle());
        cycle.await.unwrap_or_else(crashed)
    }

    fn finish(&self, outcome: CycleOutcome) {
        let mut state = self.lock_state();
        state.running = false;
        state.last_finished = Some(self.monitor.now());
        state.problem = match outcome {
            CycleOutcome::Complete => None,
            CycleOutcome::Incomplete(problem) => Some(problem),
        };
    }

    async fn wait_for_next(&self) {
        let _ = tokio::time::timeout(self.monitor.interval(), self.wake.notified()).await;
    }

    fn lock_state(&self) -> MutexGuard<'_, CycleState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn crashed(failure: JoinError) -> CycleOutcome {
    error!(error = &failure as &dyn Error, "{CYCLE_CRASHED}");
    CycleOutcome::Incomplete(CYCLE_CRASHED.to_owned())
}
