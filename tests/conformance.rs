mod support;

use std::env;
use std::path::Path;
use std::time::Duration;

use diario_alerr::domain::Alert;
use diario_alerr::gazette::{Client, HTTP_TIMEOUT};
use diario_alerr::monitor::{Monitor, MonitorOptions};
use diario_alerr::store::{EditionRecord, Store};
use monitor_hub_plugin_testkit::conformance::{Conformance, Outcome, run};
use tempfile::TempDir;

use support::gazette_site::GazetteSite;
use support::{fixed_clock, migrated_database, require_pdftotext, utc};

const TERMS: &str = "TALES SABINI";

async fn seed_two_alerts(url: &str, api_url: String) {
    let options = MonitorOptions {
        terms: vec![TERMS.to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(utc("2026-11-09T12:00:00Z")),
    };
    let client = Client::new(api_url, HTTP_TIMEOUT).expect("client");
    let monitor = Monitor::new(Store::new(url).expect("store"), client, options);
    monitor.prepare().await.expect("prepare");
    let published = utc("2026-11-02T10:00:00Z");
    let record = EditionRecord {
        id: 401,
        published,
        alert: Some(Alert {
            id: "edition-401".to_owned(),
            title: "Your name appeared in the ALE/RR Official Gazette: ED 401".to_owned(),
            link: "https://diarios.al.rr.leg.br/pdf/401#page=1".to_owned(),
            body: "TALES SABINI: p. 1".to_owned(),
            published,
        }),
    };
    monitor
        .store()
        .record_edition(&record)
        .await
        .expect("record edition 401");
}

fn text(path: &Path) -> String {
    path.to_str()
        .expect("a UTF-8 temporary directory")
        .to_owned()
}

fn plugin_conformance(url: &str, api_url: &str, sockets: &TempDir, data: &TempDir) -> Conformance {
    let mut conformance = Conformance::new(env!("CARGO_BIN_EXE_diario-alerr"));
    conformance.command.push("plugin".to_owned());
    let path = env::var("PATH").unwrap_or_default();
    conformance.env = [
        ("DATABASE_URL", url.to_owned()),
        ("WATCH_TERMS", TERMS.to_owned()),
        ("GAZETTE_API_URL", api_url.to_owned()),
        ("CHECK_INTERVAL_MINUTES", "30".to_owned()),
        ("PATH", path),
        ("TZ", "America/Boa_Vista".to_owned()),
        ("TMPDIR", text(sockets.path())),
        ("MONITOR_HUB_DATA_DIR", text(data.path())),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect();
    conformance.min_events = 2;
    conformance.ready_timeout = Duration::from_secs(10);
    conformance
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_passes_the_hub_conformance_suite() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![]).await;
    seed_two_alerts(&db.url, site.api_url()).await;
    let sockets = tempfile::tempdir().expect("socket directory");
    let data = tempfile::tempdir().expect("data directory");
    let conformance = plugin_conformance(&db.url, &site.api_url(), &sockets, &data);

    let outcome = tokio::task::spawn_blocking(move || run(&conformance))
        .await
        .expect("join the conformance run");

    match outcome {
        Outcome::Passed => {}
        Outcome::Skipped(reason) => eprintln!("skipping: {reason}"),
        Outcome::Failed(output) => panic!("{output}"),
        other => panic!("unexpected conformance outcome: {other:?}"),
    }
}
