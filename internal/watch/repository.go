package watch

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/db"
)

var ErrNotInitialized = errors.New("watch: the monitor state was never initialized")

type Repository interface {
	Initialize(ctx context.Context, snapshot Snapshot) (bool, error)
	Cursor(ctx context.Context) (time.Time, error)
	SeenEditions(ctx context.Context, ids []int64) ([]int64, error)
	RecordEdition(ctx context.Context, record EditionRecord) error
	MarkSuccess(ctx context.Context, at time.Time) error
	PendingAlerts(ctx context.Context, channel string) ([]core.Alert, error)
	MarkDelivered(ctx context.Context, alertID, channel string) error
	Prune(ctx context.Context, keepSeen, keepAlerts int32) error
}

type Postgres struct {
	pool    *pgxpool.Pool
	queries *db.Queries
}

func NewPostgres(pool *pgxpool.Pool) *Postgres {
	return &Postgres{pool: pool, queries: db.New(pool)}
}

func (r *Postgres) Initialize(ctx context.Context, snapshot Snapshot) (created bool, err error) {
	err = r.inTx(ctx, func(q *db.Queries) error {
		rows, err := q.CreateState(ctx, db.CreateStateParams{CursorAt: snapshot.Cursor, LastSuccessAt: snapshot.LastSuccess})
		if err != nil || rows == 0 {
			return err
		}
		created = true
		return restore(ctx, q, snapshot)
	})
	return created, err
}

func restore(ctx context.Context, q *db.Queries, snapshot Snapshot) error {
	for _, id := range snapshot.Seen {
		if err := q.MarkEditionSeen(ctx, id); err != nil {
			return err
		}
	}
	for _, stored := range snapshot.Alerts {
		if err := q.InsertAlert(ctx, insertParams(stored.Alert)); err != nil {
			return err
		}
		for _, channel := range stored.Delivered {
			if err := q.MarkDelivered(ctx, db.MarkDeliveredParams{AlertID: stored.ID, Channel: channel}); err != nil {
				return err
			}
		}
	}
	return nil
}

func (r *Postgres) Cursor(ctx context.Context) (time.Time, error) {
	cursor, err := r.queries.Cursor(ctx)
	if errors.Is(err, pgx.ErrNoRows) {
		return time.Time{}, ErrNotInitialized
	}
	return cursor, err
}

func (r *Postgres) SeenEditions(ctx context.Context, ids []int64) ([]int64, error) {
	return r.queries.SeenEditions(ctx, ids)
}

func (r *Postgres) RecordEdition(ctx context.Context, record EditionRecord) error {
	return r.inTx(ctx, func(q *db.Queries) error {
		if err := q.MarkEditionSeen(ctx, record.ID); err != nil {
			return err
		}
		if err := q.AdvanceCursor(ctx, record.Published); err != nil {
			return err
		}
		if record.Alert == nil {
			return nil
		}
		return q.InsertAlert(ctx, insertParams(*record.Alert))
	})
}

func (r *Postgres) MarkSuccess(ctx context.Context, at time.Time) error {
	return r.queries.MarkSuccess(ctx, &at)
}

func (r *Postgres) PendingAlerts(ctx context.Context, channel string) ([]core.Alert, error) {
	rows, err := r.queries.PendingAlerts(ctx, channel)
	if err != nil {
		return nil, err
	}
	alerts := make([]core.Alert, len(rows))
	for i, row := range rows {
		alerts[i] = core.Alert{ID: row.ID, Title: row.Title, Link: row.Link, Body: row.Body, Published: row.PublishedAt.UTC()}
	}
	return alerts, nil
}

func (r *Postgres) MarkDelivered(ctx context.Context, alertID, channel string) error {
	return r.queries.MarkDelivered(ctx, db.MarkDeliveredParams{AlertID: alertID, Channel: channel})
}

func (r *Postgres) Prune(ctx context.Context, keepSeen, keepAlerts int32) error {
	return r.inTx(ctx, func(q *db.Queries) error {
		if err := q.PruneSeenEditions(ctx, keepSeen); err != nil {
			return err
		}
		return q.PruneAlerts(ctx, keepAlerts)
	})
}

func (r *Postgres) inTx(ctx context.Context, run func(*db.Queries) error) (err error) {
	tx, err := r.pool.Begin(ctx)
	if err != nil {
		return fmt.Errorf("watch: begin a transaction: %w", err)
	}
	defer func() {
		if err != nil {
			err = errors.Join(err, tx.Rollback(context.WithoutCancel(ctx)))
		}
	}()
	if err := run(r.queries.WithTx(tx)); err != nil {
		return err
	}
	if err := tx.Commit(ctx); err != nil {
		return fmt.Errorf("watch: commit: %w", err)
	}
	return nil
}

func insertParams(a core.Alert) db.InsertAlertParams {
	return db.InsertAlertParams{ID: a.ID, Title: a.Title, Link: a.Link, Body: a.Body, PublishedAt: a.Published}
}
