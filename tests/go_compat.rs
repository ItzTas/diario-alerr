mod support;

use std::path::Path;
use std::time::Duration;

use diario_alerr::gazette::{Client, HTTP_TIMEOUT};
use diario_alerr::monitor::{CycleOutcome, Monitor, MonitorOptions};
use diario_alerr::store::{Store, acquire_writer_lock};
use monitor_hub_plugin_testkit::database;
use tokio::time::{Instant, sleep};
use tokio_postgres::NoTls;

use support::channels::Channels;
use support::gazette_site::{GazetteSite, SiteEdition};
use support::go::go_v1_binary;
use support::process::{RunningService, spawn_service};
use support::{fixed_clock, migrated_database, require_pdftotext, utc};

const MARK_ALERTS_DELIVERED: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/sql/rollback/mark-alerts-delivered.sql"
));
const GO_LIMIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(50);

fn matching_edition(id: i64, published: &str) -> SiteEdition {
    SiteEdition {
        id,
        published: utc(published),
        title: format!("ED {id}"),
        pages: vec!["TALES SABINI".to_owned()],
    }
}

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.expect("connect");
    tokio::spawn(connection);
    client
}

fn rust_monitor(url: &str, site: &GazetteSite) -> Monitor<Client> {
    let options = MonitorOptions {
        terms: vec!["Tales Sabini".to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(utc("2026-11-09T12:00:00Z")),
    };
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).expect("client");
    Monitor::new(Store::new(url).expect("store"), client, options)
}

async fn alert_ids(monitor: &Monitor<Client>) -> Vec<String> {
    monitor
        .store()
        .alerts_after(0, 100)
        .await
        .expect("alerts")
        .into_iter()
        .map(|stored| stored.alert.id)
        .collect()
}

fn start_go(
    binary: &Path,
    url: &str,
    site: &GazetteSite,
    extra: &[(&str, &str)],
) -> RunningService {
    let api_url = site.api_url();
    let mut env = vec![
        ("HTTP_PORT", "0"),
        ("CHECK_INTERVAL_MINUTES", "30"),
        ("WATCH_TERMS", "Tales Sabini"),
        ("START_AFTER", "2026-11-01"),
        ("GAZETTE_API_URL", api_url.as_str()),
        ("DATABASE_URL", url),
    ];
    env.extend_from_slice(extra);
    spawn_service(binary, &env)
}

async fn wait_for(go: &mut RunningService, what: &str, done: impl AsyncFn() -> bool) {
    let deadline = Instant::now() + GO_LIMIT;
    while !done().await {
        go.ensure_running();
        if Instant::now() >= deadline {
            go.fail(&format!(
                "timed out after {GO_LIMIT:?} waiting until {what}"
            ));
        }
        sleep(POLL).await;
    }
}

async fn go_finished_a_cycle(admin: &tokio_postgres::Client) -> bool {
    let row = admin
        .query_one(
            "SELECT (SELECT count(*) FROM alerts), (SELECT count(*) FROM alert_deliveries), (SELECT last_success_at IS NOT NULL FROM monitor_state)",
            &[],
        )
        .await;
    row.is_ok_and(|row| {
        row.get::<_, i64>(0) == 2
            && row.get::<_, i64>(1) == 2
            && row.get::<_, Option<bool>>(2).unwrap_or(false)
    })
}

async fn deliveries(admin: &tokio_postgres::Client) -> Vec<(String, String, String)> {
    admin
        .query(
            "SELECT alert_id, channel, delivered_at::text FROM alert_deliveries ORDER BY alert_id, channel",
            &[],
        )
        .await
        .expect("alert_deliveries")
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect()
}

async fn delivery_channels(admin: &tokio_postgres::Client, alert_id: &str) -> Vec<String> {
    admin
        .query(
            "SELECT channel FROM alert_deliveries WHERE alert_id = $1 ORDER BY channel",
            &[&alert_id],
        )
        .await
        .map(|rows| rows.iter().map(|row| row.get(0)).collect())
        .unwrap_or_default()
}

async fn applied_versions(admin: &tokio_postgres::Client) -> Vec<(i64, i64)> {
    admin
        .query(
            "SELECT version_id, count(*) FROM goose_db_version WHERE is_applied GROUP BY version_id ORDER BY version_id",
            &[],
        )
        .await
        .expect("goose_db_version")
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn rust_continues_a_database_left_by_go() {
    if !require_pdftotext() {
        return;
    }
    let Some(go) = go_v1_binary() else {
        return;
    };
    let Some(db) = database().await else {
        return;
    };
    let site = GazetteSite::start(vec![matching_edition(701, "2026-11-02T10:00:00Z")]).await;
    let admin = connect(&db.url).await;
    let channels = Channels::start().await;
    let ntfy_url = channels.ntfy_url();

    let mut release = start_go(&go, &db.url, &site, &[("NTFY_URL", &ntfy_url)]);
    wait_for(
        &mut release,
        "Go stored two alerts, delivered both on ntfy and recorded a successful check",
        async || go_finished_a_cycle(&admin).await,
    )
    .await;
    let stopped = release.stop();
    assert_eq!(stopped.status.code(), Some(0), "{}", stopped.stdout);
    let left_by_go = deliveries(&admin).await;
    let delivered: Vec<(&str, &str)> = left_by_go
        .iter()
        .map(|(alert, channel, _)| (alert.as_str(), channel.as_str()))
        .collect();
    assert_eq!(delivered, [("edition-701", "ntfy"), ("welcome", "ntfy")]);

    site.publish(matching_edition(702, "2026-11-04T10:00:00Z"));
    let mut lock = acquire_writer_lock(&db.url).await.expect("writer lock");
    assert_eq!(lock.migrate().await.expect("migrate"), vec![2]);
    let monitor = rust_monitor(&db.url, &site);
    monitor.prepare().await.expect("prepare");

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

    assert_eq!(
        alert_ids(&monitor).await,
        ["welcome", "edition-701", "edition-702"]
    );
    assert_eq!(deliveries(&admin).await, left_by_go);
    assert_eq!(applied_versions(&admin).await, vec![(0, 1), (1, 1), (2, 1)]);
    let last_query = site.queries().pop().expect("a listing query");
    assert!(
        last_query.contains("after=2026-11-01T10%3A00%3A00"),
        "{last_query}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn go_sends_nothing_from_the_rust_era_after_rollback() {
    if !require_pdftotext() {
        return;
    }
    let Some(go) = go_v1_binary() else {
        return;
    };
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![matching_edition(801, "2026-11-02T10:00:00Z")]).await;
    {
        let monitor = rust_monitor(&db.url, &site);
        monitor.prepare().await.expect("prepare");
        assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);
        assert_eq!(alert_ids(&monitor).await, ["welcome", "edition-801"]);
    }
    let admin = connect(&db.url).await;
    admin
        .batch_execute(MARK_ALERTS_DELIVERED)
        .await
        .expect("run the rollback SQL");
    site.publish(matching_edition(802, "2026-11-03T10:00:00Z"));
    let channels = Channels::start().await;
    let ntfy_url = channels.ntfy_url();
    let telegram_api_url = channels.telegram_api_url();

    let mut release = start_go(
        &go,
        &db.url,
        &site,
        &[
            ("NTFY_URL", &ntfy_url),
            ("NTFY_TOPIC", "diario-alerr"),
            ("TELEGRAM_BOT_TOKEN", "123:SECRET"),
            ("TELEGRAM_CHAT_ID", "42"),
            ("TELEGRAM_API_URL", &telegram_api_url),
        ],
    );
    wait_for(
        &mut release,
        "Go records edition 802 as delivered on ntfy and telegram",
        async || delivery_channels(&admin, "edition-802").await == ["ntfy", "telegram"],
    )
    .await;
    let stopped = release.stop();

    assert_eq!(
        channels.ntfy_titles(),
        ["Your name appeared in the ALE/RR Official Gazette: ED 802"],
        "{}",
        stopped.stdout
    );
    assert_eq!(channels.telegram_count(), 1, "{}", stopped.stdout);
    let newest: i64 = admin
        .query_one("SELECT max(version_id) FROM goose_db_version", &[])
        .await
        .expect("goose_db_version")
        .get(0);
    assert_eq!(newest, 2);
}
