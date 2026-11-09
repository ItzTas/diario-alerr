package main

import (
	"reflect"
	"testing"
	"time"
)

func TestLoadConfigReadsTheEnvironment(t *testing.T) {
	t.Setenv("WATCH_TERMS", " TALES SABINI , 0021805e,, ")
	t.Setenv("CHECK_INTERVAL_MINUTES", "15")
	t.Setenv("START_AFTER", "2026-11-01")
	t.Setenv("NTFY_URL", "http://ntfy.lan/")
	t.Setenv("PUBLIC_URL", "https://diario.example.com/")
	cfg, err := loadConfig()
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(cfg.terms, []string{"TALES SABINI", "0021805e"}) {
		t.Errorf("terms = %q", cfg.terms)
	}
	if cfg.interval != 15*time.Minute || !cfg.startAfter.Equal(time.Date(2026, 11, 1, 0, 0, 0, 0, time.UTC)) {
		t.Errorf("interval %v startAfter %v", cfg.interval, cfg.startAfter)
	}
	if cfg.ntfyURL != "http://ntfy.lan" || cfg.ntfyTopic != "diario-alerr" || cfg.publicURL != "https://diario.example.com" {
		t.Errorf("ntfy %q topic %q public %q", cfg.ntfyURL, cfg.ntfyTopic, cfg.publicURL)
	}
	if cfg.dataDir != "/data" || cfg.httpPort != "8080" {
		t.Errorf("dataDir %q httpPort %q", cfg.dataDir, cfg.httpPort)
	}
}

func TestLoadConfigRejectsInvalidValues(t *testing.T) {
	cases := map[string]map[string]string{
		"no terms":        {"WATCH_TERMS": " , "},
		"zero interval":   {"WATCH_TERMS": "x", "CHECK_INTERVAL_MINUTES": "0"},
		"text interval":   {"WATCH_TERMS": "x", "CHECK_INTERVAL_MINUTES": "half"},
		"malformed start": {"WATCH_TERMS": "x", "START_AFTER": "01/11/2026"},
	}
	for name, env := range cases {
		t.Run(name, func(t *testing.T) {
			for key, value := range env {
				t.Setenv(key, value)
			}
			if _, err := loadConfig(); err == nil {
				t.Error("loadConfig accepted it")
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
		got, err := parseStartAfter(raw)
		if err != nil || !got.Equal(want) {
			t.Errorf("parseStartAfter(%q) = %v, %v; want %v", raw, got, err, want)
		}
	}
}
