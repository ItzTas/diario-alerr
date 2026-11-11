use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};

use crate::timestamp::{parse_date, parse_rfc3339, parse_wordpress_time};

const DEFAULT_GAZETTE_API_URL: &str = "https://diarios.al.rr.leg.br/wp-json/wp/v2/media";

pub struct Config {
    pub database_url: String,
    pub terms: Vec<String>,
    pub interval: Duration,
    pub start_after: DateTime<Utc>,
    pub gazette_api_url: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("DATABASE_URL is empty: set the Postgres address")]
    MissingDatabaseUrl,
    #[error("WATCH_TERMS is empty: set at least one name or registration number")]
    MissingTerms,
    #[error("CHECK_INTERVAL_MINUTES must be a positive number: {}", go_quote(.0))]
    InvalidInterval(String),
    #[error("START_AFTER must be a date (2006-01-02) or an RFC 3339 timestamp: {}", go_quote(.0))]
    InvalidStartAfter(String),
}

impl Config {
    fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Config, ConfigError> {
        let read = |key: &str| lookup(key).filter(|value| !value.is_empty());
        let database_url = read("DATABASE_URL").ok_or(ConfigError::MissingDatabaseUrl)?;
        let terms = split_terms(&read("WATCH_TERMS").unwrap_or_default());
        if terms.is_empty() {
            return Err(ConfigError::MissingTerms);
        }
        let interval = parse_interval(&read("CHECK_INTERVAL_MINUTES").unwrap_or_default())?;
        let start_after = parse_start_after(&read("START_AFTER").unwrap_or_default(), now)?;
        let gazette_api_url =
            read("GAZETTE_API_URL").unwrap_or_else(|| DEFAULT_GAZETTE_API_URL.to_owned());
        Ok(Config {
            database_url,
            terms,
            interval,
            start_after,
            gazette_api_url,
        })
    }

    pub fn from_env(now: DateTime<Utc>) -> Result<Config, ConfigError> {
        Self::from_lookup(
            |key| std::env::var_os(key).map(|value| value.to_string_lossy().into_owned()),
            now,
        )
    }
}

pub(crate) fn parse_interval(raw: &str) -> Result<Duration, ConfigError> {
    const DEFAULT_INTERVAL_MINUTES: &str = "30";
    let minutes = Some(raw)
        .filter(|raw| !raw.is_empty())
        .unwrap_or(DEFAULT_INTERVAL_MINUTES);
    parse_go_float(minutes)
        .and_then(minutes_to_duration)
        .ok_or_else(|| ConfigError::InvalidInterval(raw.to_owned()))
}

fn minutes_to_duration(minutes: f64) -> Option<Duration> {
    const NANOS_PER_MINUTE: f64 = 60_000_000_000.0;
    const DURATION_LIMIT_NANOS: f64 = 9_223_372_036_854_775_808.0;
    let nanos = (minutes * NANOS_PER_MINUTE).trunc();
    (1.0..DURATION_LIMIT_NANOS)
        .contains(&nanos)
        .then(|| Duration::from_nanos(nanos as u64))
}

fn parse_go_float(raw: &str) -> Option<f64> {
    if !is_go_decimal_float(raw) {
        return None;
    }
    raw.replace('_', "").parse().ok()
}

fn is_go_decimal_float(raw: &str) -> bool {
    let unsigned = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (unsigned, None),
    };
    is_go_mantissa(mantissa)
        && exponent.is_none_or(is_go_exponent)
        && underscores_sit_between_digits(unsigned)
}

fn is_go_mantissa(mantissa: &str) -> bool {
    let bytes = mantissa.as_bytes();
    bytes.iter().any(u8::is_ascii_digit)
        && bytes.iter().filter(|&&byte| byte == b'.').count() <= 1
        && bytes
            .iter()
            .all(|&byte| byte.is_ascii_digit() || byte == b'.' || byte == b'_')
}

fn is_go_exponent(exponent: &str) -> bool {
    let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
    digits.starts_with(|c: char| c.is_ascii_digit())
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'_')
}

fn underscores_sit_between_digits(number: &str) -> bool {
    let bytes = number.as_bytes();
    let is_digit_at = |at: Option<usize>| {
        at.and_then(|at| bytes.get(at))
            .is_some_and(u8::is_ascii_digit)
    };
    bytes
        .iter()
        .enumerate()
        .filter(|&(_, &byte)| byte == b'_')
        .all(|(at, _)| is_digit_at(at.checked_sub(1)) && is_digit_at(Some(at + 1)))
}

fn split_terms(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_start_after(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, ConfigError> {
    if raw.is_empty() {
        return Ok(now - TimeDelta::hours(72));
    }
    [parse_rfc3339, parse_wordpress_time, parse_date]
        .iter()
        .find_map(|parse| parse(raw))
        .ok_or_else(|| ConfigError::InvalidStartAfter(raw.to_owned()))
}

pub(crate) fn go_quote(raw: &str) -> String {
    let mut quoted = String::from("\"");
    for c in raw.chars() {
        push_go_escaped(&mut quoted, c);
    }
    quoted.push('"');
    quoted
}

fn push_go_escaped(quoted: &mut String, c: char) {
    let named = [
        ('\u{7}', "\\a"),
        ('\u{8}', "\\b"),
        ('\u{c}', "\\f"),
        ('\n', "\\n"),
        ('\r', "\\r"),
        ('\t', "\\t"),
        ('\u{b}', "\\v"),
        ('"', "\\\""),
        ('\\', "\\\\"),
    ];
    if let Some((_, escape)) = named.iter().find(|(named, _)| *named == c) {
        quoted.push_str(escape);
        return;
    }
    if is_go_printable(c) {
        quoted.push(c);
        return;
    }
    let code = u32::from(c);
    let escape = match code {
        0..0x80 => format!("\\x{code:02x}"),
        0x80..0x10000 => format!("\\u{code:04x}"),
        _ => format!("\\U{code:08x}"),
    };
    quoted.push_str(&escape);
}

fn is_go_printable(c: char) -> bool {
    if c.is_ascii() {
        return !c.is_ascii_control();
    }
    let probe = format!("a{c}");
    probe.escape_debug().to_string() == probe
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{
        Config, ConfigError, DEFAULT_GAZETTE_API_URL, parse_interval, parse_start_after,
        split_terms,
    };
    use crate::golden::golden;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ConfigVectors {
        split_terms: Vec<SplitTermsCase>,
        start_after: Vec<StartAfterCase>,
        interval: Vec<IntervalCase>,
        required: Vec<RequiredCase>,
        precedence: Vec<PrecedenceCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SplitTermsCase {
        input: String,
        output: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StartAfterCase {
        input: String,
        now: String,
        output: Option<String>,
        error: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct IntervalCase {
        input: String,
        nanoseconds: Option<u128>,
        error: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RequiredCase {
        database_url: String,
        watch_terms: String,
        error: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PrecedenceCase {
        database_url: String,
        watch_terms: String,
        check_interval_minutes: String,
        start_after: String,
        error: Option<String>,
    }

    fn utc(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw).unwrap().to_utc()
    }

    fn now() -> DateTime<Utc> {
        utc("2026-11-09T12:00:00Z")
    }

    fn lookup<'a>(env: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            env.iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn error_of(env: &[(&str, &str)]) -> Option<String> {
        Config::from_lookup(lookup(env), now())
            .err()
            .map(|error| error.to_string())
    }

    fn expected<T>(output: Option<T>, error: Option<&str>, input: &str) -> Result<T, String> {
        match (output, error) {
            (Some(value), None) => Ok(value),
            (None, Some(message)) => Err(message.to_owned()),
            _ => panic!("case {input:?} needs exactly one of output and error"),
        }
    }

    #[test]
    fn config_matches_go() {
        let vectors: ConfigVectors = golden!("config.json");
        assert!(!vectors.split_terms.is_empty());
        assert!(!vectors.start_after.is_empty());
        assert!(!vectors.interval.is_empty());
        assert!(!vectors.required.is_empty());
        assert!(!vectors.precedence.is_empty());
        for case in &vectors.split_terms {
            assert_eq!(split_terms(&case.input), case.output, "{:?}", case.input);
        }
        for case in &vectors.start_after {
            let want = expected(
                case.output.as_deref().map(utc),
                case.error.as_deref(),
                &case.input,
            );
            let got = parse_start_after(&case.input, utc(&case.now)).map_err(|e| e.to_string());
            assert_eq!(got, want, "{:?}", case.input);
        }
        for case in &vectors.interval {
            let want = expected(case.nanoseconds, case.error.as_deref(), &case.input);
            let got = parse_interval(&case.input)
                .map(|interval| interval.as_nanos())
                .map_err(|e| e.to_string());
            assert_eq!(got, want, "{:?}", case.input);
        }
        for case in &vectors.required {
            let env = [
                ("DATABASE_URL", case.database_url.as_str()),
                ("WATCH_TERMS", case.watch_terms.as_str()),
            ];
            assert_eq!(error_of(&env), case.error, "{env:?}");
        }
        for case in &vectors.precedence {
            let env = [
                ("DATABASE_URL", case.database_url.as_str()),
                ("WATCH_TERMS", case.watch_terms.as_str()),
                (
                    "CHECK_INTERVAL_MINUTES",
                    case.check_interval_minutes.as_str(),
                ),
                ("START_AFTER", case.start_after.as_str()),
            ];
            assert_eq!(error_of(&env), case.error, "{env:?}");
        }
    }

    #[test]
    fn from_lookup_reads_every_variable() {
        let env = [
            ("DATABASE_URL", "postgres://x@db/diario"),
            ("WATCH_TERMS", " TALES SABINI , 0021805e,, "),
            ("CHECK_INTERVAL_MINUTES", "0.5"),
            ("START_AFTER", "2026-11-01"),
            ("GAZETTE_API_URL", "http://gazette.lan/media"),
        ];
        let config = Config::from_lookup(lookup(&env), now()).unwrap();
        assert_eq!(config.database_url, "postgres://x@db/diario");
        assert_eq!(config.terms, ["TALES SABINI", "0021805e"]);
        assert_eq!(config.interval, Duration::from_secs(30));
        assert_eq!(config.start_after, utc("2026-11-01T00:00:00Z"));
        assert_eq!(config.gazette_api_url, "http://gazette.lan/media");

        let without_url = Config::from_lookup(lookup(&env[..4]), now()).unwrap();
        assert_eq!(without_url.gazette_api_url, DEFAULT_GAZETTE_API_URL);
        let empty_url = [env[0], env[1], ("GAZETTE_API_URL", "")];
        let with_empty_url = Config::from_lookup(lookup(&empty_url), now()).unwrap();
        assert_eq!(with_empty_url.gazette_api_url, DEFAULT_GAZETTE_API_URL);
    }

    #[test]
    fn starts_three_days_back_without_start_after() {
        let env = [
            ("DATABASE_URL", "postgres://x@db/diario"),
            ("WATCH_TERMS", "x"),
        ];
        let config = Config::from_lookup(lookup(&env), now()).unwrap();
        assert_eq!(config.start_after, utc("2026-11-06T12:00:00Z"));
        assert_eq!(config.interval, Duration::from_secs(30 * 60));
    }

    #[test]
    fn rejects_invalid_values() {
        let database = ("DATABASE_URL", "postgres://db");
        let terms = ("WATCH_TERMS", "x");
        let cases: [(&[(&str, &str)], &str); 5] = [
            (&[terms], "DATABASE_URL is empty: set the Postgres address"),
            (
                &[database, ("WATCH_TERMS", " , ")],
                "WATCH_TERMS is empty: set at least one name or registration number",
            ),
            (
                &[database, terms, ("CHECK_INTERVAL_MINUTES", "0")],
                "CHECK_INTERVAL_MINUTES must be a positive number: \"0\"",
            ),
            (
                &[database, terms, ("CHECK_INTERVAL_MINUTES", "half")],
                "CHECK_INTERVAL_MINUTES must be a positive number: \"half\"",
            ),
            (
                &[database, terms, ("START_AFTER", "01/11/2026")],
                "START_AFTER must be a date (2006-01-02) or an RFC 3339 timestamp: \"01/11/2026\"",
            ),
        ];
        for (env, message) in cases {
            assert_eq!(error_of(env).as_deref(), Some(message), "{env:?}");
        }
    }

    #[test]
    fn parse_start_after_accepts_dates_and_timestamps() {
        let cases = [
            ("2026-11-01", "2026-11-01T00:00:00Z"),
            ("2026-11-01T08:30:00", "2026-11-01T08:30:00Z"),
            ("2026-11-01T08:30:00-04:00", "2026-11-01T12:30:00Z"),
        ];
        for (raw, want) in cases {
            assert_eq!(parse_start_after(raw, now()), Ok(utc(want)), "{raw:?}");
        }
    }

    #[test]
    fn unusable_intervals_are_rejected() {
        let raws = [
            "inf", "-inf", "+Inf", "Infinity", "NaN", "1e300", "1e400", "1e1_0", "1e-12", "1e-400",
            "0x1p4",
        ];
        for raw in raws {
            assert_eq!(
                parse_interval(raw),
                Err(ConfigError::InvalidInterval(raw.to_owned())),
                "{raw:?}"
            );
        }
        let env = [
            ("DATABASE_URL", "postgres://db"),
            ("WATCH_TERMS", "x"),
            ("CHECK_INTERVAL_MINUTES", "1e300"),
        ];
        assert_eq!(
            error_of(&env).as_deref(),
            Some("CHECK_INTERVAL_MINUTES must be a positive number: \"1e300\"")
        );
    }

    #[test]
    fn interval_accepts_the_go_float_syntax() {
        let accepted = [
            ("1.5_0", 90),
            ("0_5", 300),
            ("00.5", 30),
            ("1.", 60),
            ("1E1", 600),
            ("1e+1", 600),
            ("1e01", 600),
            ("1e-1", 6),
        ];
        for (raw, seconds) in accepted {
            assert_eq!(
                parse_interval(raw),
                Ok(Duration::from_secs(seconds)),
                "{raw:?}"
            );
        }
        let rejected = [
            "1__0", "_1", "1_", "1_.5", "1._5", "1e_1", "+_1", ".", "e1", "1e", "1e+", "1..5",
            "0x10", "\u{ff11}", "5 ",
        ];
        for raw in rejected {
            assert_eq!(
                parse_interval(raw),
                Err(ConfigError::InvalidInterval(raw.to_owned())),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn rejected_values_are_quoted_like_go() {
        let cases = [
            ("a\u{7}b", r#""a\ab""#),
            ("tab\there", r#""tab\there""#),
            ("it's", r#""it's""#),
            ("a\u{a0}b", r#""a\u00a0b""#),
            ("a\u{200b}b", r#""a\u200bb""#),
            ("Jos\u{e9}", "\"Jos\u{e9}\""),
            ("a\u{301}", "\"a\u{301}\""),
            ("\u{7f}", r#""\x7f""#),
            ("\u{1f600}", "\"\u{1f600}\""),
            ("\u{e0001}", r#""\U000e0001""#),
            ("back\\slash\"q", r#""back\\slash\"q""#),
            ("\u{85}", r#""\u0085""#),
        ];
        for (raw, quoted) in cases {
            assert_eq!(
                ConfigError::InvalidStartAfter(raw.to_owned()).to_string(),
                format!(
                    "START_AFTER must be a date (2006-01-02) or an RFC 3339 timestamp: {quoted}"
                ),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn the_database_url_never_reaches_formatted_output() {
        trait AmbiguousIfFormattable<Marker> {
            fn check() {}
        }
        impl<T: ?Sized> AmbiguousIfFormattable<()> for T {}
        impl<T: ?Sized + std::fmt::Debug> AmbiguousIfFormattable<u8> for T {}
        impl<T: ?Sized + std::fmt::Display> AmbiguousIfFormattable<u16> for T {}
        <Config as AmbiguousIfFormattable<_>>::check();

        let secret = "s3cr3t-password";
        let url = format!("postgres://diario:{secret}@db/diario");
        let failures = [
            ("WATCH_TERMS", " , "),
            ("CHECK_INTERVAL_MINUTES", "half"),
            ("START_AFTER", "01/11/2026"),
        ];
        for failure in failures {
            let env = [
                failure,
                ("DATABASE_URL", url.as_str()),
                ("WATCH_TERMS", "x"),
            ];
            let error = Config::from_lookup(lookup(&env), now()).err().unwrap();
            assert!(!error.to_string().contains(secret), "{failure:?}");
            assert!(!format!("{error:?}").contains(secret), "{failure:?}");
        }
    }
}
