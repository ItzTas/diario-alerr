use std::iter;
use std::sync::Arc;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

pub(crate) fn parse_wordpress_time(raw: &str) -> Option<DateTime<Utc>> {
    let (at, rest) = date_time(raw.as_bytes())?;
    rest.is_empty().then(|| at.and_utc())
}

pub(crate) fn parse_rfc3339(raw: &str) -> Option<DateTime<Utc>> {
    let (at, rest) = date_time(raw.as_bytes())?;
    let offset = TimeDelta::try_seconds(zone_offset_seconds(rest)?)?;
    at.checked_sub_signed(offset).map(|utc| utc.and_utc())
}

pub(crate) fn parse_date(raw: &str) -> Option<DateTime<Utc>> {
    let (day, rest) = date(raw.as_bytes())?;
    rest.is_empty()
        .then(|| day.and_time(NaiveTime::MIN).and_utc())
}

pub(crate) fn format_wordpress_time(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn date_time(input: &[u8]) -> Option<(NaiveDateTime, &[u8])> {
    let (day, rest) = date(input)?;
    let (clock, rest) = time(rest.strip_prefix(b"T")?)?;
    Some((day.and_time(clock), rest))
}

fn date(input: &[u8]) -> Option<(NaiveDate, &[u8])> {
    let (year, rest) = number(input, 4)?;
    let (month, rest) = number(rest.strip_prefix(b"-")?, 2)?;
    let (day, rest) = number(rest.strip_prefix(b"-")?, 2)?;
    let date = NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, day)?;
    Some((date, rest))
}

fn time(input: &[u8]) -> Option<(NaiveTime, &[u8])> {
    let (hour, rest) = number(input, hour_width(input))?;
    let (minute, rest) = number(rest.strip_prefix(b":")?, 2)?;
    let (second, rest) = number(rest.strip_prefix(b":")?, 2)?;
    let (nanos, rest) = fraction(rest);
    let time = NaiveTime::from_hms_nano_opt(hour, minute, second, nanos)?;
    Some((time, rest))
}

fn hour_width(input: &[u8]) -> usize {
    if input.get(1).is_some_and(u8::is_ascii_digit) {
        return 2;
    }
    1
}

fn number(input: &[u8], width: usize) -> Option<(u32, &[u8])> {
    let (digits, rest) = input.split_at_checked(width)?;
    digits
        .iter()
        .all(u8::is_ascii_digit)
        .then(|| (digits_value(digits), rest))
}

fn digits_value<'a>(digits: impl IntoIterator<Item = &'a u8>) -> u32 {
    digits
        .into_iter()
        .fold(0, |value, digit| value * 10 + u32::from(digit - b'0'))
}

fn fraction(input: &[u8]) -> (u32, &[u8]) {
    let [b'.' | b',', rest @ ..] = input else {
        return (0, input);
    };
    let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if digits == 0 {
        return (0, input);
    }
    let nanos = digits_value(rest[..digits].iter().chain(iter::repeat(&b'0')).take(9));
    (nanos, &rest[digits..])
}

fn zone_offset_seconds(input: &[u8]) -> Option<i64> {
    if input == b"Z" {
        return Some(0);
    }
    let &[sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] = input else {
        return None;
    };
    let (hours, _) = number(&[h1, h2], 2)?;
    let (minutes, _) = number(&[m1, m2], 2)?;
    if hours > 24 || minutes > 60 {
        return None;
    }
    let seconds = i64::from((hours * 60 + minutes) * 60);
    Some(if sign == b'-' { -seconds } else { seconds })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{format_wordpress_time, parse_date, parse_rfc3339, parse_wordpress_time};
    use crate::golden::golden;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DateCase {
        input: String,
        output: Option<String>,
    }

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw).unwrap().to_utc()
    }

    #[test]
    fn wordpress_time_matches_go() {
        let cases: Vec<DateCase> = golden!("date_gmt.json");
        assert!(!cases.is_empty());
        for case in &cases {
            assert_eq!(
                parse_wordpress_time(&case.input),
                case.output.as_deref().map(utc),
                "parse_wordpress_time({:?})",
                case.input
            );
        }
    }

    #[test]
    fn parsers_follow_go_layout_rules() {
        type Parser = fn(&str) -> Option<DateTime<Utc>>;
        let cases: [(Parser, &str, Option<&str>); 28] = [
            (
                parse_wordpress_time,
                "2026-11-06T20:23:18,5",
                Some("2026-11-06T20:23:18.5Z"),
            ),
            (
                parse_wordpress_time,
                "2026-11-06T20:23:18.1234567899",
                Some("2026-11-06T20:23:18.123456789Z"),
            ),
            (
                parse_wordpress_time,
                "0000-01-01T00:00:00",
                Some("0000-01-01T00:00:00Z"),
            ),
            (
                parse_wordpress_time,
                "2024-02-29T00:00:00",
                Some("2024-02-29T00:00:00Z"),
            ),
            (parse_wordpress_time, "2026-11-06T20:23:18.", None),
            (parse_wordpress_time, "2026-11-06T20:23:18.a", None),
            (parse_wordpress_time, "2026-11-06T120:23:18", None),
            (parse_wordpress_time, "2026-11-06T 8:23:18", None),
            (parse_wordpress_time, "2026-1-06T20:23:18", None),
            (parse_wordpress_time, "2026-11-6T20:23:18", None),
            (parse_wordpress_time, "2026-11-06T20:3:18", None),
            (parse_wordpress_time, "+026-11-06T20:23:18", None),
            (
                parse_rfc3339,
                "2026-11-01T08:30:00,5Z",
                Some("2026-11-01T08:30:00.5Z"),
            ),
            (
                parse_rfc3339,
                "2026-11-01T08:30:00+24:60",
                Some("2026-10-31T07:30:00Z"),
            ),
            (
                parse_rfc3339,
                "2026-11-01T08:30:00-00:60",
                Some("2026-11-01T09:30:00Z"),
            ),
            (parse_rfc3339, "2026-11-01T08:30:00+25:00", None),
            (parse_rfc3339, "2026-11-01T08:30:00+24:61", None),
            (parse_rfc3339, "2026-11-01T08:30:00+0400", None),
            (parse_rfc3339, "2026-11-01T08:30:00+04:00:00", None),
            (parse_rfc3339, "2026-11-01T08:30:00Zx", None),
            (parse_rfc3339, "2026-11-01T08:30:00", None),
            (parse_rfc3339, "2026-11-01T24:00:00Z", None),
            (parse_rfc3339, "2026-11-01T08:30:00.Z", None),
            (parse_date, "2024-02-29", Some("2024-02-29T00:00:00Z")),
            (parse_date, "2026-11-1", None),
            (parse_date, "2026-13-01", None),
            (parse_date, "2026-00-10", None),
            (parse_date, "2026-11-01T", None),
        ];
        for (parse, raw, want) in cases {
            assert_eq!(parse(raw), want.map(utc), "{raw:?}");
        }
    }

    #[test]
    fn wordpress_format_drops_the_fraction() {
        assert_eq!(
            format_wordpress_time(utc("2026-11-01T10:00:00.999Z")),
            "2026-11-01T10:00:00"
        );
        assert_eq!(
            format_wordpress_time(utc("0987-01-02T03:04:05Z")),
            "0987-01-02T03:04:05"
        );
    }
}
