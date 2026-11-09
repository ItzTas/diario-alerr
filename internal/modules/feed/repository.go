package feed

import (
	"context"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ItzTas/diario-alerr/internal/core"
	"github.com/ItzTas/diario-alerr/internal/db"
)

type Repository interface {
	RecentAlerts(ctx context.Context, limit int32) ([]core.Alert, error)
}

type Postgres struct {
	queries *db.Queries
}

func NewPostgres(pool *pgxpool.Pool) *Postgres {
	return &Postgres{queries: db.New(pool)}
}

func (r *Postgres) RecentAlerts(ctx context.Context, limit int32) ([]core.Alert, error) {
	rows, err := r.queries.RecentAlerts(ctx, limit)
	if err != nil {
		return nil, err
	}
	alerts := make([]core.Alert, len(rows))
	for i, row := range rows {
		alerts[i] = core.Alert{ID: row.ID, Title: row.Title, Link: row.Link, Body: row.Body, Published: row.PublishedAt.UTC()}
	}
	return alerts, nil
}
