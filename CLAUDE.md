# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# diario-alerr

Watches the ALE/RR Official Gazette (the PDF editions published on `diarios.al.rr.leg.br`) for a
list of names or registration numbers, and alerts through Telegram, ntfy and an RSS feed when one of
them shows up. Written in Go. The root `main.go` only builds and starts the pieces; the code lives
in the packages under `internal/`.

**Before creating or moving a directory, package, module, migration or query, read
`.claude/skills/project-structure/SKILL.md`.** It holds the full layout, the module layering and
the persistence workflow.

## Code

- **Do not write comments in the code.** No line, block or doc comments. Names of functions, types
  and variables explain what the code does. Compiler directives such as `//go:embed` and
  `//go:build` are not comments and stay allowed.
- **English everywhere**: identifiers, packages, file and directory names, SQL tables and columns,
  migration names, logs, error messages, the alert text, the feed and the docs.
- Only export what another package uses.

## Commands

```bash
go build ./...
go vet ./...
go test ./...                 # needs initdb/postgres on the machine or TEST_DATABASE_URL
golangci-lint run ./...
go tool sqlc generate         # regenerate internal/db from sql/schema + sql/queries
go tool air                   # hot reload, reads .env (.air.toml)
make check                    # gofmt + vet + golangci-lint + go test -race
docker compose up -d          # app + postgres:18
git config core.hooksPath .githooks   # once per clone
```

- Tests that touch the database go through `internal/core/coretest`. The `TestMain` of each such
  package (`database_test.go`) calls `coretest.Run`, which starts a throwaway Postgres with the
  machine's `initdb` and `postgres` in a `/tmp` directory, killed with the test process, and
  migrates a template database once. Each test gets a fresh copy with `coretest.Database(t)` (an
  address) or `coretest.Pool(t)` (an open pool). With `TEST_DATABASE_URL` set it uses that server
  instead. With `CI` set, a missing Postgres fails the run instead of skipping the tests. A new
  package with database tests needs its own `database_test.go`.
- The scan tests and the root integration tests need `pdftotext` (poppler); they skip without it
  locally and fail without it in CI.
- The root `integration_test.go` builds the whole app with the same `assemble` as `main.go`,
  against a fake gazette site (`internal/gazette/gazettetest`, which also generates the PDFs) and
  fake ntfy/Telegram servers. Restarting is calling `assemble` again on the same database.
- CI (`.github/workflows/docker.yml`) runs gofmt, `go vet` and `go test -race` against a Postgres
  service, then builds the multi-arch image (amd64/arm64) and publishes it to `ghcr.io` on pushes
  to `main` and on `v*` tags.

## Configuration (environment variables)

`core.Load` reads `.env` first when it exists (a missing file is fine) and then the environment.
`.env.example` documents every variable.

- `DATABASE_URL` (required): Postgres address. The schema is migrated at boot.
- `WATCH_TERMS` (required): comma-separated names or registration numbers. Matching ignores case,
  accents and repeated whitespace.
- `CHECK_INTERVAL_MINUTES` (default 30).
- `START_AFTER`: date (`2006-01-02`) or RFC 3339 timestamp of the first edition to consider. Only
  used the first time, when the database has no state yet. Default: three days ago.
- `STATE_FILE`: the `state.json` of earlier, file-based versions. When it exists and the database
  has no state yet, its cursor, seen editions, alerts and deliveries are imported instead of
  sending the welcome alert.
- `PUBLIC_URL`: link of the RSS channel (compose builds it from `PUBLIC_HOST`). `HTTP_PORT`
  (default 8080).
- `TELEGRAM_BOT_TOKEN` + `TELEGRAM_CHAT_ID` turn on Telegram; `NTFY_URL` (+ `NTFY_TOPIC`, default
  `diario-alerr`, and optional `NTFY_TOKEN`) turns on ntfy.
- `GAZETTE_API_URL` and `TELEGRAM_API_URL` override the upstream addresses; only the tests use them.

## Architecture

### Packages

- `internal/core`: what every package uses, one file per subject: `config.go` (`Config`, `Load`),
  `database.go` (`OpenDatabase`: migrate, open the pgx pool, ping), `http.go` (`Get`/`PostJSON`
  with the user agent; errors never carry the URL, so the Telegram token never reaches a log) and
  `alert.go` (`Alert` and the gazette constants).
- `internal/gazette`: the WordPress media API client (`ListEditions`, paginated by
  `X-WP-TotalPages`), PDF download and text extraction through `pdftotext` (`pdf.go`) and the term
  matching (`match.go`: `Normalize`, `FindTerms`, the snippet around the first hit).
- `internal/notify`: one `Sender` per channel (`telegram.go`, `ntfy.go`); `Channels` returns the
  enabled ones by name.
- `internal/watch`: the monitor. `service.go` runs one cycle, `cycle.go` is the periodic job,
  `repository.go` the Postgres access over sqlc, `alerts.go` the alert text, `statefile.go` the
  import of the old `state.json`.
- `internal/modules/feed` (`GET /feed.xml`) and `internal/modules/health` (`GET /healthz`): HTTP
  modules in layers `types` / `repository` / `service` / `handler` / `routes`.
- `internal/db`: generated by sqlc. **Never edit it by hand.**
- `sql/schema`: goose migrations, embedded and applied at boot by `schema.Migrate`.
  `sql/queries`: the sqlc queries.

Who imports whom: everyone may use `core` and `db`; `watch` uses `gazette` and `notify`; the
modules use only `core` and `db`. `main.go` is the only place that knows every package.

### Flow of a cycle

`main.go` loads the config, calls `assemble` (open and migrate the database, build the watcher,
`watch.Service.Prepare`, register the HTTP modules on one `http.ServeMux`) and runs the cycle job
and the HTTP server in an `errgroup` until SIGINT/SIGTERM.

1. `Prepare` creates the single `monitor_state` row the first time: from `STATE_FILE` when it
   exists, otherwise with the cursor at `START_AFTER` and a queued welcome alert.
2. Each cycle lists the PDF editions published after `cursor - 24h`, drops the ones in
   `seen_editions`, and scans the rest two at a time.
3. Results are recorded in publication order. Each edition is marked seen, the cursor only moves
   forward, and a match inserts an alert, all in one transaction. The first edition that fails to
   download or parse stops the recording, so it and the ones after it are retried next cycle.
   `last_success_at` is only written when the whole scan went through.
4. Every enabled channel gets the alerts it has no row for in `alert_deliveries`, oldest first. A
   failed send stops that channel until the next cycle and leaves the others alone.
5. Pruning keeps the newest 1000 seen editions and the newest 100 alerts.

`/healthz` answers 503 when the last successful scan (or the boot, before the first one) is older
than three intervals. The feed lists the newest 100 alerts, newest first.

### Persistence

- A schema change is a new migration `sql/schema/0000N_name.sql` with `-- +goose Up`/`Down`.
  **Never edit a migration that already exists.**
- Queries live in `sql/queries/*.sql`; run `go tool sqlc generate` after changing them or the
  schema and commit the regenerated `internal/db`. `timestamptz` maps to `time.Time` (`*time.Time`
  when nullable), see `sqlc.yaml`.
- Writes that must land together go through the repository's `inTx`.
