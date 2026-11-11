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
    status_query_limit: Duration,
}

impl<G: Gazette> DiarioPlugin<G> {
    pub fn new(scheduler: Arc<Scheduler<G>>) -> DiarioPlugin<G> {
        DiarioPlugin {
            scheduler,
            hub_ahead_reported: AtomicBool::new(false),
            status_query_limit: STATUS_QUERY_LIMIT,
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
        let limit = self.status_query_limit;
        let read = tokio::time::timeout(limit, store.last_success()).await;
        let Ok(read) = read else {
            warn!("reading the last success took longer than {limit:?}");
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
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use chrono::DateTime;
    use monitor_hub_plugin::migrate::up;
    use monitor_hub_plugin::proto::plugin_server::Plugin;
    use monitor_hub_plugin::proto::{EventsRequest, State, StatusRequest};
    use monitor_hub_plugin_testkit::database;
    use serde_json::{Value, json};
    use tokio::net::TcpListener;
    use tokio::runtime::{Builder, Runtime};
    use tokio_postgres::NoTls;
    use tonic::Request;

    use super::{DiarioPlugin, HUB_AHEAD, STATUS_QUERY_LIMIT};
    use crate::gazette::{Client, HTTP_TIMEOUT};
    use crate::logtest::{captured, logged};
    use crate::monitor::{Monitor, MonitorOptions};
    use crate::scheduler::Scheduler;
    use crate::store::{MIGRATIONS, Store};

    fn runtime() -> Runtime {
        Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn plugin(url: &str) -> DiarioPlugin<Client> {
        let now = DateTime::parse_from_rfc3339("2026-11-09T12:00:00Z")
            .expect("timestamp")
            .to_utc();
        let options = MonitorOptions {
            terms: vec!["TALES SABINI".to_owned()],
            interval: Duration::from_secs(30 * 60),
            start_after: now,
            clock: Arc::new(move || now),
        };
        let gazette =
            Client::new("http://127.0.0.1:9/media".to_owned(), HTTP_TIMEOUT).expect("client");
        let monitor = Monitor::new(Store::new(url).expect("store"), gazette, options);
        DiarioPlugin::new(Scheduler::new(Arc::new(monitor)))
    }

    async fn migrate(url: &str) {
        let (mut client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
        tokio::spawn(connection);
        up(&mut client, MIGRATIONS).await.expect("migrate");
    }

    #[test]
    fn a_hub_ahead_of_the_database_is_reported_once() {
        let runtime = runtime();
        let Some(db) = runtime.block_on(database()) else {
            return;
        };
        runtime.block_on(migrate(&db.url));
        let plugin = plugin(&db.url);
        let store = plugin.scheduler.monitor().store();
        runtime
            .block_on(plugin.scheduler.monitor().prepare())
            .expect("prepare");
        let newest = runtime.block_on(store.last_issued_seq()).expect("newest");
        let ahead = newest + 5;

        let events = captured(|| {
            for _ in 0..2 {
                let request = Request::new(EventsRequest {
                    after: ahead,
                    limit: 0,
                });
                let page = runtime.block_on(plugin.events(request)).expect("events");
                assert!(page.into_inner().events.is_empty());
            }
        });

        assert_eq!(
            events,
            [logged(&[
                ("after", &ahead.to_string()),
                ("last_issued", &newest.to_string()),
                ("level", "ERROR"),
                ("message", HUB_AHEAD),
            ])]
        );
    }

    #[tokio::test]
    async fn status_answers_within_its_limit_when_the_database_is_silent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let silent = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let mut plugin = plugin(&format!("postgres://diario:hunter2@{address}/diario"));
        assert_eq!(plugin.status_query_limit, STATUS_QUERY_LIMIT);
        assert_eq!(STATUS_QUERY_LIMIT, Duration::from_secs(10));
        let limit = Duration::from_millis(300);
        plugin.status_query_limit = limit;

        let started = Instant::now();
        let answered = tokio::time::timeout(
            Duration::from_secs(3),
            plugin.status(Request::new(StatusRequest {})),
        )
        .await;
        let elapsed = started.elapsed();
        silent.abort();

        let status = answered
            .expect("Status answered before the watchdog")
            .expect("status")
            .into_inner();
        assert!(elapsed < limit + Duration::from_secs(1), "took {elapsed:?}");
        assert_eq!(status.state, i32::from(State::Failing));
        assert_eq!(status.message, "the database is unavailable");
        assert_eq!(status.last_success_at, None);
    }

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
