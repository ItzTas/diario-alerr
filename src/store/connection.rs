use std::str::FromStr;
use std::time::Duration;

use sea_orm::sqlx::ConnectOptions as _;
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{DatabaseConnection, SqlxPostgresConnector};

use super::StoreError;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const POOL_SIZE: u32 = 4;
pub(crate) const POOL_LIMITS: Limits = Limits {
    statement: Duration::from_secs(20),
    lock: Duration::from_secs(5),
};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) statement: Duration,
    pub(crate) lock: Duration,
}

pub(crate) fn database_options(database_url: &str) -> Result<PgConnectOptions, StoreError> {
    let documented = ["postgres://", "postgresql://"]
        .into_iter()
        .any(|scheme| database_url.starts_with(scheme));
    if !documented {
        return Err(StoreError::InvalidUrl);
    }
    let options = PgConnectOptions::from_str(database_url).map_err(|_| StoreError::InvalidUrl)?;
    if options.get_host().contains(',') {
        return Err(StoreError::InvalidUrl);
    }
    // sqlx sets no TCP keepalive of its own; these let the server drop a monitor that vanished.
    let server_keepalives = [
        ("tcp_keepalives_idle", 30),
        ("tcp_keepalives_interval", 10),
        ("tcp_keepalives_count", 3),
        ("tcp_user_timeout", 60_000),
    ];
    Ok(options
        .disable_statement_logging()
        .options(server_keepalives))
}

pub(crate) fn with_limits(options: PgConnectOptions, limits: Limits) -> PgConnectOptions {
    options.options([
        ("statement_timeout", limits.statement.as_millis()),
        ("lock_timeout", limits.lock.as_millis()),
    ])
}

pub(crate) fn pool(options: PgConnectOptions) -> DatabaseConnection {
    // A minimum, an idle limit or a lifetime makes sqlx spawn a maintenance task, and
    // `Store::new` has to work outside a runtime.
    let pool = PgPoolOptions::new()
        .max_connections(POOL_SIZE)
        .min_connections(0)
        .acquire_timeout(CONNECT_TIMEOUT)
        .idle_timeout(None)
        .max_lifetime(None)
        .test_before_acquire(true)
        .connect_lazy_with(options);
    SqlxPostgresConnector::from_sqlx_postgres_pool(pool)
}
