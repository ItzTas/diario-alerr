use std::time::Duration;

use chrono::{DateTime, Utc};
use deadpool_postgres::{Manager, Pool, PoolError, Runtime, TimeoutType};
use monitor_hub_plugin::migrate::{MigrateError, Migration, up};
use tokio::task::JoinHandle;
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
const ACK_ALERTS: &str = "UPDATE monitor_state SET acked_seq = GREATEST(acked_seq, $1)";

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
    Database(#[from] tokio_postgres::Error),
    #[error(transparent)]
    Migrate(#[from] MigrateError),
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

pub struct WriterLock {
    client: Client,
    connection: Option<JoinHandle<Result<(), tokio_postgres::Error>>>,
}

pub async fn acquire_writer_lock(database_url: &str) -> Result<WriterLock, StoreError> {
    let config = parse(database_url)?;
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
        let _ended = connection.await;
        self.connection = None;
    }
}

#[derive(Clone)]
pub struct Store {
    pool: Pool,
}

impl Store {
    pub fn new(database_url: &str) -> Result<Store, StoreError> {
        let manager = Manager::new(parse(database_url)?, NoTls);
        let pool = Pool::builder(manager)
            .max_size(POOL_SIZE)
            .runtime(Runtime::Tokio1)
            .create_timeout(Some(CONNECT_TIMEOUT))
            .wait_timeout(Some(CONNECT_TIMEOUT))
            .build()
            .expect("the pool has a runtime for its timeouts");
        Ok(Store { pool })
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

    pub async fn ack_alerts(&self, seq: i64) -> Result<(), StoreError> {
        self.client().await?.execute(ACK_ALERTS, &[&seq]).await?;
        Ok(())
    }

    async fn client(&self) -> Result<deadpool_postgres::Client, StoreError> {
        self.pool.get().await.map_err(pool_error)
    }
}

fn parse(database_url: &str) -> Result<Config, StoreError> {
    database_url.parse().map_err(|_| StoreError::InvalidUrl)
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
