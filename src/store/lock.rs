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
const BACKEND: &str = "SELECT pg_backend_pid()";
const CONNECTING: &str = "connecting to the database for the writer lock";

pub struct WriterLock {
    connection: DatabaseConnection,
    backend: i32,
    close_reason: Option<String>,
}

pub async fn acquire_writer_lock(database_url: &str) -> Result<WriterLock, StoreError> {
    let connection = single_connection(database_options(database_url)?);
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
        let backend = answered(backend_pid(on)).await?;
        if backend != self.backend {
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

async fn backend_pid(on: &impl ConnectionTrait) -> Result<i32, DbErr> {
    on.query_one_raw(Statement::from_string(DbBackend::Postgres, BACKEND))
        .await?
        .ok_or_else(|| DbErr::RecordNotFound("pg_backend_pid() gave no row".into()))?
        .try_get_by_index(0)
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
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    use super::acquire_writer_lock;
    use crate::postgres::database;

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
