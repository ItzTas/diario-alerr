use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::domain::{Alert, Edition, GAZETTE_NAME, SITE_URL, WELCOME_ALERT_ID};
use crate::text::Hits;

const NANOS_PER_MINUTE: u128 = 60_000_000_000;

pub fn welcome_alert(terms: &[String], interval: Duration, now: DateTime<Utc>) -> Alert {
    Alert {
        id: WELCOME_ALERT_ID.to_owned(),
        title: format!("{GAZETTE_NAME} monitor started"),
        link: SITE_URL.to_owned(),
        body: format!(
            "Watching: {}\nChecking every {:.0} min.",
            terms.join(", "),
            minutes(interval)
        ),
        published: now,
    }
}

fn minutes(interval: Duration) -> f64 {
    let nanos = interval.as_nanos();
    (nanos / NANOS_PER_MINUTE) as f64 + (nanos % NANOS_PER_MINUTE) as f64 / 60e9
}

pub fn edition_alert(edition: &Edition, hits: &Hits) -> Alert {
    let lines: Vec<String> = hits
        .order
        .iter()
        .map(|term| {
            let numbers = hits.pages.get(term).map_or(&[][..], Vec::as_slice);
            format!("{term}: {}", format_pages(numbers))
        })
        .collect();
    Alert {
        id: format!("edition-{}", edition.id),
        title: format!(
            "Your name appeared in the {GAZETTE_NAME}: {}",
            edition.title
        ),
        link: format!("{}#page={}", edition.url, hits.first_page),
        body: format!("{}\n\n…{}…", lines.join("\n"), hits.snippet),
        published: edition.published,
    }
}

fn format_pages(numbers: &[usize]) -> String {
    let label = if numbers.len() == 1 { "p." } else { "pp." };
    let joined: Vec<String> = numbers.iter().map(usize::to_string).collect();
    format!("{label} {}", joined.join(", "))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{edition_alert, welcome_alert};
    use crate::config::parse_interval;
    use crate::domain::{Alert, Edition};
    use crate::golden::golden;
    use crate::text::{find_terms, normalize};
    use crate::timestamp::parse_rfc3339;

    fn utc(raw: &str) -> DateTime<Utc> {
        parse_rfc3339(raw).unwrap_or_else(|| panic!("bad timestamp {raw:?}"))
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn edition_alert_describes_pages_and_links_the_first_one() {
        let edition = Edition {
            id: 42,
            published: utc("2026-11-06T20:23:18Z"),
            url: "https://x/ed.pdf".to_owned(),
            title: "DIARIO-ALE-RR-ED.-4763".to_owned(),
        };
        let hits = crate::text::Hits {
            pages: HashMap::from([
                ("TALES SABINI".to_owned(), vec![3, 5]),
                ("0021805e".to_owned(), vec![3]),
            ]),
            order: strings(&["TALES SABINI", "0021805e"]),
            first_page: 3,
            snippet: "SNIPPET".to_owned(),
        };
        let want = Alert {
            id: "edition-42".to_owned(),
            title: "Your name appeared in the ALE/RR Official Gazette: DIARIO-ALE-RR-ED.-4763"
                .to_owned(),
            link: "https://x/ed.pdf#page=3".to_owned(),
            body: "TALES SABINI: pp. 3, 5\n0021805e: p. 3\n\n…SNIPPET…".to_owned(),
            published: edition.published,
        };
        assert_eq!(edition_alert(&edition, &hits), want);
    }

    #[test]
    fn welcome_alert_lists_terms_and_interval() {
        let now = utc("2026-11-09T14:00:00Z");
        let want = Alert {
            id: "welcome".to_owned(),
            title: "ALE/RR Official Gazette monitor started".to_owned(),
            link: "https://diarios.al.rr.leg.br".to_owned(),
            body: "Watching: TALES SABINI, 0021805e\nChecking every 30 min.".to_owned(),
            published: now,
        };
        let terms = strings(&["TALES SABINI", "0021805e"]);
        assert_eq!(
            welcome_alert(&terms, Duration::from_secs(30 * 60), now),
            want
        );
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AlertVectors {
        welcome: Vec<WelcomeCase>,
        edition: Vec<EditionCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct WelcomeCase {
        terms: Vec<String>,
        interval_minutes: String,
        now: String,
        alert: AlertCase,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EditionCase {
        edition: EditionInput,
        raw_pages: Vec<String>,
        terms: Vec<String>,
        alert: Option<AlertCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct EditionInput {
        id: i64,
        published: String,
        url: String,
        title: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AlertCase {
        id: String,
        title: String,
        link: String,
        body: String,
        published: String,
    }

    fn alert_of(case: &AlertCase) -> Alert {
        Alert {
            id: case.id.clone(),
            title: case.title.clone(),
            link: case.link.clone(),
            body: case.body.clone(),
            published: utc(&case.published),
        }
    }

    #[test]
    fn alerts_match_go() {
        let vectors: AlertVectors = golden!("alerts.json");
        assert!(!vectors.welcome.is_empty());
        assert!(!vectors.edition.is_empty());
        for case in &vectors.welcome {
            let interval = parse_interval(&case.interval_minutes).expect("interval");
            let got = welcome_alert(&case.terms, interval, utc(&case.now));
            assert_eq!(got, alert_of(&case.alert), "{:?}", case.interval_minutes);
        }
        for case in &vectors.edition {
            let edition = Edition {
                id: case.edition.id,
                published: utc(&case.edition.published),
                url: case.edition.url.clone(),
                title: case.edition.title.clone(),
            };
            let pages: Vec<String> = case.raw_pages.iter().map(|page| normalize(page)).collect();
            let got = find_terms(&pages, &case.terms).map(|hits| edition_alert(&edition, &hits));
            let want = case.alert.as_ref().map(alert_of);
            assert_eq!(got, want, "{:?}", case.edition.title);
        }
    }
}
