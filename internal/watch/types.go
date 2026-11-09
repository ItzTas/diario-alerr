package watch

import (
	"context"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/gazette"
)

type Gazette interface {
	ListEditions(ctx context.Context, after time.Time) ([]gazette.Edition, error)
	Scan(ctx context.Context, e gazette.Edition, terms []string) (*gazette.Hits, error)
}

type Snapshot struct {
	Cursor      time.Time
	LastSuccess *time.Time
	Seen        []int64
	Alerts      []StoredAlert
}

type StoredAlert struct {
	core.Alert
	Delivered []string
}

type EditionRecord struct {
	ID        int64
	Published time.Time
	Alert     *core.Alert
}
