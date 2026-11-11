mod support;

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use diario_alerr::gazette::{Client, HTTP_TIMEOUT};
use diario_alerr::monitor::{CycleOutcome, Monitor, MonitorOptions};
use diario_alerr::store::{SequencedAlert, Store};

use support::gazette_site::{GazetteSite, SiteEdition};
use support::{fixed_clock, migrated_database, require_pdftotext, utc};

fn now() -> DateTime<Utc> {
    utc("2026-11-09T12:00:00Z")
}

fn site_edition(id: i64, published: &str, title: &str, pages: &[&str]) -> SiteEdition {
    SiteEdition {
        id,
        published: utc(published),
        title: title.to_owned(),
        pages: pages.iter().map(|page| (*page).to_owned()).collect(),
    }
}

async fn prepared(
    url: &str,
    site: &GazetteSite,
    term: &str,
    now: DateTime<Utc>,
) -> Monitor<Client> {
    let options = MonitorOptions {
        terms: vec![term.to_owned()],
        interval: Duration::from_secs(30 * 60),
        start_after: utc("2026-11-01T00:00:00Z"),
        clock: fixed_clock(now),
    };
    let client = Client::new(site.api_url(), HTTP_TIMEOUT).expect("client");
    let monitor = Monitor::new(Store::new(url).expect("store"), client, options);
    monitor.prepare().await.expect("prepare");
    monitor
}

async fn alerts(monitor: &Monitor<Client>) -> Vec<SequencedAlert> {
    monitor.store().alerts_after(0, 1000).await.expect("alerts")
}

#[tokio::test]
async fn monitor_alerts_when_a_name_appears() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![
        site_edition(501, "2026-11-02T10:00:00Z", "ED 501", &["Nada aqui"]),
        site_edition(
            502,
            "2026-11-03T10:00:00Z",
            "ED 502",
            &["Sumario", "Nomeacao de TALES SABINI"],
        ),
    ])
    .await;
    let monitor = prepared(&db.url, &site, "Tales Sabini", now()).await;

    assert_eq!(monitor.run_cycle().await, CycleOutcome::Complete);

    let alerts = alerts(&monitor).await;
    let titles: Vec<&str> = alerts
        .iter()
        .map(|stored| stored.alert.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "ALE/RR Official Gazette monitor started",
            "Your name appeared in the ALE/RR Official Gazette: ED 502",
        ]
    );
    assert_eq!(
        alerts[1].alert.link,
        format!("{}#page=2", site.pdf_url(502))
    );
    assert!(
        alerts[1].alert.body.contains("Tales Sabini: p. 2"),
        "{:?}",
        alerts[1].alert.body
    );
}

#[tokio::test]
async fn monitor_remembers_what_it_saw_across_restarts() {
    if !require_pdftotext() {
        return;
    }
    let Some(db) = migrated_database().await else {
        return;
    };
    let site = GazetteSite::start(vec![site_edition(
        601,
        "2026-11-02T10:00:00Z",
        "ED 601",
        &["TALES SABINI"],
    )])
    .await;
    let first = prepared(&db.url, &site, "TALES SABINI", now()).await;
    assert_eq!(first.run_cycle().await, CycleOutcome::Complete);

    site.publish(site_edition(
        602,
        "2026-11-04T10:00:00Z",
        "ED 602",
        &["TALES SABINI aparece de novo"],
    ));
    let restarted = prepared(&db.url, &site, "TALES SABINI", now() + TimeDelta::hours(1)).await;
    assert_eq!(restarted.run_cycle().await, CycleOutcome::Complete);

    let ids: Vec<String> = alerts(&restarted)
        .await
        .into_iter()
        .map(|stored| stored.alert.id)
        .collect();
    assert_eq!(ids, ["welcome", "edition-601", "edition-602"]);
    let last_query = site.queries().pop().expect("a listing query");
    assert!(
        last_query.contains("after=2026-11-01T10%3A00%3A00"),
        "{last_query}"
    );
}
