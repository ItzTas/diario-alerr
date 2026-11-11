---
name: project-structure
description: >-
  File structure, module layering and persistence conventions for diario-alerr (a Rust monitor-hub
  plugin: tonic over the SDK, tokio-postgres + deadpool, goose-format migrations, Postgres). Read
  this before creating or moving a module, integration test, test helper, migration or any
  config/scaffolding file (Cargo.toml, rust-toolchain.toml, plugin.json, Dockerfile, compose.yaml,
  .github/, .githooks/, .env.example), so new code lands where the rest already is.
---

# Project structure: diario-alerr

One crate with a library and a binary. `src/main.rs` only handles the process (arguments, logging,
runtime, exit code), `src/app.rs` puts the pieces together, and each subject has its own module.
Everything is in English: identifiers, files, SQL, logs and user-facing text.

## Directory layout

```
.
├── Cargo.toml                 # package diario-alerr 2.0.0, edition 2024, lib + bin
├── Cargo.lock                 # committed; CI and the image build use --locked
├── rust-toolchain.toml        # the SDK's channel at sdk-rust/v0.1.1, with rustfmt and clippy
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
│   ├── store.rs               # all SQL, Store (deadpool), MIGRATIONS, WriterLock
│   ├── monitor.rs             # trait Gazette, Monitor: prepare, run_cycle
│   ├── scheduler.rs           # periodic loop, trigger, CycleState
│   ├── server.rs              # DiarioPlugin: the plugin contract over the scheduler and the store
│   ├── golden.rs              # tests only: golden!("file") reads testdata/golden
│   └── logtest.rs             # tests only: captures log events on the calling thread
├── sql/
│   ├── schema/                # goose-format migrations, embedded by MIGRATIONS
│   └── rollback/              # mark-alerts-delivered.sql, run before going back to the Go release
├── testdata/golden/           # frozen vectors from the Go release v1.0.0
├── tests/
│   ├── support/               # shared helpers, one file per subject (see Testing)
│   ├── gazette.rs  store.rs  monitor.rs  scheduler.rs  server.rs
│   └── end_to_end.rs  entrypoint.rs  conformance.rs  go_compat.rs
├── .env.example               # every variable, for compose, the tests and a local plugin run
├── .githooks/                 # commit-msg (Conventional Commits) + pre-commit (cargo fmt, cargo test)
├── .github/workflows/         # docker.yml: fmt, clippy and tests against Postgres, then the GHCR image
├── .prototools                # proto: pins Go for the compatibility and conformance tests
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
- No comments in the code. `#[cfg(test)] mod tests` at the bottom of a module is where unit tests
  go.

## Persistence

- `sql/schema/0000N_name.sql` are migrations in goose format (`-- +goose Up` and
  `-- +goose Down`). Adding one means adding the file and its entry in `MIGRATIONS` in
  `src/store.rs`. The SDK's `migrate::up` applies them at start-up on the writer lock's connection
  and keeps goose's version table, which the Go release also reads.
- **A schema change is always a new migration. Never edit one that exists.**
- **Migrations carry schema only.** The `monitor_state` row and the welcome alert are written by
  `Monitor::prepare`.
- The SQL lives as constants in `src/store.rs`, and only `Store` runs it. Nothing inserts an alert
  outside the store's methods. A transaction that inserts an alert locks the state row first, so
  `seq` order is commit order.
- Times are `TIMESTAMPTZ` and map to `DateTime<Utc>`. Integers are `BIGINT`.
- A file in `sql/rollback/` is an operator script for going back to an earlier release. It is not
  a migration and the plugin never runs it.

## Testing

- Write tests that describe what the code does now.
- Unit tests live in their module. Integration tests in `tests/` go through the public API or
  the built binary (`env!("CARGO_BIN_EXE_diario-alerr")`).
- `tests/support/` is shared by every integration test file through `mod support;`, and its
  `mod.rs` carries `#![allow(dead_code)]` because no single test file uses all of it. One file per
  subject: `gazette_site.rs` (fake WordPress site), `pdf.rs` (a hand-written PDF),
  `fake_gazette.rs` (a `Gazette` for cycle tests), `process.rs` (starts the binary, reads the
  handshake, sends signals), `grpc.rs` (calls `Events` on the running binary over its socket),
  `channels.rs` (fake ntfy and Telegram, used only by the Go release) and `go.rs` (builds the Go
  release from the tag `v1.0.0`).
- Database tests get a database from `support::migrated_database()`, or from the test kit's
  `database()` when the test migrates by itself.
- A test that needs something outside the crate (Postgres, `pdftotext`, `sh`, `go`, `git`, the tag
  `v1.0.0`, the conformance command) prints a `skipping:` notice and returns when it is missing,
  and panics when `CI` is set and not empty.
- Golden vectors in `testdata/golden/` came from the Go release and are frozen. A mismatch is a
  bug in the Rust code.
- `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings` and
  `CI=1 cargo test` must pass.

## Tooling

- The toolchain comes from `rust-toolchain.toml`. Keep its channel equal to the SDK's.
- Every dependency is checked on crates.io before it goes into `Cargo.toml`. Unicode crates are
  pinned with `=` so their Unicode version matches the Go release's vectors.
- The image builds on the build machine's platform with `cargo zigbuild` for
  `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. The final stage has only `COPY`
  instructions, so no emulation is needed. `poppler-utils` is not in the image; the hub installs
  it from the `apk` list in `plugin.json`.
- Never change the versions pinned in `.prototools`.
