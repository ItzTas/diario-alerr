use std::future::Future;
use std::time::Duration;

use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{
    ConnAcquireErr, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, DbErr,
    SqlxPostgresConnector, Statement, TransactionTrait,
};
use sea_orm_migration::MigratorTrait;
use tokio::time::{MissedTickBehavior, interval, timeout};

use super::connection::{CONNECT_TIMEOUT, database_options};
use super::{StoreError, chain_text};
use crate::migration::Migrator;

pub const WRITER_LOCK_ID: i64 = 0x6469_6172_696f;
pub const LOCK_CHECK_EVERY: Duration = Duration::from_millis(500);
pub const LOCK_CHECK_LIMIT: Duration = Duration::from_secs(10);
pub(crate) const LOCK_LOST: &str = "the database lock was lost";

const TRY_WRITER_LOCK: &str = "SELECT pg_try_advisory_lock($1), pg_backend_pid()";
// A row only while this session holds the writer lock: a connection without it answers none.
const LOCK_HOLDER: &str = "SELECT pid FROM pg_locks
WHERE locktype = 'advisory'
  AND pid = pg_backend_pid()
  AND granted
  AND objsubid = 1
  AND ((classid::bigint << 32) | objid::bigint) = $1";
const CONNECTING: &str = "connecting to the database for the writer lock";

pub struct WriterLock {
    connection: DatabaseConnection,
    backend: i32,
    close_reason: Option<String>,
}

pub async fn acquire_writer_lock(database_url: &str) -> Result<WriterLock, StoreError> {
    // Keeps Postgres's notices out of the log, such as the migrations' "already exists, skipping".
    let options = database_options(database_url)?.options([("client_min_messages", "warning")]);
    let connection = single_connection(options);
    let Ok(taken) = timeout(CONNECT_TIMEOUT, try_lock(&connection)).await else {
        return Err(StoreError::Timeout(CONNECTING));
    };
    let (locked, backend) = taken?;
    if !locked {
        return Err(StoreError::Locked);
    }
    Ok(WriterLock {
        connection,
        backend,
        close_reason: None,
    })
}

impl WriterLock {
    pub async fn migrate(&self) -> Result<Vec<String>, StoreError> {
        // The check and the migrations share one transaction, so they all run on one connection:
        // the one that took the lock, still holding it.
        let transaction: DatabaseTransaction = answered(self.connection.begin())
            .await
            .map_err(StoreError::LockLost)?;
        self.check(&transaction)
            .await
            .map_err(StoreError::LockLost)?;
        let pending = Migrator::get_pending_migrations(&transaction).await?;
        Migrator::up(&transaction, None)
            .await
            .map_err(migration_error)?;
        transaction.commit().await?;
        Ok(pending
            .iter()
            .map(|migration| migration.name().to_owned())
            .collect())
    }

    pub async fn closed(&mut self) {
        if self.close_reason.is_some() {
            return;
        }
        let mut checks = interval(LOCK_CHECK_EVERY);
        checks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            checks.tick().await;
            if let Err(reason) = self.check(&self.connection).await {
                self.close_reason = Some(reason);
                return;
            }
        }
    }

    pub fn close_reason(&self) -> Option<String> {
        self.close_reason.clone()
    }

    async fn check(&self, on: &impl ConnectionTrait) -> Result<(), String> {
        let holder = answered(lock_holder(on)).await?;
        if holder != Some(self.backend) {
            return Err("the lock's connection was replaced".to_owned());
        }
        Ok(())
    }
}

fn single_connection(options: PgConnectOptions) -> DatabaseConnection {
    // No test before acquire: a failed test would quietly open a new connection, without the lock.
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(CONNECT_TIMEOUT)
        .idle_timeout(None)
        .max_lifetime(None)
        .test_before_acquire(false)
        .connect_lazy_with(options);
    SqlxPostgresConnector::from_sqlx_postgres_pool(pool)
}

async fn try_lock(connection: &DatabaseConnection) -> Result<(bool, i32), StoreError> {
    let statement = Statement::from_sql_and_values(
        DbBackend::Postgres,
        TRY_WRITER_LOCK,
        [WRITER_LOCK_ID.into()],
    );
    let row = connection
        .query_one_raw(statement)
        .await
        .map_err(connect_error)?
        .ok_or_else(|| DbErr::RecordNotFound("the writer lock query gave no row".into()))?;
    Ok((row.try_get_by_index(0)?, row.try_get_by_index(1)?))
}

fn connect_error(error: DbErr) -> StoreError {
    if matches!(error, DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)) {
        return StoreError::Timeout(CONNECTING);
    }
    StoreError::from(error)
}

async fn lock_holder(on: &impl ConnectionTrait) -> Result<Option<i32>, DbErr> {
    let holder =
        Statement::from_sql_and_values(DbBackend::Postgres, LOCK_HOLDER, [WRITER_LOCK_ID.into()]);
    let Some(row) = on.query_one_raw(holder).await? else {
        return Ok(None);
    };
    Ok(Some(row.try_get_by_index(0)?))
}

async fn answered<T>(work: impl Future<Output = Result<T, DbErr>>) -> Result<T, String> {
    let no_answer = format!("the lock's connection did not answer within {LOCK_CHECK_LIMIT:?}");
    let Ok(done) = timeout(LOCK_CHECK_LIMIT, work).await else {
        return Err(no_answer);
    };
    done.map_err(|error| {
        // The pool hands the connection out again only after it answers a ping.
        if matches!(error, DbErr::ConnectionAcquire(ConnAcquireErr::Timeout)) {
            return no_answer;
        }
        format!("the lock's connection failed: {}", chain_text(&error))
    })
}

fn migration_error(error: DbErr) -> StoreError {
    let DbErr::Migration(text) = error else {
        return StoreError::from(error);
    };
    StoreError::Migration(text)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};

    use super::{WRITER_LOCK_ID, WriterLock, acquire_writer_lock, single_connection};
    use crate::logtest::captured;
    use crate::postgres::database;
    use crate::store::connection::database_options;

    async fn one_value<T: sea_orm::TryGetable>(on: &DatabaseConnection, statement: Statement) -> T {
        on.query_one_raw(statement)
            .await
            .expect("query")
            .expect("a row")
            .try_get_by_index(0)
            .expect("a value")
    }

    #[test]
    fn postgres_notices_stay_out_of_the_log() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let Some(db) = runtime.block_on(database()) else {
            return;
        };
        let lock = runtime
            .block_on(acquire_writer_lock(&db.url))
            .expect("lock");

        let events = captured(|| {
            runtime.block_on(lock.migrate()).expect("migrate");
            runtime.block_on(lock.migrate()).expect("migrate again");
        });

        let logged = |text: &str| {
            events
                .iter()
                .any(|event| event.iter().any(|(_, value)| value.contains(text)))
        };
        assert!(logged("No pending migrations"), "{events:?}");
        assert!(!logged("skipping"), "{events:?}");
    }

    #[tokio::test]
    async fn a_lock_released_on_its_own_connection_is_lost() {
        let Some(db) = database().await else {
            return;
        };
        let mut lock = acquire_writer_lock(&db.url).await.expect("lock");
        let unlock = Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_unlock($1)",
            [WRITER_LOCK_ID.into()],
        );
        assert!(one_value::<bool>(&lock.connection, unlock).await);

        let error = lock.migrate().await.expect_err("a released lock migrated");
        assert_eq!(
            error.to_string(),
            "the database lock was lost: the lock's connection was replaced"
        );
        let tables = Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FROM pg_tables WHERE schemaname = current_schema()",
        );
        assert_eq!(one_value::<i64>(&lock.connection, tables).await, 0);
        tokio::time::timeout(Duration::from_secs(5), lock.closed())
            .await
            .expect("closed resolves once the lock is released");
        assert_eq!(
            lock.close_reason().as_deref(),
            Some("the lock's connection was replaced")
        );
    }

    #[tokio::test]
    async fn a_replacement_is_refused_while_the_old_connection_holds_the_lock() {
        let Some(db) = database().await else {
            return;
        };
        let held = acquire_writer_lock(&db.url).await.expect("lock");
        let mut replaced = WriterLock {
            connection: single_connection(database_options(&db.url).expect("options")),
            backend: held.backend,
            close_reason: None,
        };

        let error = replaced
            .migrate()
            .await
            .expect_err("a connection without the lock migrated");
        assert_eq!(
            error.to_string(),
            "the database lock was lost: the lock's connection was replaced"
        );
        let tables = Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FROM pg_tables WHERE schemaname = current_schema()",
        );
        assert_eq!(one_value::<i64>(&held.connection, tables).await, 0);
        tokio::time::timeout(Duration::from_secs(5), replaced.closed())
            .await
            .expect("closed resolves on a connection without the lock");
        assert_eq!(
            replaced.close_reason().as_deref(),
            Some("the lock's connection was replaced")
        );
    }

    #[tokio::test]
    async fn the_lock_connection_carries_keepalives_and_no_limits() {
        let Some(db) = database().await else {
            return;
        };
        let lock = acquire_writer_lock(&db.url).await.expect("lock");
        let settings = [
            ("statement_timeout", "0"),
            ("lock_timeout", "0"),
            ("tcp_keepalives_idle", "30"),
            ("tcp_keepalives_interval", "10"),
            ("tcp_keepalives_count", "3"),
            ("tcp_user_timeout", "60000"),
        ];
        for (setting, value) in settings {
            let show = Statement::from_string(DbBackend::Postgres, format!("SHOW {setting}"));
            let shown: String = lock
                .connection
                .query_one_raw(show)
                .await
                .expect("show")
                .expect("a row")
                .try_get_by_index(0)
                .expect("the setting");
            assert_eq!(shown, value, "{setting}");
        }
    }
}
