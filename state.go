package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"time"
)

const (
	maxSeen   = 1000
	maxAlerts = 100
)

type alert struct {
	ID        string   `json:"id"`
	Title     string   `json:"title"`
	Link      string   `json:"link"`
	Body      string   `json:"body"`
	Published string   `json:"published"`
	Delivered []string `json:"delivered"`
}

type state struct {
	Cursor      time.Time  `json:"cursor"`
	Seen        []int      `json:"seen"`
	Alerts      []*alert   `json:"alerts"`
	LastSuccess *time.Time `json:"last_success"`
}

func loadState(path string, defaultCursor time.Time) (*state, bool, error) {
	raw, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return &state{Cursor: defaultCursor, Seen: []int{}, Alerts: []*alert{}}, true, nil
	}
	if err != nil {
		return nil, false, fmt.Errorf("read state: %w", err)
	}
	var st state
	if err := json.Unmarshal(raw, &st); err != nil {
		return nil, false, fmt.Errorf("parse state %s: %w", path, err)
	}
	return &st, false, nil
}

func (s *state) trim() {
	s.Seen = s.Seen[max(0, len(s.Seen)-maxSeen):]
	s.Alerts = s.Alerts[max(0, len(s.Alerts)-maxAlerts):]
}

func (s *state) save(path string) error {
	raw, err := json.MarshalIndent(s, "", "  ")
	if err != nil {
		return fmt.Errorf("encode state: %w", err)
	}
	tmp := path + ".tmp"
	if err := os.WriteFile(tmp, raw, 0o600); err != nil {
		return fmt.Errorf("write state: %w", err)
	}
	if err := os.Rename(tmp, path); err != nil {
		return fmt.Errorf("replace state: %w", err)
	}
	return nil
}
