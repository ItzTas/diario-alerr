use std::time::Duration;

// The url crate's type, the one sqlx parses addresses with.
use reqwest::Url;
use sea_orm::sqlx::ConnectOptions as _;
use sea_orm::sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sea_orm::{DatabaseConnection, SqlxPostgresConnector};

use super::StoreError;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const NOT_A_POSTGRES_ADDRESS: &str = "DATABASE_URL is not a valid Postgres address";
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
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    let url =
        Url::parse(database_url).map_err(|_| StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS))?;
    let options = PgConnectOptions::from_url(&keys_sqlx_reads(url)?)
        .map_err(|_| StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS))?;
    if options.get_host().contains(',') {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
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

// sqlx logs every query key it does not read, value included, so it only gets the keys it reads.
fn keys_sqlx_reads(mut url: Url) -> Result<Url, StoreError> {
    let read = [
        "sslmode",
        "ssl-mode",
        "sslrootcert",
        "ssl-root-cert",
        "ssl-ca",
        "sslcert",
        "ssl-cert",
        "sslkey",
        "ssl-key",
        "statement-cache-capacity",
        "host",
        "hostaddr",
        "port",
        "dbname",
        "user",
        "password",
        "application_name",
        "options",
    ];
    // Accepted by 2.1's client; sqlx has no equivalent, so they are left out.
    let left_out = [
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
    ];
    let read_by_sqlx =
        |key: &str| read.contains(&key) || (key.starts_with("options[") && key.ends_with(']'));
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs
        .iter()
        .any(|(key, _)| !read_by_sqlx(key) && !left_out.contains(&key.as_str()))
    {
        return Err(StoreError::InvalidUrl(NOT_A_POSTGRES_ADDRESS));
    }
    // sqlx cannot honour other values of these; left out, they would connect without what was asked.
    let honoured_only_as = [
        (
            "channel_binding",
            &["disable", "prefer"][..],
            "DATABASE_URL's channel_binding needs TLS, and the plugin connects without TLS",
        ),
        (
            "sslnegotiation",
            &["postgres"][..],
            "DATABASE_URL's sslnegotiation needs TLS, and the plugin connects without TLS",
        ),
        (
            "target_session_attrs",
            &["any"][..],
            "DATABASE_URL's target_session_attrs needs a session check the plugin does not run",
        ),
    ];
    let not_honoured = pairs.iter().find_map(|(key, value)| {
        honoured_only_as
            .iter()
            .find(|(name, values, _)| name == key && !values.contains(&value.as_str()))
            .map(|(_, _, refusal)| *refusal)
    });
    if let Some(refusal) = not_honoured {
        return Err(StoreError::InvalidUrl(refusal));
    }
    url.set_query(None);
    url.query_pairs_mut()
        .extend_pairs(pairs.iter().filter(|(key, _)| read_by_sqlx(key)));
    Ok(url)
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
