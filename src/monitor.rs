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
        }
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
            .map(|edition| self.gazette.scan(edition, &self.options.terms))
            .collect();
        stream::iter(scans).buffered(SCAN_WORKERS).collect().await
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
            error!(error = %describe(&failure), "could not record the successful check");
        }
    }

    async fn prune(&self) {
        if let Err(failure) = self.store.prune(MAX_SEEN, MAX_ALERTS).await {
            error!(error = %describe(&failure), "could not prune old editions and alerts");
        }
    }
}

fn alert_for(edition: &Edition, hits: Option<Hits>) -> Option<Alert> {
    let Some(hits) = hits else {
        info!(edition = %edition.title, "no match");
        return None;
    };
    info!(edition = %edition.title, pages = ?hits.pages, "MATCH");
    Some(edition_alert(edition, &hits))
}

fn listing_failed(failure: &(dyn Error + 'static)) -> String {
    let problem = format!("could not list editions: {}", describe(failure));
    warn!("{problem}");
    problem
}

fn read_failed(edition: &Edition, failure: &GazetteError) -> String {
    let problem = format!(
        "could not read edition {}: {}",
        edition.title,
        describe(failure)
    );
    warn!("{problem}");
    problem
}

fn record_failed(edition: &Edition, failure: &StoreError) -> String {
    let problem = format!(
        "could not record edition {}: {}",
        edition.title,
        describe(failure)
    );
    error!("{problem}");
    problem
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
    use super::describe;

    #[derive(Debug, thiserror::Error)]
    #[error("connect")]
    struct Refused;

    #[derive(Debug, thiserror::Error)]
    #[error("send request")]
    struct Opaque(#[source] Refused);

    #[derive(Debug, thiserror::Error)]
    #[error("download: {0}")]
    struct Explained(#[source] Opaque);

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
