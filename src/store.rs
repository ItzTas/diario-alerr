use std::error::Error;
use std::time::Duration;

use chrono::{DateTime, Utc};
use deadpool_postgres::{Manager, Pool, PoolError, Runtime, TimeoutType};
use monitor_hub_plugin::migrate::{MigrateError, Migration, up};
use tokio::task::{JoinError, JoinHandle};
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Config, NoTls, Row, Transaction};

use crate::domain::Alert;

pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "00001_monitor.sql",
        sql: include_str!("../sql/schema/00001_monitor.sql"),
    },
    Migration {
        version: 2,
        name: "00002_acked_seq.sql",
        sql: include_str!("../sql/schema/00002_acked_seq.sql"),
    },
];

pub const WRITER_LOCK_ID: i64 = 0x6469_6172_696f;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const POOL_SIZE: usize = 4;
const POOL_LIMITS: Limits = Limits {
    statement: Duration::from_secs(20),
    lock: Duration::from_secs(5),
};
const KEEPALIVES_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVES_INTERVAL: Duration = Duration::from_secs(10);
const KEEPALIVES_RETRIES: u32 = 3;
const TCP_USER_TIMEOUT: Duration = Duration::from_secs(60);

const TRY_WRITER_LOCK: &str = "SELECT pg_try_advisory_lock($1)";

const CREATE_STATE: &str = "INSERT INTO monitor_state (cursor_at, last_success_at)
VALUES ($1, NULL)
ON CONFLICT (singleton) DO NOTHING";
const CURSOR: &str = "SELECT cursor_at FROM monitor_state";
const ADVANCE_CURSOR: &str = "UPDATE monitor_state SET cursor_at = GREATEST(cursor_at, $1)";
const MARK_SUCCESS: &str = "UPDATE monitor_state SET last_success_at = $1";
const LAST_SUCCESS: &str = "SELECT last_success_at FROM monitor_state";
const MARK_EDITION_SEEN: &str = "INSERT INTO seen_editions (edition_id) VALUES ($1)
ON CONFLICT (edition_id) DO NOTHING";
const SEEN_EDITIONS: &str =
    "SELECT edition_id FROM seen_editions WHERE edition_id = ANY($1::BIGINT[])";
const PRUNE_SEEN_EDITIONS: &str = "DELETE FROM seen_editions
WHERE edition_id NOT IN (
    SELECT edition_id FROM seen_editions ORDER BY edition_id DESC LIMIT $1
)";
const INSERT_ALERT: &str = "INSERT INTO alerts (id, title, link, body, published_at)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (id) DO NOTHING";
const PRUNE_ALERTS: &str = "DELETE FROM alerts
WHERE seq <= (SELECT acked_seq FROM monitor_state)
  AND seq NOT IN (SELECT seq FROM alerts ORDER BY seq DESC LIMIT $1)";
const ALERTS_AFTER: &str = "SELECT seq, id, title, link, body, published_at FROM alerts WHERE seq > $1 ORDER BY seq LIMIT $2";
const ACK_ALERTS: &str = "UPDATE monitor_state SET acked_seq = $1 WHERE acked_seq < $1";
const LAST_ISSUED_SEQ: &str = "SELECT GREATEST(
    (SELECT CASE WHEN is_called THEN last_value ELSE 0 END FROM alerts_seq_seq),
    (SELECT COALESCE(max(seq), 0) FROM alerts)
)";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("DATABASE_URL is not a valid Postgres address")]
    InvalidUrl,
    #[error("the monitor state was never initialized")]
    NotInitialized,
    #[error("another diario-alerr process holds the database lock")]
    Locked,
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error(transparent)]
    Pool(PoolError),
    #[error(transparent)]
    Database(tokio_postgres::Error),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
}

impl From<tokio_postgres::Error> for StoreError {
    fn from(error: tokio_postgres::Error) -> Self {
        let timeouts = [
            (SqlState::QUERY_CANCELED, "running a database statement"),
            (SqlState::LOCK_NOT_AVAILABLE, "waiting for a database lock"),
        ];
        let step = error.code().and_then(|code| {
            timeouts
                .into_iter()
                .find(|(timeout, _)| timeout == code)
                .map(|(_, step)| step)
        });
        let Some(step) = step else {
            return StoreError::Database(error);
        };
        StoreError::Timeout(step)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditionRecord {
    pub id: i64,
    pub published: DateTime<Utc>,
    pub alert: Option<Alert>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequencedAlert {
    pub seq: i64,
    pub alert: Alert,
}

#[derive(Clone, Copy)]
struct Limits {
    statement: Duration,
    lock: Duration,
}

pub struct WriterLock {
    client: Client,
    connection: Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
    close_reason: Option<String>,
}

pub async fn acquire_writer_lock(database_url: &str) -> Result<WriterLock, StoreError> {
    let config = lock_config(database_url)?;
    let (client, connection) = tokio::time::timeout(CONNECT_TIMEOUT, config.connect(NoTls))
        .await
        .map_err(|_| StoreError::Timeout("connecting to the database for the writer lock"))??;
    let connection = tokio::spawn(connection);
    let locked: bool = client
        .query_one(TRY_WRITER_LOCK, &[&WRITER_LOCK_ID])
        .await?
        .try_get(0)?;
    if !locked {
        return Err(StoreError::Locked);
    }
    Ok(WriterLock {
        client,
        connection: Some(connection),
        close_reason: None,
    })
}

impl WriterLock {
    pub async fn migrate(&mut self) -> Result<Vec<i64>, StoreError> {
        Ok(up(&mut self.client, MIGRATIONS).await?)
    }

    pub async fn closed(&mut self) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let ended = connection.await;
        self.close_reason = reason_for(ended);
        self.connection = None;
    }

    pub fn close_reason(&self) -> Option<String> {
        self.close_reason.clone()
    }
}

#[derive(Clone)]
pub struct Store {
    pool: Pool,
}

impl Store {
    pub fn new(database_url: &str) -> Result<Store, StoreError> {
        Ok(Store::with_config(pool_config(database_url, POOL_LIMITS)?))
    }

    fn with_config(config: Config) -> Store {
        let manager = Manager::new(config, NoTls);
        let pool = Pool::builder(manager)
            .max_size(POOL_SIZE)
            .runtime(Runtime::Tokio1)
            .create_timeout(Some(CONNECT_TIMEOUT))
            .wait_timeout(Some(CONNECT_TIMEOUT))
            .build()
            .expect("the pool has a runtime for its timeouts");
        Store { pool }
    }

    pub async fn initialize(
        &self,
        cursor: DateTime<Utc>,
        welcome: &Alert,
    ) -> Result<bool, StoreError> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        let created = transaction.execute(CREATE_STATE, &[&cursor]).await? > 0;
        if created {
            insert_alert(&transaction, welcome).await?;
        }
        transaction.commit().await?;
        Ok(created)
    }

    pub async fn cursor(&self) -> Result<DateTime<Utc>, StoreError> {
        let row = self.client().await?.query_opt(CURSOR, &[]).await?;
        let row = row.ok_or(StoreError::NotInitialized)?;
        Ok(row.try_get(0)?)
    }

    pub async fn seen_editions(&self, ids: &[i64]) -> Result<Vec<i64>, StoreError> {
        let rows = self.client().await?.query(SEEN_EDITIONS, &[&ids]).await?;
        Ok(rows
            .iter()
            .map(|row| row.try_get(0))
            .collect::<Result<_, _>>()?)
    }

    pub async fn record_edition(&self, record: &EditionRecord) -> Result<(), StoreError> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .execute(MARK_EDITION_SEEN, &[&record.id])
            .await?;
        transaction
            .execute(ADVANCE_CURSOR, &[&record.published])
            .await?;
        if let Some(alert) = &record.alert {
            insert_alert(&transaction, alert).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub async fn mark_success(&self, at: DateTime<Utc>) -> Result<(), StoreError> {
        self.client().await?.execute(MARK_SUCCESS, &[&at]).await?;
        Ok(())
    }

    pub async fn last_success(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let row = self.client().await?.query_opt(LAST_SUCCESS, &[]).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(row.try_get(0)?)
    }

    pub async fn prune(&self, keep_seen: i64, keep_alerts: i64) -> Result<(), StoreError> {
        let mut client = self.client().await?;
        let transaction = client.transaction().await?;
        transaction
            .execute(PRUNE_SEEN_EDITIONS, &[&keep_seen])
            .await?;
        transaction.execute(PRUNE_ALERTS, &[&keep_alerts]).await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn alerts_after(
        &self,
        after: i64,
        limit: i64,
    ) -> Result<Vec<SequencedAlert>, StoreError> {
        let rows = self
            .client()
            .await?
            .query(ALERTS_AFTER, &[&after, &limit])
            .await?;
        Ok(rows.iter().map(sequenced_alert).collect::<Result<_, _>>()?)
    }

    pub async fn last_issued_seq(&self) -> Result<i64, StoreError> {
        let row = self.client().await?.query_one(LAST_ISSUED_SEQ, &[]).await?;
        Ok(row.try_get(0)?)
    }

    pub async fn ack_alerts(&self, seq: i64) -> Result<(), StoreError> {
        self.client().await?.execute(ACK_ALERTS, &[&seq]).await?;
        Ok(())
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, StoreError> {
        self.pool.get().await.map_err(pool_error)
    }
}

fn pool_config(database_url: &str, limits: Limits) -> Result<Config, StoreError> {
    let mut config = lock_config(database_url)?;
    let limits = format!(
        "-c statement_timeout={} -c lock_timeout={}",
        limits.statement.as_millis(),
        limits.lock.as_millis()
    );
    let options = after_existing(config.get_options(), limits);
    config.options(options);
    Ok(config)
}

fn after_existing(existing: Option<&str>, settings: String) -> String {
    let Some(existing) = existing.filter(|options| !options.trim().is_empty()) else {
        return settings;
    };
    format!("{existing} {settings}")
}

fn lock_config(database_url: &str) -> Result<Config, StoreError> {
    let mut config = parse(database_url)?;
    config
        .keepalives(true)
        .keepalives_idle(KEEPALIVES_IDLE)
        .keepalives_interval(KEEPALIVES_INTERVAL)
        .keepalives_retries(KEEPALIVES_RETRIES)
        .tcp_user_timeout(TCP_USER_TIMEOUT);
    Ok(config)
}

fn parse(database_url: &str) -> Result<Config, StoreError> {
    database_url.parse().map_err(|_| StoreError::InvalidUrl)
}

fn reason_for(ended: Result<Result<(), tokio_postgres::Error>, JoinError>) -> Option<String> {
    match ended {
        Ok(Ok(())) => None,
        Ok(Err(error)) => Some(chain_text(&error)),
        Err(error) => Some(chain_text(&error)),
    }
}

pub(crate) fn chain_text(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
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

fn pool_error(error: PoolError) -> StoreError {
    let PoolError::Timeout(kind) = error else {
        return StoreError::Pool(error);
    };
    StoreError::Timeout(match kind {
        TimeoutType::Wait => "waiting for a free database connection",
        TimeoutType::Create => "connecting to the database",
        TimeoutType::Recycle => "checking a pooled database connection",
    })
}

async fn insert_alert(transaction: &Transaction<'_>, alert: &Alert) -> Result<(), StoreError> {
    transaction
        .execute(
            INSERT_ALERT,
            &[
                &alert.id,
                &alert.title,
                &alert.link,
                &alert.body,
                &alert.published,
            ],
        )
        .await?;
    Ok(())
}

fn sequenced_alert(row: &Row) -> Result<SequencedAlert, tokio_postgres::Error> {
    Ok(SequencedAlert {
        seq: row.try_get(0)?,
        alert: Alert {
            id: row.try_get(1)?,
            title: row.try_get(2)?,
            link: row.try_get(3)?,
            body: row.try_get(4)?,
            published: row.try_get(5)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use chrono::DateTime;
    use monitor_hub_plugin::migrate::up;
    use monitor_hub_plugin_testkit::{Database, database};
    use tokio_postgres::{Client, NoTls};

    use super::{
        Limits, MIGRATIONS, POOL_SIZE, Store, StoreError, acquire_writer_lock, chain_text,
        lock_config, pool_config,
    };
    use crate::domain::Alert;

    const URL: &str = "postgres://diario:hunter2@db.internal:5432/diario";
    const SHORT: Limits = Limits {
        statement: Duration::from_millis(400),
        lock: Duration::from_millis(300),
    };
    const SLACK: Duration = Duration::from_secs(3);

    async fn connect(url: &str) -> Client {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
        tokio::spawn(connection);
        client
    }

    async fn migrated() -> Option<Database> {
        let database = database().await?;
        let mut client = connect(&database.url).await;
        up(&mut client, MIGRATIONS).await.expect("migrate");
        Some(database)
    }

    async fn show(client: &Client, setting: &str) -> String {
        client
            .query_one(&format!("SHOW {setting}"), &[])
            .await
            .expect("show")
            .get(0)
    }

    fn with_query(url: &str, parameter: &str) -> String {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!("{url}{separator}{parameter}")
    }

    fn welcome() -> Alert {
        let at = DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z")
            .expect("timestamp")
            .to_utc();
        Alert {
            id: "welcome".to_owned(),
            title: "title".to_owned(),
            link: "https://x/welcome".to_owned(),
            body: "body".to_owned(),
            published: at,
        }
    }

    fn assert_keepalives(config: &tokio_postgres::Config) {
        assert!(config.get_keepalives());
        assert_eq!(config.get_keepalives_idle(), Duration::from_secs(30));
        assert_eq!(
            config.get_keepalives_interval(),
            Some(Duration::from_secs(10))
        );
        assert_eq!(config.get_keepalives_retries(), Some(3));
        assert_eq!(
            config.get_tcp_user_timeout(),
            Some(&Duration::from_secs(60))
        );
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

    #[test]
    fn chain_text_shows_each_cause_once() {
        assert_eq!(chain_text(&Refused), "connect");
        assert_eq!(chain_text(&Opaque(Refused)), "send request: connect");
        assert_eq!(
            chain_text(&Explained(Opaque(Refused))),
            "download: send request: connect"
        );
    }

    #[test]
    fn pool_connections_get_limits_and_keepalives() {
        let config = pool_config(URL, super::POOL_LIMITS).expect("config");
        assert_eq!(
            config.get_options(),
            Some("-c statement_timeout=20000 -c lock_timeout=5000")
        );
        assert_keepalives(&config);
    }

    #[test]
    fn the_lock_connection_gets_keepalives_and_no_limits() {
        let config = lock_config(URL).expect("config");
        assert_eq!(config.get_options(), None);
        assert_keepalives(&config);
    }

    #[test]
    fn the_store_settings_come_after_the_urls_own() {
        let url = with_query(
            URL,
            "options=-c%20statement_timeout%3D1&keepalives_idle=5&tcp_user_timeout=1",
        );
        let config = pool_config(&url, super::POOL_LIMITS).expect("config");
        assert_eq!(
            config.get_options(),
            Some("-c statement_timeout=1 -c statement_timeout=20000 -c lock_timeout=5000")
        );
        assert_keepalives(&config);
    }

    #[tokio::test]
    async fn pool_and_lock_connections_carry_their_limits() {
        let Some(db) = migrated().await else {
            return;
        };
        let store = Store::new(&db.url).expect("store");
        let pooled = store.client().await.expect("pooled connection");
        assert_eq!(show(&pooled, "statement_timeout").await, "20s");
        assert_eq!(show(&pooled, "lock_timeout").await, "5s");

        let lock = acquire_writer_lock(&db.url).await.expect("lock");
        assert_eq!(show(&lock.client, "statement_timeout").await, "0");
        assert_eq!(show(&lock.client, "lock_timeout").await, "0");
    }

    #[tokio::test]
    async fn the_urls_own_options_still_apply() {
        let Some(db) = migrated().await else {
            return;
        };
        let url = with_query(&db.url, "options=-c%20application_name%3Dx");
        let store = Store::new(&url).expect("store");
        let pooled = store.client().await.expect("pooled connection");
        assert_eq!(show(&pooled, "application_name").await, "x");
        assert_eq!(show(&pooled, "statement_timeout").await, "20s");
    }

    #[tokio::test]
    async fn a_held_state_row_times_out_without_wedging_the_pool() {
        let Some(db) = migrated().await else {
            return;
        };
        let store = Store::with_config(pool_config(&db.url, SHORT).expect("config"));
        store
            .initialize(welcome().published, &welcome())
            .await
            .expect("initialize");
        let outside = connect(&db.url).await;
        outside
            .batch_execute("BEGIN; SELECT * FROM monitor_state FOR UPDATE")
            .await
            .expect("hold the state row");

        let started = Instant::now();
        let error = tokio::time::timeout(SHORT.lock + SLACK, store.ack_alerts(7))
            .await
            .expect("the held row stopped the call within the lock timeout")
            .expect_err("the row is held");
        assert!(
            started.elapsed() < SHORT.lock + SLACK,
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_eq!(error.to_string(), "waiting for a database lock timed out");

        for _ in 0..POOL_SIZE {
            let abandoned = tokio::time::timeout(Duration::from_millis(20), store.ack_alerts(9));
            assert!(abandoned.await.is_err(), "the abandoned call finished");
        }
        let cursor = tokio::time::timeout(SHORT.lock + SLACK, store.cursor())
            .await
            .expect("the pool is usable again")
            .expect("cursor");
        assert_eq!(cursor, welcome().published);

        outside.batch_execute("ROLLBACK").await.expect("release");
        store.ack_alerts(7).await.expect("ack once released");
        let acked: i64 = outside
            .query_one("SELECT acked_seq FROM monitor_state", &[])
            .await
            .expect("acked_seq")
            .get(0);
        assert_eq!(acked, 7);
    }

    #[tokio::test]
    async fn statement_timeouts_are_reported_as_timeouts() {
        let Some(db) = migrated().await else {
            return;
        };
        let store = Store::with_config(pool_config(&db.url, SHORT).expect("config"));
        let pooled = store.client().await.expect("pooled connection");
        let started = Instant::now();
        let error = tokio::time::timeout(
            SHORT.statement + SLACK,
            pooled.query_one("SELECT pg_sleep(30)", &[]),
        )
        .await
        .expect("the server ended the statement within its limit")
        .expect_err("the statement outlived its limit");
        assert!(
            started.elapsed() < SHORT.statement + SLACK,
            "{:?}",
            started.elapsed()
        );
        let error = StoreError::from(error);
        assert!(matches!(error, StoreError::Timeout(_)), "{error:?}");
        assert_eq!(error.to_string(), "running a database statement timed out");
    }
}
