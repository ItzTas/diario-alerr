package core

import (
	"testing"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core/coretest"
)

func TestGoldenVectors(t *testing.T) {
	coretest.Golden(t, "config.json", configVectors(t))
}

type splitCase struct {
	Input  string   `json:"input"`
	Output []string `json:"output"`
}

type startAfterCase struct {
	Input  string  `json:"input"`
	Now    string  `json:"now"`
	Output *string `json:"output"`
}

type intervalCase struct {
	Input       string `json:"input"`
	Nanoseconds *int64 `json:"nanoseconds"`
}

type configVectorSet struct {
	SplitTerms []splitCase      `json:"split_terms"`
	StartAfter []startAfterCase `json:"start_after"`
	Interval   []intervalCase   `json:"interval"`
}

func configVectors(t *testing.T) configVectorSet {
	var set configVectorSet
	for _, input := range []string{" TALES SABINI , 0021805e,, ", "", " , ,", "a,,b", " José ", "a;b"} {
		output := SplitTerms(input)
		if output == nil {
			output = []string{}
		}
		set.SplitTerms = append(set.SplitTerms, splitCase{Input: input, Output: output})
	}

	now := time.Date(2026, 11, 9, 12, 0, 0, 0, time.UTC)
	for _, input := range []string{
		"", "2026-11-01", "2026-11-01T08:30:00", "2026-11-01T08:30:00-04:00", "2026-11-01T08:30:00Z",
		"2026-11-01T08:30:00.25+00:00", "2026-11-01T08:30:00.1234567891Z", "2026-11-01t08:30:00z",
		"2026-11-01 08:30:00Z", "2026-11-01T8:30:00Z", "2026-11-01T08:30Z", "2026-11-01T08:30:00+24:00",
		"01/11/2026", "2026-02-30",
	} {
		c := startAfterCase{Input: input, Now: now.Format(time.RFC3339Nano)}
		if parsed, err := ParseStartAfter(input, func() time.Time { return now }); err == nil {
			formatted := parsed.UTC().Format(time.RFC3339Nano)
			c.Output = &formatted
		}
		set.StartAfter = append(set.StartAfter, c)
	}

	for _, input := range []string{"", "30", "0.5", "+5", "3e1", ".5", "5.", "1.5e-3", "0", "-1", "half", " 30", "1_0"} {
		set.Interval = append(set.Interval, intervalCase{Input: input, Nanoseconds: intervalNanoseconds(t, input)})
	}
	return set
}

func intervalNanoseconds(t *testing.T, input string) *int64 {
	t.Setenv("DATABASE_URL", "x")
	t.Setenv("WATCH_TERMS", "x")
	t.Setenv("CHECK_INTERVAL_MINUTES", input)
	t.Setenv("START_AFTER", "")
	cfg, err := FromEnvironment(time.Now)
	if err != nil {
		return nil
	}
	nanoseconds := int64(cfg.Interval)
	return &nanoseconds
}
