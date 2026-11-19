---
name: project-structure
description: >-
  File structure, module layering and persistence conventions for diario-alerr (a Rust monitor-hub
  plugin: tonic over the SDK, SeaORM on sqlx with sea-orm-migration, Postgres). Read this before
  creating or moving a module, integration test, test helper, migration or any config/scaffolding
  file (Cargo.toml, rust-toolchain.toml, plugin.json, Dockerfile, compose.yaml, .github/,
  .githooks/, .env.example), so new code lands where the rest already is.
---

# Project structure: diario-alerr

One crate with a library and a binary. `src/main.rs` only handles the process (arguments, logging,
runtime, exit code), `src/app.rs` puts the pieces together, and each subject has its own module.
Everything is in English: identifiers, files, SQL, logs and user-facing text.

## Directory layout

```
.
├── Cargo.toml                 # package diario-alerr 2.1.0, edition 2024, lib + bin
├── Cargo.lock                 # committed; CI and the image build use --locked
├── rust-toolchain.toml        # the SDK's channel at sdk-rust/v0.3.0, with rustfmt and clippy
├── plugin.json                # the manifest the hub reads: name diario, command diario-alerr plugin
├── src/
│   ├── main.rs                # "plugin" argument + hub cookie, logging, runtime, exit code
│   ├── lib.rs                 # module declarations only
│   ├── app.rs                 # start-up: config, pdftotext check, lock, PDF sweep, migrations, first-read seq, prepare, serve
│   ├── config.rs              # Config from the environment, defaults and the Go release's messages
│   ├── timestamp.rs           # Clock; parsing and formatting times with the Go release's rules
│   ├── domain.rs              # Edition, Alert, gazette constants
│   ├── text.rs                # normalize, find_terms, Hits, the snippet
│   ├── alerts.rs              # welcome_alert, edition_alert
│   ├── gazette.rs             # media API client, listing query, GazetteError
│   ├── gazette/html.rs        # HTML entity decoding as Go's html.UnescapeString does it
│   ├── pdf.rs                 # download to a temporary file, pdftotext, its start-up check, leftover sweep
│   ├── store.rs               # Store: the queries, StoreError, the signal after an alert's commit
│   ├── store/
│   │   ├── connection.rs      # DATABASE_URL checks, connection options, the pool
│   │   ├── lock.rs            # WriterLock: the advisory lock, its 0.5 s check, migrate
│   │   ├── entity.rs          # module declarations only
│   │   └── entity/            # SeaORM entities: alerts.rs, monitor_state.rs, seen_editions.rs
│   ├── migration.rs           # Migrator: the migrations, in order
│   ├── migration/
│   │   ├── goose.rs           # catch_up: the goose files a database has not recorded
│   │   ├── m20261118_000001_goose_schema.rs   # adopts the goose schema
│   │   └── m20261118_000002_drop_go_tables.rs # drops alert_deliveries and goose_db_version
│   ├── monitor.rs             # trait Gazette, Monitor: prepare, run_cycle
│   ├── scheduler.rs           # periodic loop, trigger, CycleState
│   ├── server.rs              # DiarioPlugin: the plugin contract over the scheduler and the store
│   ├── golden.rs              # tests only: golden!("file") reads testdata/golden
│   └── logtest.rs             # tests only: captures log events on the calling thread
├── sql/
│   └── schema/                # goose-format files, the first migration's input; never edited
├── testdata/golden/           # frozen vectors from the Go release v1.0.0
├── tests/
│   ├── support/               # shared helpers, one file per subject (see Testing)
│   │   ├── mod.rs             # migrated_database, orm, admin client, writer lock helpers, waits
│   │   ├── postgres.rs        # database(): a database per test, on a throwaway cluster over TCP
│   │   ├── schema.rs          # catalog snapshot, goose-era databases for the migration tests
│   │   ├── proxy.rs           # PausingProxy: a TCP proxy that stops forwarding
│   │   └── fake_gazette.rs  gazette_site.rs  pdf.rs  process.rs
│   ├── gazette.rs  store.rs  migration.rs  monitor.rs  scheduler.rs  server.rs
│   └── end_to_end.rs  entrypoint.rs  conformance.rs
├── .env.example               # every variable, for compose, the tests and a local plugin run
├── .githooks/                 # commit-msg (Conventional Commits) + pre-commit (cargo fmt, cargo test)
├── .github/workflows/         # docker.yml: fmt, clippy and tests against Postgres, then the GHCR image
├── .prototools                # proto: pins Go for the conformance test
├── Dockerfile                 # cross-compiled static musl binary, final stage FROM scratch
├── compose.yaml               # development Postgres only, read-only, no capabilities
└── Makefile                   # check, test, up, down
```

## Modules

- `main.rs` stays thin. It refuses to run outside the hub, starts logging and the runtime, calls
  `app::run`, and turns its error into the single `fatal` log line and exit code 1.
- `app.rs` is the only module that knows every other one. Anything that has to happen before the
  handshake goes in `start_up`, inside the 45 s start-up limit, with a step name for its error.
- `monitor.rs` holds the cycle logic and depends on the `Gazette` trait, not on `gazette::Client`,
  so tests can use a fake gazette. `scheduler.rs` decides when cycles run, and `server.rs` maps
  the scheduler and the store onto the plugin contract.
- A subject module owns its types and its error enum (`thiserror`). Move a helper only when a
  second module needs it.
- `pub` is for what another module or `tests/` uses. The rest is `pub(crate)` or private.
- Comments are short and only where they explain something worth explaining (a guarantee, a rule
  from outside, a pointer to `CLAUDE.md`). None where the code is obvious, and never one that
  repeats the code. `#[cfg(test)] mod tests` at the bottom of a module is where unit tests go.

## Persistence

These are the rules of the Persistence section in `CLAUDE.md`, which also lists the log lines the
database layer writes.

- The plugin reaches Postgres through SeaORM on sqlx. The tables are hand-written entities in
  `src/store/entity/`. The migrations are `sea-orm-migration` modules in `src/migration/`, run in
  the order of `Migrator::migrations`, and `seaql_migrations` records the ones applied.
- The first migration, `m20261118_000001_goose_schema`, adopts the schema that goose built for 2.1
  and the Go release. It reads `goose_db_version` in the current schema and runs the Up section of
  each `sql/schema/` file that table has not recorded. A database without the table is new and
  gets every file. It refuses a gap (a file not recorded while a higher version is) and a table
  with no rows, accepts versions above 2, and never writes a goose row.
- The second, `m20261118_000002_drop_go_tables`, drops `alert_deliveries` and `goose_db_version`
  from that same schema, one statement each and without `CASCADE`. If another object depends on
  one of them, the drop fails, nothing is dropped, and start-up stops with Postgres's error. 2.1
  and the Go release no longer run on a database that went through it.
- A schema change is a new module in `src/migration/` and a new entry at the end of
  `Migrator::migrations`. **Never edit a migration that already exists, nor the files in
  `sql/schema/`**: they are the first migration's input.
- **Migrations carry schema only.** The `monitor_state` row and the welcome alert are written by
  `Monitor::prepare`.
- Start-up runs the migrations on the writer lock's connection, in one transaction that first
  checks that the connection still holds the lock. Each migration's own transaction becomes a
  savepoint inside it, and a run that fails records nothing. No migration can leave that
  transaction: `use_transaction(Some(false))` does not take it out, and a statement Postgres
  refuses inside a transaction block, such as `CREATE INDEX CONCURRENTLY`, makes start-up fail.
- The SQL lives in the store (`src/store.rs` and `src/store/`) and in the migrations
  (`src/migration/`, which reads `sql/schema/`). Nothing inserts an alert outside the store's
  methods. Every transaction that inserts an alert locks the state row first, so the order of
  `seq` values is the order of the commits.
- `Store::record_edition` signals the hub with the new alert's `seq` right after the commit that
  inserted it, never before. A failed commit, or a record without a new alert, signals nothing.
  `Store::initialize` writes the welcome alert without a signal.
- The pool has 4 connections. Each one runs with a 20 s `statement_timeout` and a 5 s
  `lock_timeout`, asks the server for TCP keepalives (`tcp_keepalives_idle=30`,
  `tcp_keepalives_interval=10`, `tcp_keepalives_count=3`, `tcp_user_timeout=60000`, in ms), and
  is tested before each use. Getting a connection has 10 s. The pool keeps no minimum, idle limit
  or lifetime, so sqlx runs no maintenance task and `Store::new` works outside a runtime.
- The writer lock (`pg_try_advisory_lock`) has a connection of its own, outside the pool, with the
  same keepalives, no statement or lock timeout because the migrations run there, and
  `client_min_messages=warning`. It is never tested before use: a failed test would quietly open a
  new connection without the lock. Every 0.5 s, a query with 10 s to answer checks in `pg_locks`
  that this connection still holds the lock. The plugin gives up on a silent connection after
  about 10.5 s, and the server frees a vanished plugin's lock only after about 60 s, so the lock
  cannot pass to a second copy while the first could still write.
- Times are `TIMESTAMPTZ` and map to `DateTime<Utc>`. Integers are `BIGINT`.

## Testing

- Write tests that describe what the code does now.
- Unit tests live in their module. Integration tests in `tests/` go through the public API or
  the built binary (`env!("CARGO_BIN_EXE_diario-alerr")`).
- `tests/support/` is shared by every integration test file through `mod support;`, and its
  `mod.rs` carries `#![allow(dead_code)]` because no single test file uses all of it. One file per
  subject: `gazette_site.rs` (fake WordPress site), `pdf.rs` (a hand-written PDF),
  `fake_gazette.rs` (a `Gazette` for cycle tests), `process.rs` (starts the binary, reads the
  handshake, sends signals), `postgres.rs` (the throwaway Postgres), `schema.rs` (catalog
  snapshots and goose-era databases) and `proxy.rs` (a TCP proxy that can stop forwarding).
- Database tests get a migrated database from `support::migrated_database()`, or an empty one
  from `support::database()` when the test migrates by itself, as `tests/migration.rs` does. Unit
  tests reach the same `postgres.rs` as `crate::postgres`, through a `#[path]` in `src/lib.rs`.
- A test that needs something outside the crate (Postgres, `pdftotext`, `sh`, `go`, the
  conformance command) prints a `skipping:` notice and returns when it is missing, and panics when
  `CI` is set and not empty.
- Golden vectors in `testdata/golden/` came from the Go release and are frozen. A mismatch is a
  bug in the Rust code.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and
  `CI=1 cargo test` must pass.

## Tooling

- The toolchain comes from `rust-toolchain.toml`. Keep its channel equal to the SDK's.
- Every dependency is checked on crates.io before it goes into `Cargo.toml`. Unicode crates are
  pinned with `=` so their Unicode version matches the Go release's vectors.
- `sea-orm` and `sea-orm-migration` are pinned with `=` at 2.0.4, with default features off and
  only the Postgres, tokio and chrono ones on. `sea-orm-migration` turns sea-orm's default
  features back on; that was accepted when the two crates went in. sqlx gets no TLS feature:
  `cargo tree -e features -i sqlx-core | grep -c _tls` stays 0.
- After any change to `Cargo.toml`, set these back with `cargo update -p <crate> --precise
  <version>`: `sqlx` 0.9.0, `nix` 0.31.3, `uuid` 1.29.0, `winnow` 1.0.4, `toml_edit`
  0.25.17+spec-1.1.0 and `toml_datetime` 1.1.2+spec-1.1.0 (`libsqlite3-sys` follows sqlx to
  0.37.0). Then check on crates.io that each new lock entry was published at least two weeks ago,
  and move a newer one back to the release before it with `--precise`. Entries already in the lock
  keep their versions, above all `regex` 1.13.1, `regex-syntax` 0.8.11 and the Unicode crates,
  whose tables the golden vectors depend on.
- `libsqlite3-sys` is in `Cargo.lock` and never compiled. The sqlx features that sea-orm's
  defaults turn on carry weak `sqlx-sqlite?/…` edges, and cargo keeps their targets in the lock
  (cargo#10801). `cargo tree -i libsqlite3-sys --target all -e all` prints nothing. An audit that
  reads the lock still reports its advisory; look again when sqlx 0.9.1 is two weeks old, after
  2026-12-02.
- The image builds on the build machine's platform with `cargo zigbuild` for
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. The final stage has only `COPY`
  instructions, so no emulation is needed. `poppler-utils` is not in the image; the hub installs
  it from the `apk` list in `plugin.json`.
- Never change the versions pinned in `.prototools`.
