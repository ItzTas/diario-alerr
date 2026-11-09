package health

import (
	"context"
	"errors"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ItzTas/diario-alerr/internal/db"
)

type Repository interface {
	LastSuccess(ctx context.Context) (*time.Time, error)
}

type Postgres struct {
	queries *db.Queries
}

func NewPostgres(pool *pgxpool.Pool) *Postgres {
	return &Postgres{queries: db.New(pool)}
}

func (r *Postgres) LastSuccess(ctx context.Context) (*time.Time, error) {
	last, err := r.queries.LastSuccess(ctx)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, nil
	}
	return last, err
}
