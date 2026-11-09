package watch

import (
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"time"
)

type stateFile struct {
	Cursor      time.Time  `json:"cursor"`
	Seen        []int64    `json:"seen"`
	LastSuccess *time.Time `json:"last_success"`
	Alerts      []struct {
		ID        string   `json:"id"`
		Title     string   `json:"title"`
		Link      string   `json:"link"`
		Body      string   `json:"body"`
		Published string   `json:"published"`
		Delivered []string `json:"delivered"`
	} `json:"alerts"`
}

func ReadStateFile(path string) (Snapshot, bool, error) {
	raw, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return Snapshot{}, false, nil
	}
	if err != nil {
		return Snapshot{}, false, fmt.Errorf("read the state file: %w", err)
	}
	var file stateFile
	if err := json.Unmarshal(raw, &file); err != nil {
		return Snapshot{}, false, fmt.Errorf("parse the state file %s: %w", path, err)
	}
	snapshot := Snapshot{Cursor: file.Cursor.UTC(), LastSuccess: file.LastSuccess, Seen: file.Seen}
	for _, a := range file.Alerts {
		published, err := time.Parse(time.RFC3339Nano, a.Published)
		if err != nil {
			return Snapshot{}, false, fmt.Errorf("parse the state file %s: alert %s: %w", path, a.ID, err)
		}
		snapshot.Alerts = append(snapshot.Alerts, StoredAlert{
			ID:        a.ID,
			Title:     a.Title,
			Link:      a.Link,
			Body:      a.Body,
			Published: published.UTC(),
			Delivered: a.Delivered,
		})
	}
	return snapshot, true, nil
}
