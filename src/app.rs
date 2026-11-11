use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use chrono::Utc;
use monitor_hub_plugin::serve;
use tokio::time::{Instant, timeout_at};
use tracing::info;

use crate::config::Config;
use crate::gazette::{Client, HTTP_TIMEOUT};
use crate::monitor::{Monitor, MonitorOptions};
use crate::pdf::check_pdftotext;
use crate::scheduler::Scheduler;
use crate::server::DiarioPlugin;
use crate::store::{Store, WriterLock, acquire_writer_lock};
use crate::timestamp::system_clock;

const START_UP_LIMIT: Duration = Duration::from_secs(45);
const LOCK_LOST: &str = "the database lock was lost";

pub async fn run() -> anyhow::Result<()> {
    let config = Config::from_env(Utc::now())?;
    let (lock, monitor) = start_up(&config, START_UP_LIMIT).await?;
    serve_until_stopped(lock, monitor).await
}

async fn start_up(
    config: &Config,
    limit: Duration,
) -> anyhow::Result<(WriterLock, Monitor<Client>)> {
    let deadline = Instant::now() + limit;
    within(deadline, limit, "checking pdftotext", check_pdftotext()).await?;
    let mut lock = within(
        deadline,
        limit,
        "acquiring the database writer lock",
        acquire_writer_lock(&config.database_url),
    )
    .await?;
    let applied = within(
        deadline,
        limit,
        "applying the database migrations",
        lock.migrate(),
    )
    .await?;
    info!(versions = ?applied, "database migrations applied");
    let store = Store::new(&config.database_url).context("opening the database pool")?;
    let gazette = Client::new(config.gazette_api_url.clone(), HTTP_TIMEOUT)
        .context("building the gazette client")?;
    let monitor = Monitor::new(store, gazette, monitor_options(config));
    within(
        deadline,
        limit,
        "preparing the monitor state",
        monitor.prepare(),
    )
    .await?;
    Ok((lock, monitor))
}

async fn within<T, E>(
    deadline: Instant,
    limit: Duration,
    step: &'static str,
    work: impl Future<Output = Result<T, E>>,
) -> anyhow::Result<T>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let Ok(finished) = timeout_at(deadline, work).await else {
        return Err(anyhow!("the start-up limit of {limit:?} ran out").context(step));
    };
    finished.context(step)
}

fn monitor_options(config: &Config) -> MonitorOptions {
    MonitorOptions {
        terms: config.terms.clone(),
        interval: config.interval,
        start_after: config.start_after,
        clock: system_clock(),
    }
}

async fn serve_until_stopped(mut lock: WriterLock, monitor: Monitor<Client>) -> anyhow::Result<()> {
    let scheduler = Scheduler::new(Arc::new(monitor));
    let cycles = tokio::spawn(Arc::clone(&scheduler).run());
    let ended = tokio::select! {
        served = serve(DiarioPlugin::new(scheduler)) => served.context("serving the plugin"),
        () = lock.closed() => Err(lock_lost(&lock)),
    };
    cycles.abort();
    let _ = cycles.await;
    ended
}

fn lock_lost(lock: &WriterLock) -> anyhow::Error {
    let Some(reason) = lock.close_reason() else {
        return anyhow!(LOCK_LOST);
    };
    anyhow::Error::msg(reason).context(LOCK_LOST)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::net::TcpListener;

    use super::start_up;
    use crate::config::Config;
    use crate::pdf::tests::pdftotext_available;
    use crate::timestamp::parse_rfc3339;

    #[tokio::test]
    async fn start_up_stops_at_its_limit_and_names_the_step() {
        if !pdftotext_available() {
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let address = listener.local_addr().expect("address");
        let silent = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let config = Config {
            database_url: format!("postgres://diario:hunter2@{address}/diario"),
            terms: vec!["TALES SABINI".to_owned()],
            interval: Duration::from_secs(30 * 60),
            start_after: parse_rfc3339("2026-11-01T00:00:00Z").expect("timestamp"),
            gazette_api_url: "http://127.0.0.1:9/media".to_owned(),
        };

        let started = Instant::now();
        let Err(error) = start_up(&config, Duration::from_millis(300)).await else {
            panic!("a silent database let the start-up finish");
        };
        let elapsed = started.elapsed();
        silent.abort();

        assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
        assert_eq!(
            format!("{error:#}"),
            "acquiring the database writer lock: the start-up limit of 300ms ran out"
        );
    }
}
