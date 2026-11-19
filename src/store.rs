mod connection;
mod entity;
mod lock;

use std::error::Error;

use chrono::{DateTime, Utc};
use monitor_hub_plugin::Notifier;
use sea_orm::sea_query::{Expr, ExprTrait, OnConflict, PgFunc};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnAcquireErr, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, DbBackend, DbErr, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    RuntimeErr, Statement, TransactionTrait, TryInsertResult, Value,
};

use crate::domain::Alert;
use connection::{Limits, POOL_LIMITS, database_options, pool};
use entity::{alerts, monitor_state, seen_editions};
pub(crate) use lock::LOCK_LOST;
pub use lock::{
    LOCK_CHECK_EVERY, LOCK_CHECK_LIMIT, WRITER_LOCK_ID, WriterLock, acquire_writer_lock,
};

// Raw SQL on purpose: this UPDATE locks the state row before the alert's INSERT, so the order of
// the seq values is the order of the commits.
const ADVANCE_CURSOR: &str = "UPDATE monitor_state SET cursor_at = GREATEST(cursor_at, $1)";
const PRUNE_SEEN_EDITIONS: &str = "DELETE FROM seen_editions
WHERE edition_id NOT IN (
    SELECT edition_id FROM seen_editions ORDER BY edition_id DESC LIMIT $1
)";
const PRUNE_ALERTS: &str = "DELETE FROM alerts
WHERE seq <= (SELECT acked_seq FROM monitor_state)
  AND seq NOT IN (SELECT seq FROM alerts ORDER BY seq DESC LIMIT $1)";
const NEWEST_ALERT_SEQ: &str = "SELECT COALESCE(max(seq), 0) FROM alerts";
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
    #[error("{LOCK_LOST}: {0}")]
    LockLost(String),
    #[error("{0}")]
    Migration(String),
    #[error(transparent)]
    Database(DbErr),
}

impl From<DbErr> for StoreError {
    fn from(error: DbErr) -> Self {
        let Some(step) = timed_out_step(&error) else {
            return StoreError::Database(error);
        };
        StoreError::Timeout(step)
    }
}

fn timed_out_step(error: &DbErr) -> Option<&'static str> {
    if matches!(error, DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)) {
        return Some("getting a database connection");
    }
    let timeouts = [
        ("57014", "running a database statement"),
        ("55P03", "waiting for a database lock"),
    ];
    let code = sql_state(error)?;
    timeouts
        .into_iter()
        .find(|(timeout, _)| code == *timeout)
        .map(|(_, step)| step)
}

fn sql_state(error: &DbErr) -> Option<String> {
    let (DbErr::Exec(RuntimeErr::SqlxError(error))
    | DbErr::Query(RuntimeErr::SqlxError(error))
    | DbErr::Conn(RuntimeErr::SqlxError(error))) = error
    else {
        return None;
    };
    Some(error.as_database_error()?.code()?.into_owned())
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

#[derive(Clone)]
pub struct Store {
    connection: DatabaseConnection,
    notifier: Notifier,
}

impl Store {
    pub fn new(database_url: &str) -> Result<Store, StoreError> {
        Store::with_limits(database_url, POOL_LIMITS)
    }

    fn with_limits(database_url: &str, limits: Limits) -> Result<Store, StoreError> {
        let options = connection::with_limits(database_options(database_url)?, limits);
        Ok(Store {
            connection: pool(options),
            notifier: Notifier::new(),
        })
    }

    pub(crate) fn with_notifier(self, notifier: Notifier) -> Store {
        Store { notifier, ..self }
    }

    pub fn notifier(&self) -> &Notifier {
        &self.notifier
    }

    #[cfg(test)]
    fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }

    // No signal for the welcome: it is written before the hub can read, and the start-up signal
    // and the end of the first read announce it.
    pub async fn initialize(
        &self,
        cursor: DateTime<Utc>,
        welcome: &Alert,
    ) -> Result<bool, StoreError> {
        let transaction = self.connection.begin().await?;
        let state = monitor_state::ActiveModel {
            cursor_at: Set(cursor),
            last_success_at: Set(None),
            ..Default::default()
        };
        let created = monitor_state::Entity::insert(state)
            .on_conflict(
                OnConflict::column(monitor_state::Column::Singleton)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(&transaction)
            .await?
            > 0;
        if created {
            insert_alert(&transaction, welcome).await?;
        }
        transaction.commit().await?;
        Ok(created)
    }

    pub async fn cursor(&self) -> Result<DateTime<Utc>, StoreError> {
        let state = monitor_state::Entity::find().one(&self.connection).await?;
        let state = state.ok_or(StoreError::NotInitialized)?;
        Ok(state.cursor_at)
    }

    pub async fn seen_editions(&self, ids: &[i64]) -> Result<Vec<i64>, StoreError> {
        let listed = Expr::col(seen_editions::Column::EditionId).eq(PgFunc::any(ids.to_vec()));
        Ok(seen_editions::Entity::find()
            .select_only()
            .column(seen_editions::Column::EditionId)
            .filter(listed)
            .into_tuple()
            .all(&self.connection)
            .await?)
    }

    pub async fn record_edition(&self, record: &EditionRecord) -> Result<Option<i64>, StoreError> {
        let transaction = self.connection.begin().await?;
        let seen = seen_editions::ActiveModel {
            edition_id: Set(record.id),
            ..Default::default()
        };
        seen_editions::Entity::insert(seen)
            .on_conflict_do_nothing_on([seen_editions::Column::EditionId])
            .exec_without_returning(&transaction)
            .await?;
        transaction
            .execute_raw(statement(ADVANCE_CURSOR, [record.published.into()]))
            .await?;
        let inserted = match &record.alert {
            Some(alert) => insert_alert(&transaction, alert).await?,
            None => None,
        };
        transaction.commit().await?;
        let Some(seq) = inserted else {
            return Ok(None);
        };
        self.notifier.notify(seq);
        Ok(Some(seq))
    }

    pub async fn mark_success(&self, at: DateTime<Utc>) -> Result<(), StoreError> {
        monitor_state::Entity::update_many()
            .col_expr(monitor_state::Column::LastSuccessAt, Expr::value(at))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    pub async fn last_success(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        let state = monitor_state::Entity::find().one(&self.connection).await?;
        Ok(state.and_then(|state| state.last_success_at))
    }

    pub async fn prune(&self, keep_seen: i64, keep_alerts: i64) -> Result<(), StoreError> {
        let transaction = self.connection.begin().await?;
        transaction
            .execute_raw(statement(PRUNE_SEEN_EDITIONS, [keep_seen.into()]))
            .await?;
        transaction
            .execute_raw(statement(PRUNE_ALERTS, [keep_alerts.into()]))
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn alerts_after(
        &self,
        after: i64,
        limit: i64,
    ) -> Result<Vec<SequencedAlert>, StoreError> {
        let rows = alerts::Entity::find()
            .filter(alerts::Column::Seq.gt(after))
            .order_by_asc(alerts::Column::Seq)
            .limit(u64::try_from(limit).unwrap_or(0))
            .all(&self.connection)
            .await?;
        Ok(rows.into_iter().map(sequenced_alert).collect())
    }

    pub async fn last_issued_seq(&self) -> Result<i64, StoreError> {
        self.read_seq(LAST_ISSUED_SEQ).await
    }

    pub async fn newest_alert_seq(&self) -> Result<i64, StoreError> {
        self.read_seq(NEWEST_ALERT_SEQ).await
    }

    pub async fn ack_alerts(&self, seq: i64) -> Result<(), StoreError> {
        monitor_state::Entity::update_many()
            .col_expr(monitor_state::Column::AckedSeq, Expr::value(seq))
            .filter(monitor_state::Column::AckedSeq.lt(seq))
            .exec(&self.connection)
            .await?;
        Ok(())
    }

    async fn read_seq(&self, sql: &str) -> Result<i64, StoreError> {
        let read = Statement::from_string(DbBackend::Postgres, sql);
        let row = self.connection.query_one_raw(read).await?;
        let row = row.ok_or_else(|| DbErr::RecordNotFound("a sequence read gave no row".into()))?;
        Ok(row.try_get_by_index(0)?)
    }
}

fn statement<const N: usize>(sql: &str, values: [Value; N]) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
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

async fn insert_alert(
    transaction: &DatabaseTransaction,
    alert: &Alert,
) -> Result<Option<i64>, StoreError> {
    let row = alerts::ActiveModel {
        id: Set(alert.id.clone()),
        title: Set(alert.title.clone()),
        link: Set(alert.link.clone()),
        body: Set(alert.body.clone()),
        published_at: Set(alert.published),
        ..Default::default()
    };
    let inserted = alerts::Entity::insert(row)
        .on_conflict_do_nothing_on([alerts::Column::Id])
        .exec(transaction)
        .await?;
    let TryInsertResult::Inserted(inserted) = inserted else {
        return Ok(None);
    };
    Ok(Some(inserted.last_insert_id))
}

fn sequenced_alert(row: alerts::Model) -> SequencedAlert {
    SequencedAlert {
        seq: row.seq,
        alert: Alert {
            id: row.id,
            title: row.title,
            link: row.link,
            body: row.body,
            published: row.published_at,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::postgres::{Database, database};
    use chrono::DateTime;
    use futures_util::future::try_join_all;
    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
    use tokio_postgres::{Client, NoTls};

    use super::connection::{Limits, POOL_LIMITS, POOL_SIZE, database_options, with_limits};
    use super::{Store, StoreError, acquire_writer_lock, chain_text};
    use crate::domain::Alert;
    use crate::logtest::captured;

    const URL: &str = "postgres://diario:hunter2@db.internal:5432/diario";
    const POOL_OPTIONS: &str = "-c tcp_keepalives_idle=30 -c tcp_keepalives_interval=10 -c tcp_keepalives_count=3 -c tcp_user_timeout=60000 -c statement_timeout=20000 -c lock_timeout=5000";
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
        let lock = acquire_writer_lock(&database.url).await.expect("lock");
        lock.migrate().await.expect("migrate");
        Some(database)
    }

    async fn show(connection: &DatabaseConnection, setting: &str) -> String {
        let statement = Statement::from_string(DbBackend::Postgres, format!("SHOW {setting}"));
        connection
            .query_one_raw(statement)
            .await
            .expect("show")
            .expect("a row")
            .try_get_by_index(0)
            .expect("the setting")
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
    fn pool_connections_get_limits_and_server_keepalives() {
        let options = with_limits(database_options(URL).expect("options"), POOL_LIMITS);
        assert_eq!(options.get_options(), Some(POOL_OPTIONS));
    }

    #[test]
    fn the_store_settings_come_after_the_urls_own() {
        let url = with_query(URL, "options=-c%20statement_timeout%3D1&keepalives_idle=5");
        let options = with_limits(database_options(&url).expect("options"), POOL_LIMITS);
        assert_eq!(
            options.get_options(),
            Some(format!("-c statement_timeout=1 {POOL_OPTIONS}").as_str())
        );
    }

    #[test]
    fn database_options_accept_only_the_documented_forms() {
        let accepted = [
            "postgres://diario:hunter2@db.internal:5432/diario?sslmode=disable",
            "postgresql://diario:hunter2@db.internal/diario",
            "postgres://diario:hunter2@db.internal/diario?sslnegotiation=postgres&connect_timeout=5&tcp_user_timeout=5&keepalives=1&keepalives_idle=5&keepalives_interval=5&keepalives_retries=3&target_session_attrs=any&channel_binding=disable&load_balance_hosts=disable",
            "postgres://diario:hunter2@db.internal/diario?options[application_name]=x",
        ];
        for url in accepted {
            let options = database_options(url).expect(url);
            assert_eq!(options.get_host(), "db.internal", "{url}");
            assert_eq!(options.get_database(), Some("diario"), "{url}");
        }
        let refused = [
            "host=db user=diario dbname=diario",
            "postgres://diario:hunter2@h1,h2/diario",
            "postgres://diario:hunter2@h1:5432,h2:5432/diario",
            "mysql://diario:hunter2@db/diario",
            "postgres://diario:hunter2@[bad",
            "postgres://diario:hunter2@db/diario?sslmode=sometimes",
            "postgres://diario:hunter2@db/diario?sslpassword=x",
            "postgres://diario:hunter2@db/diario?Password=x",
        ];
        for url in refused {
            assert!(
                matches!(database_options(url), Err(StoreError::InvalidUrl)),
                "{url}"
            );
        }
    }

    #[test]
    fn url_query_values_never_reach_the_log() {
        let value = "s3cr3t-value";
        let keys = [
            "sslnegotiation",
            "connect_timeout",
            "tcp_user_timeout",
            "keepalives",
            "keepalives_idle",
            "keepalives_interval",
            "keepalives_retries",
            "target_session_attrs",
            "channel_binding",
            "load_balance_hosts",
            "sslpassword",
            "Password",
            "passfile",
        ];
        let events = captured(|| {
            for key in keys {
                let _ = database_options(&with_query(URL, &format!("{key}={value}")));
            }
        });
        let leaked: Vec<&(String, String)> = events
            .iter()
            .flatten()
            .filter(|(_, logged)| logged.contains(value))
            .collect();
        assert!(leaked.is_empty(), "{leaked:?}");
    }

    #[tokio::test]
    async fn pool_connections_carry_their_limits_and_keepalives() {
        let Some(db) = database().await else {
            return;
        };
        let store = Store::new(&db.url).expect("store");
        let settings = [
            ("statement_timeout", "20s"),
            ("lock_timeout", "5s"),
            ("tcp_keepalives_idle", "30"),
            ("tcp_keepalives_interval", "10"),
            ("tcp_keepalives_count", "3"),
            ("tcp_user_timeout", "60000"),
        ];
        for (setting, value) in settings {
            assert_eq!(show(store.connection(), setting).await, value, "{setting}");
        }
    }

    #[tokio::test]
    async fn the_urls_own_options_still_apply() {
        let Some(db) = migrated().await else {
            return;
        };
        let url = with_query(&db.url, "options=-c%20application_name%3Dx");
        let store = Store::new(&url).expect("store");
        assert_eq!(show(store.connection(), "application_name").await, "x");
        assert_eq!(show(store.connection(), "statement_timeout").await, "20s");
    }

    #[tokio::test]
    async fn a_held_state_row_times_out_without_wedging_the_pool() {
        let Some(db) = migrated().await else {
            return;
        };
        let store = Store::with_limits(&db.url, SHORT).expect("store");
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

        // sqlx takes a dropped call's connection back only once its statement ends, so holding
        // every connection at once waits for the abandoned calls to end under the held row.
        let pool = store.connection().get_postgres_connection_pool();
        let whole_pool = try_join_all((0..POOL_SIZE).map(|_| pool.acquire()));
        let held = tokio::time::timeout(SHORT.lock + SLACK, whole_pool)
            .await
            .expect("the abandoned calls ended within the lock timeout")
            .expect("every pooled connection");
        drop(held);

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
        let store = Store::with_limits(&db.url, SHORT).expect("store");
        let sleep = Statement::from_string(DbBackend::Postgres, "SELECT pg_sleep(30)");
        let started = Instant::now();
        let error = tokio::time::timeout(
            SHORT.statement + SLACK,
            store.connection().query_one_raw(sleep),
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
