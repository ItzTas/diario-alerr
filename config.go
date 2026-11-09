package main

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

type config struct {
	terms          []string
	telegramToken  string
	telegramChatID string
	ntfyURL        string
	ntfyTopic      string
	ntfyToken      string
	interval       time.Duration
	startAfter     time.Time
	dataDir        string
	publicURL      string
	httpPort       string
}

func loadConfig() (config, error) {
	terms := splitTerms(os.Getenv("WATCH_TERMS"))
	if len(terms) == 0 {
		return config{}, errors.New("WATCH_TERMS is empty: set at least one name or registration number")
	}
	minutes, err := strconv.ParseFloat(envOr("CHECK_INTERVAL_MINUTES", "30"), 64)
	if err != nil || minutes <= 0 {
		return config{}, fmt.Errorf("CHECK_INTERVAL_MINUTES must be a positive number: %q", os.Getenv("CHECK_INTERVAL_MINUTES"))
	}
	startAfter, err := parseStartAfter(os.Getenv("START_AFTER"))
	if err != nil {
		return config{}, err
	}
	return config{
		terms:          terms,
		telegramToken:  os.Getenv("TELEGRAM_BOT_TOKEN"),
		telegramChatID: os.Getenv("TELEGRAM_CHAT_ID"),
		ntfyURL:        strings.TrimRight(os.Getenv("NTFY_URL"), "/"),
		ntfyTopic:      envOr("NTFY_TOPIC", "diario-alerr"),
		ntfyToken:      os.Getenv("NTFY_TOKEN"),
		interval:       time.Duration(minutes * float64(time.Minute)),
		startAfter:     startAfter,
		dataDir:        envOr("DATA_DIR", "/data"),
		publicURL:      strings.TrimRight(os.Getenv("PUBLIC_URL"), "/"),
		httpPort:       envOr("HTTP_PORT", "8080"),
	}, nil
}

func envOr(key, fallback string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return fallback
}

func splitTerms(raw string) []string {
	var terms []string
	for term := range strings.SplitSeq(raw, ",") {
		if trimmed := strings.TrimSpace(term); trimmed != "" {
			terms = append(terms, trimmed)
		}
	}
	return terms
}

func parseStartAfter(raw string) (time.Time, error) {
	if raw == "" {
		return time.Now().UTC().Add(-72 * time.Hour), nil
	}
	for _, layout := range []string{time.RFC3339, "2006-01-02T15:04:05", "2006-01-02"} {
		if parsed, err := time.Parse(layout, raw); err == nil {
			return parsed.UTC(), nil
		}
	}
	return time.Time{}, fmt.Errorf("START_AFTER must be a date (2006-01-02) or an RFC 3339 timestamp: %q", raw)
}
