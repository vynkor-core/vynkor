# Database Plugin Design

Date: 2026-07-15

## Goal

Give Veyron plugins/kernel parameterized SQL access to a database, via a
dedicated plugin (`database`) that declares `PERMISSION_DATABASE` and
exposes `db_query`, `db_execute`, and `db_transaction` actions.

The plugin is configured with exactly **one** backend connection at
startup (via env var / config, not per-call) — callers never choose or
supply a connection string; they only ever talk to whichever DB the
kernel operator configured for this plugin instance.

## Scope

- v1 backends: Postgres, SQLite, MySQL, via `sqlx::Any` (driver selected
  automatically from the configured DSN's scheme).
- v1 actions: `db_query` (SELECT), `db_execute` (single INSERT/UPDATE/
  DELETE), `db_transaction` (batch of DML statements, all-or-nothing).
- Out of scope for v1: DDL (CREATE/ALTER/DROP/GRANT/...), schema
  introspection actions, per-call connection strings (multi-tenant),
  streaming/cursor-based large result sets, SELECT statements inside a
  transaction.
- Connection drops: `sqlx`'s pool reconnects transparently on its own;
  no plugin-level reconnect/hot-reload logic needed or built in v1.

## Location

New crate: `veyron-plugins/plugins/database/`.

```
veyron-plugins/plugins/database/
  Cargo.toml       # veyron-sdk + veyron (path deps) + sqlx (any, postgres,
                    # sqlite, mysql features) + serde_json
  plugin.json      # registry metadata (permissions: ["PERMISSION_DATABASE"])
  config.example.yaml   # dsn: "postgres://user:pass@host/db"
  src/
    main.rs          # DatabasePlugin (Plugin trait impl), holds sqlx::AnyPool
    classify.rs       # keyword-prefix statement classifier
    placeholders.rs   # quote-aware `?` -> `$N` rewriter (no-op except Postgres)
```

Binary name: `database`. Plugin id: `"database"`.

DSN read from `DATABASE_DSN` env var at `on_init`. `sqlx::AnyPool` connects
lazily/eagerly per `sqlx` defaults; the pool handles reconnect on drop.

## Manifest

```rust
PluginManifest {
    permissions: vec!["PERMISSION_DATABASE".into()],
    actions: vec!["db_query".into(), "db_execute".into(), "db_transaction".into()],
    events: vec![],
    ipc_targets: vec![],
    ..Default::default()
}
```

New permission `PERMISSION_DATABASE` added to `proto/veyron_protocol.proto`
(reserved-field discipline applies as usual for this file).

## Placeholder syntax

Callers always write `?` for positional params, regardless of configured
backend. `placeholders.rs` rewrites `?` -> `$1, $2, ...` only when the
configured backend is Postgres; SQLite and MySQL already use `?` natively
so the rewrite is a no-op for them.

The rewriter is quote-aware: it scans the SQL string tracking single-quote
string-literal state (and Postgres dollar-quoting) so a literal `?`
character inside a string value is never counted or rewritten — only `?`
tokens outside of quotes are treated as placeholders.

Param count in the rewritten SQL is checked against the `params` array
length before execution; a mismatch is rejected as an error, not sent to
the driver.

## `db_query` action

Request (`ActionRequest.params_json`, JSON):

```json
{ "sql": "SELECT id, name FROM users WHERE id = ?", "params": [42] }
```

- `sql` required. First keyword (after trimming whitespace/comments) must
  be `SELECT` or `WITH`; anything else is rejected before touching the DB.
- `params` optional (defaults to empty array), JSON scalars (string,
  number, bool, null) bound positionally.

Response (`ActionResponse.data_json`, JSON) on success:

```json
{ "columns": ["id", "name"], "rows": [[1, "alice"], [2, "bob"]] }
```

`ActionResponse.status = ACTION_OK`.

## `db_execute` action

Request: same shape as `db_query` (`sql` + `params`).

- First keyword must be `INSERT`, `UPDATE`, or `DELETE`; anything else
  (including `SELECT`) is rejected.

Response on success:

```json
{ "rows_affected": 3 }
```

## `db_transaction` action

Request:

```json
{
  "statements": [
    { "sql": "INSERT INTO users (name) VALUES (?)", "params": ["carol"] },
    { "sql": "UPDATE accounts SET balance = balance - ? WHERE user_id = ?", "params": [10, 1] }
  ]
}
```

- Each statement classified the same way as `db_execute` (DML only — no
  `SELECT`/`WITH` inside a transaction in v1). Any statement failing
  classification rejects the whole request before any statement runs.
- All statements run inside a single `sqlx` transaction. Any execution
  failure mid-batch rolls back the entire transaction; nothing is
  partially applied.

Response on success:

```json
{ "rows_affected": [1, 2] }
```

One integer per statement, same order as the request.

### Error handling

All failures (bad JSON, statement-type rejection, param count mismatch,
row-cap exceeded, timeout, connection error, transaction rollback) map to
`ActionStatus::ACTION_ERROR` with a human-readable message in
`ActionResponse.error`. `ACTION_PERMISSION_DENY` is not set by this
plugin — it's the kernel's status for routing a request to a plugin that
hasn't declared the required permission.

Example rejection messages:

- `"db_query only accepts SELECT/WITH statements"`
- `"db_execute only accepts INSERT/UPDATE/DELETE statements"`
- `"param count mismatch: sql has 2 placeholders, params has 1"`
- `"row cap exceeded: query returned more than 10000 rows"`

### Guardrails

- **Statement allowlist:** classification via `classify.rs` keyword-prefix
  check (trim whitespace/comments, inspect first keyword). `db_query`
  restricted to `SELECT`/`WITH`; `db_execute` and each statement in
  `db_transaction` restricted to `INSERT`/`UPDATE`/`DELETE`. Any DDL or
  admin statement (`CREATE`, `DROP`, `ALTER`, `GRANT`, ...) is rejected
  outright — a deliberate v1 restriction, not a security parser (see
  Testing section for edge cases this needs to cover).
- **Row cap:** `db_query` results capped at 10,000 rows. Exceeding the cap
  is an error (`ACTION_ERROR`), not silent/partial truncation.
- **Timeout:** 30s hard ceiling per action (including the whole
  `db_transaction` batch), matching the kernel's default action timeout.
- **Param binding:** all values passed as bound params, never
  string-interpolated into SQL — this is the primary injection defense,
  independent of the statement-type allowlist.

## Testing

- **Unit tests** (`classify.rs`): keyword-prefix classification across
  leading whitespace, SQL comments, `WITH` CTEs, mixed-case keywords,
  and each rejected statement type.
- **Unit tests** (`placeholders.rs`): quote-aware rewrite correctness —
  `?` inside single-quoted string literals is never rewritten, multiple
  `?` tokens are numbered correctly in order, no-op behavior when backend
  isn't Postgres, Postgres dollar-quoted strings handled.
- **Integration tests** (`tests/integration/`): run against a real SQLite
  file DB (no external service dependency needed in CI) covering:
  `db_query`/`db_execute`/`db_transaction` happy paths, statement-type
  rejection, row-cap exceeded, param count mismatch, and transaction
  rollback on a mid-batch failure.
- No cross-SDK test is needed — this is a standalone Rust plugin, not an
  SDK-surface feature like the Phase 7 (P7-01..04) work.
