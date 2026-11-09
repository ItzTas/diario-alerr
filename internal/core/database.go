package core

import (
	"context"
	"fmt"

	"github.com/jackc/pgx/v5/pgxpool"

	"github.com/ItzTas/diario-alerr/sql/schema"
)

func OpenDatabase(ctx context.Context, databaseURL string) (*pgxpool.Pool, error) {
	if err := schema.Migrate(ctx, databaseURL); err != nil {
		return nil, err
	}
	pool, err := pgxpool.New(ctx, databaseURL)
	if err != nil {
		return nil, fmt.Errorf("open the database: %w", err)
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, fmt.Errorf("reach the database: %w", err)
	}
	return pool, nil
}
