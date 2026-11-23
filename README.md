# Meerkat V1

A locally hosted, scheduled SQL **rising-edge detector** for a warehouse management system. Meerkat executes one read-only Oracle query per configured flow during weekly polling windows, detects newly appearing row identities, and creates an outgoing static-JSON webhook request. The request is delivered independently from polling, with bounded session-local retries and a complete delivery-attempt log.

This is a **fresh codebase**, not a refactoring of the previous Meerkat application. The agreed V1 design and deferred features are documented in [SPEC.md](SPEC.md).

## Workspace

| Crate | Responsibility |
| --- | --- |
| `meerkat-core` | SQL read-only validation, query result types, `QueryExecutor` trait, rising edges |
| `meerkat-schedule` | Pure weekly-window eligibility rules |
| `meerkat-oracle` | Single-account, single-thread blocking Oracle connector |
| `meerkat-store` | SQLite tables, configuration, baselines, history, transactional outbox |
| `meerkat-webhook` | HTTP POST, endpoint/header validation, no redirects/proxies |
| `meerkat-web` | Askama HTML templates, Axum routes, HTMX-enhanced query testing, and static CSS |
| `meerkat` (app) | Startup, capacity-driven coordinator and independent dispatcher |

The workspace uses **Rust 1.100.0, edition 2024**, with the requested workspace-wide Clippy lint policy. Code organization is intended to follow the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html), applying Chromium-specific rules only when appropriate to this standalone application.

## Run

1. Install Rust 1.100.0 (including `rustfmt` and `clippy`). Oracle's native client libraries are needed at runtime for Oracle connectivity; follow [rust-oracle's setup instructions](https://github.com/kubo/rust-oracle).
2. Create the WMS read-only Oracle account or use an existing least-privileged account.
3. Copy `.env.example` to a *local* `.env` file and configure the values. Meerkat automatically loads an optional `.env` at startup via `dotenvy`. Existing process environment variables take precedence over entries in `.env`. Never commit real credentials.
4. From the repository root, run `cargo run --release -p meerkat`.
5. Open `http://127.0.0.1:3000/flows`.
6. Create a flow, paste your actual Oracle `SELECT`, click **Test SQL**, and enter an identity column such as `SHIFT_ID` using the returned column names. For UC-001 check **Trigger if matching rows already exist on the first run**, choose Saturday 13:00–19:00, stop after detection, and configure your Teams/localhost webhook.

The `.env` lookup starts in the **process working directory** and searches its parent directories; it does not depend on the executable's directory. A missing `.env` is fine (for example, when running as a Windows service with environment variables provided by the service manager). An unreadable or malformed `.env` causes startup to fail with an error. Environment variables are parsed and validated together before opening the database or starting Oracle.

Alternatively, provide configuration through actual environment variables, such as this POSIX-shell example:

```sh
export ORACLE_CONNECTION_STRING='host:1521/service'
export ORACLE_USERNAME='readonly_user'
export ORACLE_PASSWORD='example-replace-me'
export MEERKAT_SQLITE_PATH='./meerkat.sqlite'
cargo run --release -p meerkat
```

The `.env.example` file includes all optional settings, including Oracle timeouts, result limit, listener address, and webhook retry settings. `.env` is ignored by Git. The startup configuration is parsed into a typed `Config` in `app/src/config.rs`.

## Semantics to be aware of

- **First successful poll:** Baseline only, unless `trigger_on_first_run` is enabled. An initialized empty baseline is different from an uninitialized one.
- **Rising edges:** A row key present in the latest successful results but absent from the previous successful set. Identical keys cannot appear twice within one result; missing/NULL identity values are errors. Non-key values are ignored.
- **One message per triggering run:** Static JSON only in V1. No dynamic payloads, conditional expressions, or per-row delivery yet.
- **Window stop:** A committed detected event completes that *specific* weekly window when the option is enabled, without waiting for webhook delivery. A query started before the window ends may complete afterward.
- **Oracle errors:** Preserve the last successful baseline. Query results exceeding the configured row limit are rejected, not silently truncated.
- **Webhook retries:** The initial request plus `WEBHOOK_MAX_RETRIES` retries, with bounded exponential backoff. HTTP 2xx is successful; redirects are failures. Each attempt is recorded when it begins and when it ends.
- **Restart safety:** Old `pending` or `sending` messages and `in_progress` attempts become `interrupted`; **they will never be sent automatically on restart**. This sacrifices some delivery reliability to avoid unexpectedly delayed notifications.
- **Flow deletion:** Soft-deletes the flow, schedule and webhook configuration. **Existing outbox messages remain independent** and can still be delivered within their original session.
- **One instance:** An OS file lock alongside SQLite prevents multiple application processes from opening the same database for operations.

## Web UI

`/flows`, `/flows/new`, `/flows/:id`, `/flows/:id/edit`, `/runs`, `/runs/:id`, `/messages`, `/messages/:id`, and `/delivery-attempts`.

Run/message/attempt overviews accept `flow_id`, message listings additionally accept `run_id` and `status`, and attempt listings accept `message_id` as query parameters. Parent-child links are shown directly from each detail page.

## Web UI implementation

The web crate now uses [Askama](https://askama.rs/) (compile-time checked, auto-escaped HTML templates) rather than concatenating HTML strings in Rust. Templates are under `crates/web/templates/`, shared table fragments under `templates/partials/`, and the visual design is in `crates/web/static/styles.css`. Routes, forms, and cross-links stay server rendered; there is no frontend build step.

The SQL test button uses [HTMX 2.0.11](https://htmx.org/) for an in-place results preview when available. The library is loaded from a **version-pinned CDN with Subresource Integrity**; if offline or blocked, the same button falls back to submitting the ordinary HTML form and returning the full editor with its results. No part of the scheduler or delivery system depends on HTMX. To make HTMX work completely offline, vendor the pinned library and serve it locally before deployment.

Key navigation remains: flow → run → generated webhook message → individual delivery attempts. List filters continue to use query parameters, and user-provided SQL, payloads, headers, and errors are auto-escaped by Askama.

## Quality checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release -p meerkat
```

`nursery` lint is denied at the workspace level; other requested panic/unsafe lints are also denied. Oracle-specific *live integration tests* are intentionally omitted. Core/schedule/SQLite behavior can be exercised with an in-memory SQLite store and a fake `QueryExecutor`; the actual WMS query and Oracle connection should be smoke-tested manually.

**Build verification note:** The prior V1 version passed Cargo tests and Clippy on the developer's Windows machine. This Askama/HTMX UI refactor was prepared without a local Rust toolchain, so it requires a fresh `cargo fmt`, `cargo clippy`, and `cargo test` run. The SQL/state logic was not modified.

## Non-goals for V1

Multi-step actions, value-change detectors, expressions, multiple event rules/webhooks per query, templating/loop constructs, continuous scheduling, general cron, multiple schedules per flow, multiple Oracle workers, and manual outbox resend/cancel buttons. The present boundaries allow these to be introduced without teaching the delivery system about Oracle or flow logic.
