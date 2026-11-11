use std::collections::HashSet;
use std::error::Error;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use futures_util::stream::{self, StreamExt};
use tracing::{error, info, warn};

use crate::alerts::{edition_alert, welcome_alert};
use crate::domain::{Alert, Edition};
use crate::gazette::GazetteError;
use crate::store::{EditionRecord, Store, StoreError};
use crate::text::Hits;
use crate::timestamp::Clock;

const CURSOR_OVERLAP: TimeDelta = TimeDelta::hours(24);
const SCAN_WORKERS: usize = 2;
const MAX_SEEN: i64 = 1000;
const MAX_ALERTS: i64 = 100;

pub(crate) const DATABASE_UNAVAILABLE: &str = "the database is unavailable";

type ScanResult = Result<Option<Hits>, GazetteError>;

pub trait Gazette: Send + Sync + 'static {
    fn list_editions(
        &self,
        after: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<Edition>, GazetteError>> + Send;

    fn scan(
        &self,
        edition: &Edition,
        terms: &[String],
    ) -> impl Future<Output = Result<Option<Hits>, GazetteError>> + Send;
}

pub struct MonitorOptions {
    pub terms: Vec<String>,
    pub interval: Duration,
    pub start_after: DateTime<Utc>,
    pub clock: Clock,
}

pub struct Monitor<G> {
    store: Store,
    gazette: G,
    options: MonitorOptions,
    keep_seen: i64,
    keep_alerts: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CycleOutcome {
    Complete,
    Incomplete(String),
}

impl<G: Gazette> Monitor<G> {
    pub fn new(store: Store, gazette: G, options: MonitorOptions) -> Monitor<G> {
        Monitor {
            store,
            gazette,
            options,
            keep_seen: MAX_SEEN,
            keep_alerts: MAX_ALERTS,
        }
    }

    #[cfg(test)]
    pub(crate) fn keeping(mut self, seen: i64, alerts: i64) -> Monitor<G> {
        self.keep_seen = seen;
        self.keep_alerts = alerts;
        self
    }

    pub async fn prepare(&self) -> Result<(), StoreError> {
        let start_after = self.options.start_after;
        let welcome = welcome_alert(&self.options.terms, self.options.interval, self.now());
        if self.store.initialize(start_after, &welcome).await? {
            info!(cursor = ?start_after, "monitor state created");
        }
        Ok(())
    }

    pub async fn run_cycle(&self) -> CycleOutcome {
        let recorded = self.record_new_editions().await;
        if recorded.is_ok() {
            self.mark_success().await;
        }
        self.prune().await;
        match recorded {
            Ok(()) => CycleOutcome::Complete,
            Err(problem) => CycleOutcome::Incomplete(problem),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn interval(&self) -> Duration {
        self.options.interval
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.options.clock)()
    }

    async fn record_new_editions(&self) -> Result<(), String> {
        let fresh = self
            .unseen_editions()
            .await
            .map_err(|error| listing_failed(&*error))?;
        let scans = self.scan_all(&fresh).await;
        self.record_in_order(&fresh, scans).await
    }

    async fn unseen_editions(&self) -> Result<Vec<Edition>, Box<dyn Error + Send + Sync>> {
        let cursor = self.store.cursor().await?;
        let editions = self.gazette.list_editions(cursor - CURSOR_OVERLAP).await?;
        let ids: Vec<i64> = editions.iter().map(|edition| edition.id).collect();
        let seen: HashSet<i64> = self.store.seen_editions(&ids).await?.into_iter().collect();
        Ok(editions
            .into_iter()
            .filter(|edition| !seen.contains(&edition.id))
            .collect())
    }

    async fn scan_all(&self, editions: &[Edition]) -> Vec<ScanResult> {
        let scans: Vec<_> = editions
            .iter()
            .enumerate()
            .map(|(index, edition)| {
                let scan = self.gazette.scan(edition, &self.options.terms);
                async move { (index, scan.await) }
            })
            .collect();
        let mut finished: Vec<(usize, ScanResult)> = stream::iter(scans)
            .buffer_unordered(SCAN_WORKERS)
            .collect()
            .await;
        finished.sort_unstable_by_key(|(index, _)| *index);
        finished.into_iter().map(|(_, scan)| scan).collect()
    }

    async fn record_in_order(
        &self,
        editions: &[Edition],
        scans: Vec<ScanResult>,
    ) -> Result<(), String> {
        for (edition, scan) in editions.iter().zip(scans) {
            let hits = scan.map_err(|error| read_failed(edition, &error))?;
            self.record(edition, hits)
                .await
                .map_err(|error| record_failed(edition, &error))?;
        }
        Ok(())
    }

    async fn record(&self, edition: &Edition, hits: Option<Hits>) -> Result<(), StoreError> {
        let record = EditionRecord {
            id: edition.id,
            published: edition.published,
            alert: alert_for(edition, hits),
        };
        self.store.record_edition(&record).await
    }

    async fn mark_success(&self) {
        if let Err(failure) = self.store.mark_success(self.now()).await {
            error!(
                error = &failure as &dyn Error,
                "could not record the successful check"
            );
        }
    }

    async fn prune(&self) {
        if let Err(failure) = self.store.prune(self.keep_seen, self.keep_alerts).await {
            error!(
                error = &failure as &dyn Error,
                "could not prune old editions and alerts"
            );
        }
    }
}

fn alert_for(edition: &Edition, hits: Option<Hits>) -> Option<Alert> {
    let Some(hits) = hits else {
        info!(edition = %edition.title, "no match");
        return None;
    };
    info!(edition = %edition.title, pages = ?pages_in_term_order(&hits), "MATCH");
    Some(edition_alert(edition, &hits))
}

fn pages_in_term_order(hits: &Hits) -> Vec<(&str, &[usize])> {
    hits.order
        .iter()
        .map(|term| {
            let pages = hits.pages.get(term).map_or(&[][..], Vec::as_slice);
            (term.as_str(), pages)
        })
        .collect()
}

fn listing_failed(failure: &(dyn Error + 'static)) -> String {
    warn!(error = failure, "could not list editions");
    one_line(&format!("could not list editions: {}", shown(failure)))
}

fn read_failed(edition: &Edition, failure: &GazetteError) -> String {
    warn!(edition = %edition.title, error = failure as &dyn Error, "could not read edition");
    one_line(&format!(
        "could not read edition {}: {}",
        edition.title,
        shown(failure)
    ))
}

fn record_failed(edition: &Edition, failure: &StoreError) -> String {
    error!(edition = %edition.title, error = failure as &dyn Error, "could not record edition");
    one_line(&format!(
        "could not record edition {}: {}",
        edition.title,
        shown(failure)
    ))
}

fn shown(failure: &(dyn Error + 'static)) -> String {
    if failure.is::<StoreError>() {
        return DATABASE_UNAVAILABLE.to_owned();
    }
    describe(failure)
}

fn one_line(text: &str) -> String {
    text.split(['\r', '\n'])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn describe(failure: &(dyn Error + 'static)) -> String {
    let mut text = failure.to_string();
    let mut cause = failure.source();
    while let Some(current) = cause {
        let shown = current.to_string();
        if !text.ends_with(&shown) {
            text.push_str(": ");
            text.push_str(&shown);
        }
        cause = current.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::error::Error;
    use std::fmt::Debug;
    use std::sync::{Arc, Once};
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use monitor_hub_plugin::migrate::up;
    use monitor_hub_plugin_testkit::database;
    use tokio_postgres::NoTls;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::subscriber::Interest;
    use tracing::{Event, Metadata, Subscriber};

    use super::{
        CycleOutcome, Gazette, Monitor, MonitorOptions, alert_for, describe, listing_failed,
        read_failed, record_failed,
    };
    use crate::domain::Edition;
    use crate::gazette::GazetteError;
    use crate::store::{MIGRATIONS, Store, StoreError};
    use crate::text::Hits;

    struct EveryEditionMatches(Vec<Edition>);

    impl Gazette for EveryEditionMatches {
        async fn list_editions(&self, _after: DateTime<Utc>) -> Result<Vec<Edition>, GazetteError> {
            Ok(self.0.clone())
        }

        async fn scan(
            &self,
            _edition: &Edition,
            terms: &[String],
        ) -> Result<Option<Hits>, GazetteError> {
            Ok(Some(Hits {
                pages: HashMap::from([(terms[0].clone(), vec![1])]),
                order: terms.to_vec(),
                first_page: 1,
                snippet: "S".to_owned(),
            }))
        }
    }

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("timestamp")
            .to_utc()
    }

    #[tokio::test]
    async fn every_cycle_prunes_old_editions_and_acknowledged_alerts() {
        let Some(db) = database().await else {
            return;
        };
        let (mut client, connection) = tokio_postgres::connect(&db.url, NoTls)
            .await
            .expect("connect");
        tokio::spawn(connection);
        up(&mut client, MIGRATIONS).await.expect("migrate");
        let editions = (1..=4)
            .map(|id| Edition {
                id,
                published: utc("2026-11-06T20:00:00Z"),
                url: format!("https://x/{id}.pdf"),
                title: format!("ED {id}"),
            })
            .collect();
        let options = MonitorOptions {
            terms: vec!["TALES SABINI".to_owned()],
            interval: Duration::from_secs(30 * 60),
            start_after: utc("2026-11-01T00:00:00Z"),
            clock: Arc::new(|| utc("2026-11-09T12:00:00Z")),
        };
        let store = Store::new(&db.url).expect("store");
        let monitor = Monitor::new(store, EveryEditionMatches(editions), options).keeping(2, 1);
        monitor.prepare().await.expect("prepare");
        let welcome = monitor.store().last_issued_seq().await.expect("seq");
        monitor.store().ack_alerts(welcome).await.expect("ack");

        assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

        let mut seen = monitor
            .store()
            .seen_editions(&[1, 2, 3, 4])
            .await
            .expect("seen");
        seen.sort_unstable();
        assert_eq!(seen, [3, 4]);
        let alerts = monitor.store().alerts_after(0, 100).await.expect("alerts");
        let ids: Vec<&str> = alerts
            .iter()
            .map(|stored| stored.alert.id.as_str())
            .collect();
        assert_eq!(ids, ["edition-1", "edition-2", "edition-3", "edition-4"]);
    }

    #[derive(Debug, thiserror::Error)]
    #[error("connect")]
    struct Refused;

    #[derive(Debug, thiserror::Error)]
    #[error("send request")]
    struct Opaque(#[source] Refused);

    #[derive(Debug, thiserror::Error)]
    #[error("download: {0}")]
    struct Explained(#[source] Opaque);

    type LoggedEvent = Vec<(String, String)>;

    thread_local! {
        static CAPTURED: RefCell<Option<Vec<LoggedEvent>>> = const { RefCell::new(None) };
    }

    struct CaptureOnMarkedThreads;

    #[derive(Default)]
    struct Fields(LoggedEvent);

    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }

        fn record_error(&mut self, field: &Field, value: &(dyn Error + 'static)) {
            self.0
                .push((field.name().to_owned(), format!("error field: {value}")));
        }
    }

    impl Subscriber for CaptureOnMarkedThreads {
        fn register_callsite(&self, _: &'static Metadata<'static>) -> Interest {
            Interest::sometimes()
        }

        fn enabled(&self, _: &Metadata<'_>) -> bool {
            CAPTURED.with_borrow(Option::is_some)
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {}

        fn record_follows_from(&self, _: &Id, _: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = Fields::default();
            event.record(&mut fields);
            fields
                .0
                .push(("level".to_owned(), event.metadata().level().to_string()));
            fields.0.sort();
            CAPTURED.with_borrow_mut(|captured| {
                if let Some(events) = captured {
                    events.push(fields.0);
                }
            });
        }

        fn enter(&self, _: &Id) {}

        fn exit(&self, _: &Id) {}
    }

    fn captured(work: impl FnOnce()) -> Vec<LoggedEvent> {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            tracing::subscriber::set_global_default(CaptureOnMarkedThreads)
                .expect("no other global subscriber in the unit tests");
        });
        tracing::callsite::rebuild_interest_cache();
        CAPTURED.set(Some(Vec::new()));
        work();
        CAPTURED.take().unwrap_or_default()
    }

    fn logged(fields: &[(&str, &str)]) -> LoggedEvent {
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn problems_and_matches_are_logged_with_fields() {
        let edition = Edition {
            id: 1,
            published: utc("2026-11-06T20:00:00Z"),
            url: "https://x/1.pdf".to_owned(),
            title: "ED 1".to_owned(),
        };
        let hits = Hits {
            pages: HashMap::from([("B".to_owned(), vec![2]), ("A".to_owned(), vec![1, 3])]),
            order: vec!["B".to_owned(), "A".to_owned()],
            first_page: 1,
            snippet: "S".to_owned(),
        };

        let events = captured(|| {
            listing_failed(&GazetteError::TooManyPages { limit: 1 });
            read_failed(
                &edition,
                &GazetteError::Pdftotext("pdf unavailable".to_owned()),
            );
            record_failed(&edition, &StoreError::Timeout("waiting"));
            alert_for(&edition, Some(hits));
        });

        assert_eq!(
            events,
            [
                logged(&[
                    ("error", "error field: media list: more than 1 pages"),
                    ("level", "WARN"),
                    ("message", "could not list editions"),
                ]),
                logged(&[
                    ("edition", "ED 1"),
                    ("error", "error field: pdftotext: pdf unavailable"),
                    ("level", "WARN"),
                    ("message", "could not read edition"),
                ]),
                logged(&[
                    ("edition", "ED 1"),
                    ("error", "error field: waiting timed out"),
                    ("level", "ERROR"),
                    ("message", "could not record edition"),
                ]),
                logged(&[
                    ("edition", "ED 1"),
                    ("level", "INFO"),
                    ("message", "MATCH"),
                    ("pages", r#"[("B", [2]), ("A", [1, 3])]"#),
                ]),
            ]
        );
    }

    #[test]
    fn problem_texts_hide_database_errors_and_fit_on_one_line() {
        let edition = Edition {
            id: 1,
            published: utc("2026-11-06T20:00:00Z"),
            url: "https://x/1.pdf".to_owned(),
            title: "ED\n1".to_owned(),
        };

        assert_eq!(
            listing_failed(&StoreError::NotInitialized),
            "could not list editions: the database is unavailable"
        );
        assert_eq!(
            listing_failed(&GazetteError::TooManyPages { limit: 1 }),
            "could not list editions: media list: more than 1 pages"
        );
        assert_eq!(
            read_failed(
                &edition,
                &GazetteError::Pdftotext("bad font\r\n\nat page 2".to_owned())
            ),
            "could not read edition ED 1: pdftotext: bad font at page 2"
        );
        assert_eq!(
            record_failed(&edition, &StoreError::Timeout("waiting")),
            "could not record edition ED 1: the database is unavailable"
        );
    }

    #[test]
    fn describe_shows_each_cause_once() {
        assert_eq!(describe(&Refused), "connect");
        assert_eq!(describe(&Opaque(Refused)), "send request: connect");
        assert_eq!(
            describe(&Explained(Opaque(Refused))),
            "download: send request: connect"
        );
    }
}
