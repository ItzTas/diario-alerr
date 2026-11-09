package coretest

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"sync/atomic"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	_ "github.com/jackc/pgx/v5/stdlib"

	"github.com/ItzTas/diario-alerr/sql/schema"
)

var server struct {
	base        string
	admin       *sql.DB
	template    string
	unavailable error
	databases   atomic.Int64
}

func Run(m *testing.M) int {
	stop, err := start()
	if err != nil {
		if os.Getenv("CI") != "" {
			_, _ = fmt.Fprintln(os.Stderr, "coretest: Postgres is required when CI is set:", err)
			return 1
		}
		server.unavailable = err
		return m.Run()
	}
	code := m.Run()
	if err := stop(); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, "coretest: stop Postgres:", err)
	}
	return code
}

func Database(t testing.TB) string {
	t.Helper()
	if server.admin == nil {
		t.Skipf("Postgres unavailable: %v", server.unavailable)
	}
	name := fmt.Sprintf("diario_alerr_test_%d_%d", os.Getpid(), server.databases.Add(1))
	if _, err := server.admin.ExecContext(t.Context(), "CREATE DATABASE "+name+" TEMPLATE "+server.template); err != nil {
		t.Fatalf("coretest: create a test database: %v", err)
	}
	t.Cleanup(func() {
		if _, err := server.admin.ExecContext(context.Background(), "DROP DATABASE "+name+" WITH (FORCE)"); err != nil {
			t.Errorf("coretest: drop a test database: %v", err)
		}
	})
	address, err := withDatabase(server.base, name)
	if err != nil {
		t.Fatalf("coretest: build the test database address: %v", err)
	}
	return address
}

func Pool(t testing.TB) *pgxpool.Pool {
	t.Helper()
	pool, err := pgxpool.New(t.Context(), Database(t))
	if err != nil {
		t.Fatalf("coretest: open the test database: %v", err)
	}
	t.Cleanup(pool.Close)
	return pool
}

func start() (func() error, error) {
	if base := os.Getenv("TEST_DATABASE_URL"); base != "" {
		return useServer(base, func() error { return nil })
	}
	if _, err := exec.LookPath("initdb"); err != nil {
		return nil, errors.New("set TEST_DATABASE_URL or install PostgreSQL (initdb and postgres)")
	}
	dir, stopCluster, err := startCluster()
	if err != nil {
		return nil, err
	}
	base := (&url.URL{
		Scheme:   "postgres",
		User:     url.User("postgres"),
		Path:     "/postgres",
		RawQuery: url.Values{"host": {dir}, "sslmode": {"disable"}}.Encode(),
	}).String()
	stop, err := useServer(base, stopCluster)
	if err != nil {
		return nil, errors.Join(err, stopCluster())
	}
	return stop, nil
}

func useServer(base string, stopServer func() error) (func() error, error) {
	ctx := context.Background()
	admin, err := sql.Open("pgx", base)
	if err != nil {
		return nil, err
	}
	if err := waitReady(ctx, admin); err != nil {
		return nil, errors.Join(err, admin.Close())
	}
	template := fmt.Sprintf("diario_alerr_template_%d", os.Getpid())
	if err := createTemplate(ctx, admin, base, template); err != nil {
		return nil, errors.Join(err, admin.Close())
	}
	server.base, server.admin, server.template = base, admin, template
	return func() error {
		_, err := admin.ExecContext(context.Background(), "DROP DATABASE IF EXISTS "+template+" WITH (FORCE)")
		return errors.Join(err, admin.Close(), stopServer())
	}, nil
}

func createTemplate(ctx context.Context, admin *sql.DB, base, template string) error {
	if _, err := admin.ExecContext(ctx, "DROP DATABASE IF EXISTS "+template+" WITH (FORCE)"); err != nil {
		return err
	}
	if _, err := admin.ExecContext(ctx, "CREATE DATABASE "+template); err != nil {
		return err
	}
	address, err := withDatabase(base, template)
	if err != nil {
		return err
	}
	if err := schema.Migrate(ctx, address); err != nil {
		return fmt.Errorf("migrate the template database: %w", err)
	}
	return nil
}

func waitReady(ctx context.Context, admin *sql.DB) error {
	deadline := time.Now().Add(30 * time.Second)
	for {
		err := admin.PingContext(ctx)
		if err == nil || time.Now().After(deadline) {
			return err
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func withDatabase(base, name string) (string, error) {
	address, err := url.Parse(base)
	if err != nil {
		return "", err
	}
	address.Path = "/" + name
	return address.String(), nil
}
