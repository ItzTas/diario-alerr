package schema

import (
	"context"
	"database/sql"
	"embed"
	"errors"
	"fmt"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/stdlib"
	"github.com/pressly/goose/v3"
	"github.com/pressly/goose/v3/lock"
)

//go:embed *.sql
var migrations embed.FS

func Migrate(ctx context.Context, databaseURL string) error {
	config, err := pgx.ParseConfig(databaseURL)
	if err != nil {
		return errors.New("schema: DATABASE_URL is not a valid Postgres address")
	}
	locker, err := lock.NewPostgresSessionLocker()
	if err != nil {
		return fmt.Errorf("schema: prepare the migration lock: %w", err)
	}
	db := sql.OpenDB(stdlib.GetConnector(*config))
	return errors.Join(migrate(ctx, db, locker), db.Close())
}

func migrate(ctx context.Context, db *sql.DB, locker lock.SessionLocker) error {
	provider, err := goose.NewProvider(goose.DialectPostgres, db, migrations,
		goose.WithDisableGlobalRegistry(true),
		goose.WithSessionLocker(locker),
	)
	if err != nil {
		return fmt.Errorf("schema: prepare the migrations: %w", err)
	}
	if _, err := provider.Up(ctx); err != nil {
		return fmt.Errorf("schema: run the migrations: %w", err)
	}
	return nil
}
