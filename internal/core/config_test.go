package core

import (
	"reflect"
	"testing"
	"time"
)

var fixedNow = func() time.Time { return time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC) }

func TestFromEnvironmentReadsEveryVariable(t *testing.T) {
	t.Setenv("DATABASE_URL", "postgres://x@db/diario")
	t.Setenv("WATCH_TERMS", " TALES SABINI , 0021805e,, ")
	t.Setenv("CHECK_INTERVAL_MINUTES", "15")
	t.Setenv("START_AFTER", "2026-11-01")
	t.Setenv("STATE_FILE", "/data/state.json")
	t.Setenv("NTFY_URL", "http://ntfy.lan/")
	t.Setenv("PUBLIC_URL", "https://diario.example.com/")
	t.Setenv("TELEGRAM_API_URL", "http://telegram.lan/")
	cfg, err := FromEnvironment(fixedNow)
	if err != nil {
		t.Fatal(err)
	}
	want := Config{
		DatabaseURL:    "postgres://x@db/diario",
		Terms:          []string{"TALES SABINI", "0021805e"},
		Interval:       15 * time.Minute,
		StartAfter:     time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC),
		StateFile:      "/data/state.json",
		PublicURL:      "https://diario.example.com",
		HTTPPort:       "8080",
		GazetteAPIURL:  DefaultGazetteAPIURL,
		TelegramAPIURL: "http://telegram.lan",
		NtfyURL:        "http://ntfy.lan",
		NtfyTopic:      "diario-alerr",
	}
	if !reflect.DeepEqual(cfg, want) {
		t.Errorf("config = %+v\nwant     %+v", cfg, want)
	}
}

func TestFromEnvironmentStartsThreeDaysBackWithoutStartAfter(t *testing.T) {
	t.Setenv("DATABASE_URL", "postgres://x@db/diario")
	t.Setenv("WATCH_TERMS", "x")
	cfg, err := FromEnvironment(fixedNow)
	if err != nil {
		t.Fatal(err)
	}
	if want := fixedNow().Add(-72 * time.Hour); !cfg.StartAfter.Equal(want) {
		t.Errorf("start after = %v, want %v", cfg.StartAfter, want)
	}
	if cfg.Interval != 30*time.Minute {
		t.Errorf("interval = %v, want 30m", cfg.Interval)
	}
}

func TestFromEnvironmentRejectsInvalidValues(t *testing.T) {
	cases := map[string]map[string]string{
		"no database":     {"WATCH_TERMS": "x"},
		"no terms":        {"DATABASE_URL": "postgres://db", "WATCH_TERMS": " , "},
		"zero interval":   {"DATABASE_URL": "postgres://db", "WATCH_TERMS": "x", "CHECK_INTERVAL_MINUTES": "0"},
		"text interval":   {"DATABASE_URL": "postgres://db", "WATCH_TERMS": "x", "CHECK_INTERVAL_MINUTES": "half"},
		"malformed start": {"DATABASE_URL": "postgres://db", "WATCH_TERMS": "x", "START_AFTER": "01/11/2026"},
	}
	for name, env := range cases {
		t.Run(name, func(t *testing.T) {
			t.Setenv("DATABASE_URL", "")
			for key, value := range env {
				t.Setenv(key, value)
			}
			if _, err := FromEnvironment(fixedNow); err == nil {
				t.Error("FromEnvironment accepted it")
			}
		})
	}
}

func TestParseStartAfterAcceptsDatesAndTimestamps(t *testing.T) {
	cases := map[string]time.Time{
		"2026-11-01":                time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC),
		"2026-11-01T08:30:00":       time.Date(2026, 11, 1, 8, 30, 0, 0, time.UTC),
		"2026-11-01T08:30:00-04:00": time.Date(2026, 11, 1, 12, 30, 0, 0, time.UTC),
	}
	for raw, want := range cases {
		got, err := ParseStartAfter(raw, fixedNow)
		if err != nil || !got.Equal(want) {
			t.Errorf("ParseStartAfter(%q) = %v, %v; want %v", raw, got, err, want)
		}
	}
}
