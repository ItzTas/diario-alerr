package core

import (
	"errors"
	"fmt"
	"io/fs"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/joho/godotenv"
)

const (
	DefaultGazetteAPIURL  = "https://diarios.al.rr.leg.br/wp-json/wp/v2/media"
	DefaultTelegramAPIURL = "https://api.telegram.org"
)

type Config struct {
	DatabaseURL    string
	Terms          []string
	Interval       time.Duration
	StartAfter     time.Time
	StateFile      string
	PublicURL      string
	HTTPPort       string
	GazetteAPIURL  string
	TelegramAPIURL string
	TelegramToken  string
	TelegramChatID string
	NtfyURL        string
	NtfyTopic      string
	NtfyToken      string
}

func Load() (Config, error) {
	if err := godotenv.Load(); err != nil && !errors.Is(err, fs.ErrNotExist) {
		return Config{}, fmt.Errorf("read .env: %w", err)
	}
	return FromEnvironment(time.Now)
}

func FromEnvironment(now func() time.Time) (Config, error) {
	databaseURL := os.Getenv("DATABASE_URL")
	if databaseURL == "" {
		return Config{}, errors.New("DATABASE_URL is empty: set the Postgres address")
	}
	terms := SplitTerms(os.Getenv("WATCH_TERMS"))
	if len(terms) == 0 {
		return Config{}, errors.New("WATCH_TERMS is empty: set at least one name or registration number")
	}
	minutes, err := strconv.ParseFloat(envOr("CHECK_INTERVAL_MINUTES", "30"), 64)
	if err != nil || minutes <= 0 {
		return Config{}, fmt.Errorf("CHECK_INTERVAL_MINUTES must be a positive number: %q", os.Getenv("CHECK_INTERVAL_MINUTES"))
	}
	startAfter, err := ParseStartAfter(os.Getenv("START_AFTER"), now)
	if err != nil {
		return Config{}, err
	}
	return Config{
		DatabaseURL:    databaseURL,
		Terms:          terms,
		Interval:       time.Duration(minutes * float64(time.Minute)),
		StartAfter:     startAfter,
		StateFile:      os.Getenv("STATE_FILE"),
		PublicURL:      strings.TrimRight(os.Getenv("PUBLIC_URL"), "/"),
		HTTPPort:       envOr("HTTP_PORT", "8080"),
		GazetteAPIURL:  envOr("GAZETTE_API_URL", DefaultGazetteAPIURL),
		TelegramAPIURL: strings.TrimRight(envOr("TELEGRAM_API_URL", DefaultTelegramAPIURL), "/"),
		TelegramToken:  os.Getenv("TELEGRAM_BOT_TOKEN"),
		TelegramChatID: os.Getenv("TELEGRAM_CHAT_ID"),
		NtfyURL:        strings.TrimRight(os.Getenv("NTFY_URL"), "/"),
		NtfyTopic:      envOr("NTFY_TOPIC", "diario-alerr"),
		NtfyToken:      os.Getenv("NTFY_TOKEN"),
	}, nil
}

func envOr(key, fallback string) string {
	if value := os.Getenv(key); value != "" {
		return value
	}
	return fallback
}

func SplitTerms(raw string) []string {
	var terms []string
	for term := range strings.SplitSeq(raw, ",") {
		if trimmed := strings.TrimSpace(term); trimmed != "" {
			terms = append(terms, trimmed)
		}
	}
	return terms
}

func ParseStartAfter(raw string, now func() time.Time) (time.Time, error) {
	if raw == "" {
		return now().UTC().Add(-72 * time.Hour), nil
	}
	for _, layout := range []string{time.RFC3339, "2006-01-02T15:04:05", "2006-01-02"} {
		if parsed, err := time.Parse(layout, raw); err == nil {
			return parsed.UTC(), nil
		}
	}
	return time.Time{}, fmt.Errorf("START_AFTER must be a date (2006-01-02) or an RFC 3339 timestamp: %q", raw)
}
