use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, TimeDelta, Utc};
use monitor_hub_plugin::proto::plugin_server::Plugin;
use monitor_hub_plugin::proto::{
    CheckRequest, CheckResponse, DescribeRequest, DescribeResponse, Event, EventKind,
    EventsRequest, EventsResponse, Severity, State, StatusRequest, StatusResponse,
};
use prost_types::Timestamp;
use tonic::{Request, Response, Status};
use tracing::{error, warn};

use crate::domain::{GAZETTE_NAME, WELCOME_ALERT_ID};
use crate::monitor::{DATABASE_UNAVAILABLE, Gazette};
use crate::scheduler::{CycleState, Scheduler};
use crate::store::{SequencedAlert, StoreError};

const PLUGIN_NAME: &str = "diario";
const TERM_FOUND_KIND: &str = "term_found";
const WELCOME_KIND: &str = "welcome";
const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;
const STATUS_QUERY_LIMIT: Duration = Duration::from_secs(10);
const ALREADY_RUNNING: &str = "a check is already running";
const HUB_AHEAD: &str =
    "the hub is ahead of this database: reset the hub's position for this plugin";

pub struct DiarioPlugin<G> {
    scheduler: Arc<Scheduler<G>>,
    hub_ahead_reported: AtomicBool,
}

impl<G: Gazette> DiarioPlugin<G> {
    pub fn new(scheduler: Arc<Scheduler<G>>) -> DiarioPlugin<G> {
        DiarioPlugin {
            scheduler,
            hub_ahead_reported: AtomicBool::new(false),
        }
    }

    async fn read_events(&self, after: i64, limit: i64) -> Result<Vec<Event>, StoreError> {
        let store = self.scheduler.monitor().store();
        let last_issued = store.last_issued_seq().await?;
        if after > last_issued {
            self.report_hub_ahead(after, last_issued);
            return Ok(Vec::new());
        }
        store.ack_alerts(after).await?;
        let alerts = store.alerts_after(after, limit).await?;
        Ok(alerts.into_iter().map(event).collect())
    }

    fn report_hub_ahead(&self, after: i64, last_issued: i64) {
        if self.hub_ahead_reported.swap(true, Ordering::Relaxed) {
            return;
        }
        error!(after, last_issued, "{HUB_AHEAD}");
    }

    async fn last_success(&self) -> Option<Option<DateTime<Utc>>> {
        let store = self.scheduler.monitor().store();
        let read = tokio::time::timeout(STATUS_QUERY_LIMIT, store.last_success()).await;
        let Ok(read) = read else {
            warn!("reading the last success took longer than {STATUS_QUERY_LIMIT:?}");
            return None;
        };
        read.inspect_err(|failure| {
            warn!(
                error = failure as &dyn Error,
                "could not read the last success"
            );
        })
        .ok()
    }
}

#[tonic::async_trait]
impl<G: Gazette> Plugin for DiarioPlugin<G> {
    async fn describe(
        &self,
        _: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        Ok(Response::new(DescribeResponse {
            name: PLUGIN_NAME.to_owned(),
            display_name: GAZETTE_NAME.to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            kinds: vec![
                kind(TERM_FOUND_KIND, "A watched term appears in a new edition"),
                kind(WELCOME_KIND, "Monitoring started"),
            ],
        }))
    }

    async fn status(&self, _: Request<StatusRequest>) -> Result<Response<StatusResponse>, Status> {
        let cycle = self.scheduler.state();
        let last_success = self.last_success().await;
        let interval = self.scheduler.monitor().interval();
        Ok(Response::new(status_of(&cycle, last_success, interval)))
    }

    async fn check(&self, _: Request<CheckRequest>) -> Result<Response<CheckResponse>, Status> {
        if self.scheduler.trigger() {
            return Ok(Response::new(CheckResponse {
                accepted: true,
                message: String::new(),
            }));
        }
        Ok(Response::new(CheckResponse {
            accepted: false,
            message: ALREADY_RUNNING.to_owned(),
        }))
    }

    async fn events(
        &self,
        request: Request<EventsRequest>,
    ) -> Result<Response<EventsResponse>, Status> {
        let EventsRequest { after, limit } = request.into_inner();
        let events = self
            .read_events(after, effective_limit(limit))
            .await
            .map_err(unavailable)?;
        Ok(Response::new(EventsResponse { events }))
    }
}

fn kind(id: &str, description: &str) -> EventKind {
    EventKind {
        id: id.to_owned(),
        description: description.to_owned(),
    }
}

fn effective_limit(requested: i32) -> i64 {
    if requested <= 0 {
        return DEFAULT_LIMIT;
    }
    i64::from(requested).min(MAX_LIMIT)
}

fn unavailable(failure: StoreError) -> Status {
    warn!(error = &failure as &dyn Error, "could not read the alerts");
    Status::unavailable(DATABASE_UNAVAILABLE)
}

fn event(stored: SequencedAlert) -> Event {
    let SequencedAlert { seq, alert } = stored;
    let (kind, severity) = kind_and_severity(&alert.id);
    Event {
        seq,
        id: alert.id,
        kind: kind.to_owned(),
        severity: severity.into(),
        occurred_at: Some(timestamp(alert.published)),
        title: alert.title,
        body: alert.body,
        url: alert.link,
        payload: None,
    }
}

fn kind_and_severity(id: &str) -> (&'static str, Severity) {
    if id == WELCOME_ALERT_ID {
        return (WELCOME_KIND, Severity::Info);
    }
    (TERM_FOUND_KIND, Severity::Urgent)
}

fn status_of(
    cycle: &CycleState,
    last_success: Option<Option<DateTime<Utc>>>,
    interval: Duration,
) -> StatusResponse {
    let (state, message) = state_and_message(cycle, last_success.is_some());
    StatusResponse {
        state: state.into(),
        message,
        last_check_started_at: cycle.last_started.map(timestamp),
        last_success_at: last_success.flatten().map(timestamp),
        next_check_at: next_check(cycle, interval).map(timestamp),
    }
}

fn state_and_message(cycle: &CycleState, database_answered: bool) -> (State, String) {
    if cycle.running {
        return (State::Checking, String::new());
    }
    if let Some(problem) = &cycle.problem {
        return (State::Failing, problem.clone());
    }
    if !database_answered {
        return (State::Failing, DATABASE_UNAVAILABLE.to_owned());
    }
    (State::Idle, String::new())
}

fn next_check(cycle: &CycleState, interval: Duration) -> Option<DateTime<Utc>> {
    if cycle.running {
        return None;
    }
    let interval = TimeDelta::from_std(interval).ok()?;
    cycle.last_finished?.checked_add_signed(interval)
}

fn timestamp(at: DateTime<Utc>) -> Timestamp {
    Timestamp::from(SystemTime::from(at))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    #[test]
    fn plugin_json_matches_the_contract() {
        let manifest: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/plugin.json"
        )))
        .expect("plugin.json is JSON");

        assert_eq!(
            manifest,
            json!({
                "protocol_version": 1,
                "name": "diario",
                "command": ["diario-alerr", "plugin"],
                "apk": ["poppler-utils"],
            })
        );
    }
}
