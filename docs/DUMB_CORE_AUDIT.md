# Dumb-Core Audit & Fix Plan

**Date:** 2026-08-16
**Scope:** full kernel — `src/kernel`, `src/api`, `src/plugins`, `src/events`,
`src/ipc`, `src/auth`, `src/marketplace`, `src/bridge`, `src/cli`, `src/utils`,
plus the wire protocol (`../vynkor-wire/proto/veyron_protocol.proto`).
**Companion:** findings mirrored as DC-1…DC-5 in `AUDIT.md`.
**Status:** all findings **OPEN**. This file is the working plan for the fixes.

---

## 1. What "dumb core" means here

The manifesto (README §1 "Dumb Core", ROADMAP "Manifesto") is non-negotiable:

> The kernel contains **no** business logic, **no** AI models, **no** databases
> for application state. It is a high-speed byte router and process supervisor.
> All intelligence lives in plugins.

Operationally that means the kernel owns exactly four jobs:

1. **Transport** — frame bytes over UDS, route by the 32-byte `target` field,
   zero-parse, MAC-authenticated, CRC-checked.
2. **Lifecycle** — spawn, supervise, restart, sandbox, kill plugin processes.
3. **Security** — JWT auth, per-frame HMAC, default-deny permissions.
4. **Plumbing** — API gateway (REST/WS), event-bus delivery mechanism, metrics,
   TLS, config/CLI ergonomics.

Everything else — any feature that answers *"what should the system do"*
rather than *"how do processes talk"* — belongs in a plugin or a companion tool.

---

## 2. Verdict

**Manifesto: declared. Code: partially drifted.**

The transport/supervision/security core is genuinely dumb (see §3). But four
blocks of product-level logic have grown into the kernel, and one manifesto
clause is technically violated by the event store:

| # | Finding | Severity | Status |
|---|---------|----------|--------|
| DC-1 | Marketplace / app-store client in the kernel (2 485 L) | Medium | OPEN |
| DC-2 | Device-fleet domain model (D-01…D-14) | Medium | OPEN |
| DC-3 | AI tool-calling surface in protocol + kernel | Low-Med | OPEN (decision) |
| DC-4 | Hardcoded action→permission policy | Low | OPEN |
| DC-5 | Events SQLite DB vs "no databases" clause | Info/Low-Med | CLOSED (2026-08-26: wording + S2 + PERF-2 all shipped) |

The drift is not accidental rot — it is deliberate shipped product work
(D-series remote devices, Phase 10 marketplace state, D-08 tool schemas). The
fix is therefore a **boundary decision**, not a bug hunt.

---

## 3. What is clean (do not re-litigate without new evidence)

Verified dumb-core-clean in this pass:

- **`src/ipc/protocol.rs`** (1389 L) — `MessageRouter`: registration, permission
  clamping, zero-parse forward/broadcast with default-deny gates, action
  correlation/streaming, timeouts, quotas. Transport/security only.
- **`src/plugins/`** lifecycle + sandbox — `supervisor.rs` (spawn/restart/
  backoff/watchdog/log rings), `runner.rs` (namespaces, rlimits, cgroups),
  `shim.rs`, `seccomp.rs`, `fsaccess.rs` (Landlock). Pure infrastructure.
- **`src/auth/`** — JWT validation, per-frame HMAC, permission checks.
- **`src/events/bus.rs` + `store.rs`** — the *mechanism* (subscription fan-out,
  at-least-once delivery) is infrastructure. Payload *shape* is flagged in DC-3.
- **`src/api/{server,middleware,rate_limit}.rs`** — Axum gateway, auth
  middleware, per-token rate limiting. The only domain endpoint is
  `GET /devices` (see DC-2).
- **`src/kernel/orchestrator.rs`** — wiring, signals, graceful shutdown,
  config reload. No business logic.

Endpoint inventory (`src/api/server.rs`): `GET /health` (:70), `GET /metrics`
(:85), `GET /plugins` (:86), `GET /plugins/{id}` (:87),
`GET /plugins/{id}/logs` (:88), `POST /plugins/{id}/start|stop|restart` (:79-81,
admin), **`GET /devices` (:89 — domain, DC-2)**, `WS /ws` (:139-145).

---

## 4. Findings

### DC-1 — Marketplace / plugin app-store client embedded in the kernel

**Where:**
- `src/marketplace/registry.rs` (1509 L) — registry client:
  - `DEFAULT_REGISTRY_URL` hardcoded to the veyron-plugins GitHub raw URL
    (`:15-16`);
  - maintainer Ed25519 public key pinned in kernel source (`:38-39`);
  - registry v2 parse/flatten, signature verification, versioned disk cache,
    revocation enforcement, kernel-compat range policy (`:626-661`).
- `src/marketplace/installer.rs` (822 L) — install pipeline: download with
  progress, sha256, zip-slip-protected extraction, atomic rename, manifest
  validation, `installed.json` ledger, `plugins.d/<slug>.yaml` drop-in write,
  enable/disable.
  - **Hardcoded business rule:** `let sandbox = installed.plugin_id != "network";`
    (`:647`) — the kernel knows a specific plugin and its sandbox constraints.
- `src/marketplace/state.rs` (154 L) — install ledger.
- `src/cli/plugin.rs` — `vyn plugin list/search/install/remove/enable/disable`.

**Why it violates dumb core:** package management, marketplace governance and
upgrade detection are product features. Every change to the catalog, registry
key or install policy currently ships a kernel release.

**Fix (see §6, item F1):** extract into a `marketplace` plugin (or a separate
binary) that drives the kernel only through the existing lifecycle surface
(`plugins.d/` drop-ins + `config` kernel command). Decide explicitly whether
signed-archive verification stays a kernel security boundary.

---

### DC-2 — Device-fleet domain model in the kernel

**Where:**
- `src/plugins/registry.rs`:
  - `DeviceMeta` (device_id/user_id/os/arch/os_version/capabilities) `:17-24`;
  - `devices: DashMap<String, DeviceInfo>` `:91`;
  - device upsert on registration `:206-229`;
  - device → **Offline** when its last plugin unregisters `:239-247`;
  - `record_pong` advances `last_seen` / state `:250-260`;
  - `get_device`/`list_devices` `:271-278`.
- Discovery surfaces: `GET /devices` (`src/api/routes.rs:107-136`,
  `src/api/server.rs:89`), IPC kernel command `list_devices`
  (`src/kernel/commands.rs:61-78`), CLI `vyn devices` (`src/cli/devices.rs`).
- Pairing/identity tooling: `vyn device connect` — QR code embedding the
  **master `jwt_secret`** (`src/cli/device.rs:26-36,126-150`); `vyn token mint`
  per-device JWTs (`src/cli/token.rs`).
- Bridge: `src/bridge/mod.rs` (810 L) — `role: client` mirrors local plugins to
  a host kernel over WS as `device.<cap>` (`:199-201,283-297`).
- Wire protocol: `PluginRegister` device fields (`../vynkor-wire/proto/...:
  79-88`), `DeviceInfo`/`DeviceOs`/`DeviceState` (`:107-133`).
- Config: `Role::{Host,Client}`, `BridgeConfig`, `device_id`
  (`src/utils/config.rs:61-91,110-118`).

**Why it violates dumb core (revised 2026-08-16):** the *violation* is not
the device registry itself — the kernel must track devices for auth (JWT
`sub` = device_id) — nor the `GET /devices` / `list_devices` surfaces (same
shape as `GET /plugins`, i.e. observability), nor the bridge (transport). It
is the **interpretation embedded in the core**: capabilities semantics, the
online/offline state machine driving UX, friendly display mapping
(`device_os_str`/`device_state_str`, `:511-529`), and `device.<cap>` mirroring
semantics in the bridge.

**Defensible slice (stays in the kernel):** device identity + liveness
(`last_seen`) + raw metadata, exposed as pass-through observability
(`GET /devices` parallel to `GET /plugins`); the `role: client` bridge as
transport; `vyn token mint` + QR pairing as auth tooling.

**Non-defensible (moves out):** interpretation and friendly UX — capabilities
semantics, device-management views, `device.<cap>` mirroring semantics —
→ `discovery` plugin / web frontend consuming the raw surfaces.

**Fix (see §6, items F2-F3):** enforce pass-through in the kernel surfaces;
move interpretation/UX outside; keep transport and auth tooling.

---

### DC-3 — AI tool-calling surface baked into protocol and kernel

**Where:**
- Wire proto: `ActionSpec`/`ActionRisk` — comment: *"tool schema for the AI
  (D-08)"* (`../vynkor-wire/proto/veyron_protocol.proto:159-173`).
- `src/kernel/commands.rs` `get_manifest` (`:79-127`) — comment: *"D-08:
  tool-calling surface — serve a plugin's manifest (incl. action_specs) to the
  AI"*.
- `src/events/bus.rs` `plugin_lifecycle_payload` (`:223-259`) — `action_specs`
  embedded in `system.plugin_joined`/`system.plugin_left` *"so the AI can
  enumerate callable actions from the joined event alone"*.
- README framing: Kairo = "AI agent, memory, voice" built on Veyron.

**Why it violates dumb core:** the kernel is explicitly shaped for an AI-agent
frontend. Tool-schema interpretation (risk levels, `requires_confirmation`,
params_schema) is domain logic.

**Fix (see §6, item F4):** this is a **policy decision**, not code. Either
(a) accept `action_specs` as a *generic manifest feature* (rename/re-document,
keep in protocol) or (b) strip interpretation out of the kernel and let the AI
plugin own it.

---

### DC-4 — Hardcoded action→permission policy

**Where:** `src/auth/permissions.rs:12-17` —
`required_permission_for_action("http_request") → PERMISSION_NETWORK`.

**Why it violates dumb core:** the kernel hardcodes knowledge of a specific
plugin's ("network") action name as the fallback permission map. The
data-driven v2 path (`registry.action_requirement`, `src/plugins/loader.rs:74-90`)
already supersedes it, but the fallback remains and its comment says new
sensitive actions must be added to the kernel.

**Fix (see §6, item F5):** drop the fallback; require v2 per-action permission
declarations; fail closed on undeclared sensitive actions.

---

### DC-5 — Events SQLite DB vs the manifesto's "no databases" clause

**The database in the kernel is exactly one:** `<data_dir>/events.db`, owned
entirely by `src/events/store.rs` (rusqlite 0.40, bundled; `Cargo.toml:83`).

**Schema** (single table, inline `CREATE TABLE IF NOT EXISTS`, no migration
framework — `store.rs:17-26`):

```
events (
  event_id     TEXT PRIMARY KEY,
  event_type   TEXT NOT NULL,
  payload_json BLOB NOT NULL,             -- full serialized Event protobuf
  status       TEXT DEFAULT 'pending',    -- pending | delivered | dead
  created_at   INTEGER NOT NULL,          -- unix secs
  retry_count  INTEGER DEFAULT 0
)
```

**Purpose — a delivery outbox, not application state:**

```
publish → store.persist (INSERT OR IGNORE, pending)      bus.rs:84-89
       → fan out to subscribers
       → plugin EventAck → mark_delivered                 protocol.rs:1024-1028
       → retry worker (every 5s): pending >10s → retry_count++ → dead @5
                                                        bus.rs:180-200, store.rs:121-131
       → prune: delivered/dead older than event_retention_secs (default 3600)
```

Without durable persistence, the at-least-once guarantee is impossible (a
kernel crash between publish and ack loses the event) — and at-least-once
delivery is a property of the bus *mechanism*, not a business feature. The
plugin registry is honestly in-memory (DashMap), and marketplace state is JSON
files (`installed.json`, `registry-cache.json`) — no other database exists.

**The three real problems:**
1. **Manifesto wording** — "no databases" reads literally as "no DB at all",
   yet the event store exists. Fix: amend to *"no databases **for application
   state**; the event-delivery outbox is an explicit exception"* (README §1,
   ROADMAP Manifesto).
2. **`payload_json` BLOB** transiently stores full plugin event payloads
   (bounded: 1h retention prune). Acceptable for an outbox; worth documenting.
3. **Open audit items on this path:** **S2** — `data_dir` default places
   `events.db` in world-writable `/tmp` (forgeable/readable on multi-user
   hosts; `AUDIT.md` S2); **PERF-2** — synchronous rusqlite under
   `std::sync::Mutex<Connection>` runs on tokio workers, blocking disk I/O on
   the publish path (`AUDIT.md` PERF-2).

**Fix (see §6, item F6):** keep the DB (it is the right tool), amend the
manifesto, close S2 (private runtime dir, 0o700, ownership check) and PERF-2
(`spawn_blocking` or a dedicated writer task).

---

## 5. What plugins already own (the correct boundary)

Sibling repo `veyron-plugins` proves the ecosystem pattern works: **ai**
(chat_completion, agents, model discovery, usage db), **network**
(`http_request` + SSRF guard), **stt/tts** (speech), **sync** (device KV sync,
heartbeat liveness, `sync.delta` events), **database**, **secrets**,
**gated-write**, **ping-pong-rs**. All real business logic (AI, networking,
storage, speech) lives in plugins; `veyron-sdk-*` repos are pure client
libraries. The kernel's job is only to keep those processes alive and route
their bytes.

---

## 6. Fix plan

Priorities: **P0** do first (removes the clearest violations, unblocks the
rest) · **P1** this cycle · **P2** when convenient. Each item lists goal,
files, steps and acceptance criteria.

### F1 (DC-1, P0) — Extract the marketplace out of the kernel

- **Goal:** `src/marketplace/` no longer ships in the `vyn` binary.
- **Decision (§7):** separate binary **`vynm`** ("vyn manager") — no new
  kernel surface; writes the same `plugins.d/` drop-ins the kernel already
  reads. UX: `vynm install`, `vynm search`, `vynm list`, `vynm remove`,
  `vynm enable|disable`. New code/docs use **vynkor** naming (rename-in-progress
  policy): "vynm — the vynkor plugin manager".
- **Files:** `src/marketplace/` (registry.rs, installer.rs, state.rs),
  `src/cli/plugin.rs`, `src/cli/complete.rs`, `Cargo.toml` (drop
  `zip`/`indicatif` if unused elsewhere), new `vynm` binary/crate.
- **Steps:**
  1. Move `src/marketplace/` into a standalone binary crate `vynm` sharing
     the version and release cadence of `vyn`.
  2. Port the install pipeline as-is; keep the security pieces (sha256,
     zip-slip guard, Ed25519 verification) intact — they are good.
  3. Delete the `plugin_id != "network"` special-case; sandbox preference
     comes from the plugin's own manifest.
  4. Move the pinned maintainer key and `DEFAULT_REGISTRY_URL` into
     `vynm`'s config (env/flag), not source.
  5. `vyn plugin install/remove/list` become delegation shims to `vynm`
     (with a clear "install vynm" error when it's missing), or are removed
     from `vyn` entirely — `vynm` is the documented interface.
  6. Untangle the `src/cli/devices.rs → marketplace::state` import before
     moving (share the state-ledger module or inline it).
- **Acceptance:** `vyn` binary contains no marketplace code; `vynm install`
  works standalone; the `database`/`secrets` plugins still install and run
  against a kernel that has no marketplace module; kernel unit tests for
  marketplace move with it.

### F2 (DC-2, P0) — Keep device surfaces as dumb pass-through, move interpretation

- **Goal:** the kernel keeps identity + liveness + raw metadata and exposes
  them as observability (same shape as `GET /plugins`); interpretation and
  friendly UX live outside.
- **Decision (§7, revised):** keep `GET /devices` / `list_devices` /
  `vyn devices` in the kernel as **raw pass-through**; move only the
  *interpretation helpers* and friendly views out. Enforce the D-series
  principle: stores/passes metadata, never interprets.
- **Files:** `src/plugins/registry.rs` (`device_os_str`/`device_state_str`
  display mapping `:511-529` — move or reduce to raw wire values; `DeviceMeta`/
  `devices` map and the online/offline transition `:239-247` stay — that is
  liveness state, parallel to plugin state), `src/api/routes.rs:107-136`
  (keep `DeviceInfoView`, but raw pass-through only), `src/kernel/commands.rs:61-78`
  (keep `list_devices`, semantics = raw dump), `src/cli/devices.rs` (keep).
- **Steps:**
  1. Make `GET /devices` / `list_devices` return registry records verbatim
     (identity, `last_seen`, state as wire enum values) — no friendly
     interpretation in the kernel.
  2. Move the *interpretation* (capabilities semantics, online/offline
     meaning, friendly names) into a `discovery` plugin / web frontend that
     consumes the raw endpoint.
  3. Remove the kernel-side display helpers (`device_os_str`/
     `device_state_str`) or reduce them to raw wire values.
- **Acceptance:** `GET /devices` stays and returns raw pass-through data; no
  interpretation helpers in the kernel; a `discovery` plugin provides the
  friendly view; existing device integration tests pass unchanged (no API
  break — consumers like veyron-web / the Android agent keep working).

### F3 (DC-2, P1) — Keep the bridge as transport, strip capability interpretation

- **Goal:** the `role: client` bridge stays in the kernel as transport (remote
  connectivity, symmetric to the WS gateway); only capability *interpretation*
  is removed.
- **Decision (§7, revised):** keep the bridge, `vyn token mint` and QR pairing
  in the kernel — they are transport/auth tooling, and the VPS/remote-attach
  scenario depends on them.
- **Files:** `src/bridge/mod.rs` (810 L), `src/cli/device.rs`, `src/cli/token.rs`,
  `src/utils/config.rs` (`Role::Client`, `BridgeConfig`).
- **Steps:**
  1. Keep the bridge's relay, registration and MAC re-tagging — transport.
  2. Remove or make pass-through the `device.<cap>` mirroring *semantics* —
     naming/interpretation of mirrored capabilities moves to the remote agent
     (it knows its own capabilities; the kernel just passes bytes).
  3. Keep `vyn token mint --device` and `vyn device connect` QR as auth
     tooling (they mint/encode auth material — auth is kernel infra).
- **Acceptance:** the bridge still connects a client kernel to a host; no
  capability *semantics* live in the kernel; the Android agent (vynkor) still
  pairs via the existing tooling; no `BridgeConfig` change needed.

### F4 (DC-3, P1) — Neutralize the AI tool-calling surface (generic manifest feature)

- **Goal:** `action_specs`/`get_manifest` stay in the protocol as a **generic
  per-action capability mechanism**, with the "for the AI" framing removed.
- **Decision (§7):** generic manifest feature — no wire break, no feature
  removal; neutral wording everywhere.
- **Files:** `../vynkor-wire/proto/veyron_protocol.proto:159-173`,
  `src/kernel/commands.rs:79-127` (`get_manifest`), `src/events/bus.rs:223-259`.
- **Steps:**
  1. Reword the proto comments on `ActionSpec`/`ActionRisk` from "tool schema
     for the AI (D-08)" to neutral per-action capability descriptors (no
     semantic change, comments only — no wire bump needed; if a bump happens
     for other reasons, rename-only is fine to include).
  2. Reword `get_manifest`'s comment in `commands.rs` from "serve ... to the
     AI" to a generic manifest-query description.
  3. Reword the `plugin_lifecycle_payload` comment in `bus.rs` ("so the AI can
     enumerate ...") to neutral phrasing.
  4. Update README §1 / ROADMAP wording so `action_specs` is documented as a
     generic manifest feature usable by any frontend (AI, control panel,
     mobile), not an AI-only surface.
- **Acceptance:** no "for the AI" / "to the AI" / "AI" references in the
  protocol schema or kernel comments for this mechanism; behavior unchanged;
  all tests green.

### F5 (DC-4, P1) — Drop the hardcoded action→permission fallback (three-step migration)

- **Goal:** the kernel has no knowledge of any specific plugin's actions;
  the data-driven v2 path is the single source of truth.
- **Context:** `required_permission_for_action` (`permissions.rs:12-17`) is a
  *transitional safety net* — it gates `http_request` (the one sensitive
  action in the ecosystem) before network-плагин adopted v2 per-action
  permissions. Current semantics (R5-07): v2 `action_requirement` → gated;
  legacy string-form → fallback to the map; action not in the map → `None` =
  **unrestricted** (declaring the action is authorization enough).
- **Files:** `src/auth/permissions.rs:12-17`, `src/ipc/protocol.rs:652-653`
  (the `.or_else(required_permission_for_action(...))` call),
  `src/plugins/loader.rs:74-90`, `src/plugins/registry.rs:282-300`
  (`action_requirements` — stays, it is the v2 data path),
  `docs/PLUGIN_REGISTRY_SCHEMA.md:264,294`.
- **Steps (order matters — step 1 is a hard dependency):**
  1. **Migrate the data first (veyron-plugins):** network-плагин declares
     `action_requirement: http_request → network` in its v2 manifest. The map
     must NOT be removed before this lands — otherwise `http_request` becomes
     unrestricted and any plugin can trigger outbound HTTP via the network
     plugin (T-19 regression).
  2. **Kernel: two-tier policy replaces the map** (delete
     `required_permission_for_action` entirely):
     - v2 manifest, action **with** `permission` → gated by
       `action_requirement` (as today);
     - v2 manifest, action **without** `permission` → **fail-closed (deny)** —
       the provider explicitly declared the action but forgot the permission;
       fail loudly rather than silently unrestricted;
     - legacy string-form action → **unrestricted + load-time warning**
       ("plugin X action Y has no permission requirement — migrate to manifest
       v2") — preserves R5-07 for legacy third-party plugins, but the risk is
       now visible to operators.
  3. **Drift-proof tests:** v2-undeclared action → denied; legacy path →
     warning + unrestricted (no behavior break); a guard test asserting no
     plugin action names appear in `src/auth/` (the map is gone — nothing left
     to drift).
- **Rejected alternatives:** policy in config (`action_permissions: {...}` in
  YAML) — just moves the domain list from code to config, dumb-core not fixed;
  fail-closed for *everything* — breaks legacy third-party plugins.
- **Acceptance:** no plugin-name/action-name strings in `src/auth/`; the
  `network` plugin declares `http_request → PERMISSION_NETWORK` in its v2
  manifest (landed before this change); a v2 action without a declared
  permission is denied by default; legacy string-form plugins keep working
  with a boot warning.

### F6 (DC-5, P1) — Manifesto wording + event-store hardening

- **Goal:** the "no databases" clause says what it means; the outbox is safe
  and off the async hot path.
- **Files:** `README.md` §1, `ROADMAP.md` Manifesto, `src/events/store.rs`,
  `src/events/bus.rs`, `src/utils/config.rs` (`data_dir` default),
  `config.yaml`.
- **Steps:**
  1. Amend the manifesto: *"no databases for application state; the
     event-delivery outbox is an explicit exception"*.
  2. Close **S2**: default `data_dir` to the per-user private runtime dir;
     create the store dir 0o700 with an ownership check.
  3. Close **PERF-2**: move rusqlite calls off the router task —
     `tokio::task::spawn_blocking` or a dedicated writer task.
  4. Document the `payload_json` retention policy (1h default) in
     `docs/FRAMING.md` or the config reference.
- **Acceptance:** manifesto wording matches reality; `events.db` lives in a
  0o700 private dir; the publish path performs no synchronous SQLite I/O;
  event-delivery integration tests stay green.
- **Status: SHIPPED 2026-08-26.** Step 1 landed in `2f47d5f` (README §1 +
  ROADMAP Manifesto carve-out); step 2 = S2 (PR #35, 2026-08-18); step 3 =
  PERF-2 (PR #70 — `spawn_blocking` wrappers + batched retry sweep);
  step 4 documented (README "bounded 1h retention" + `event_retention_secs`
  in config.yaml). Event-delivery integration tests green throughout.

---

## 7. Decisions (resolved 2026-08-16)
All four open decisions are settled — the fix plan in §6 is decision-complete.

1. **Marketplace extraction target** (F1): **separate binary `vynm`** ("vyn
   manager"), shipped alongside `vyn`. Rationale: the installer is operator
   tooling, not a runtime plugin — a plugin would need a privileged kernel
   hook to write `plugins.d/` (new kernel surface + chicken-and-egg). A binary
   keeps the kernel surface at zero and runs outside the sandbox with operator
   credentials. It writes the same `plugins.d/<slug>.yaml` drop-ins the kernel
   already reads, so no kernel change is required for install. **Naming note:
   the rename-in-progress policy (CLAUDE.md) applies — the binary is `vynm`,
   and all new code, comments, docs and its `--help` use **vynkor** naming
   ("vynm — vynkor plugin manager"); migrate the module as vynkor, not
   veyron.**
2. **AI tool-calling boundary** (F4): **generic manifest feature**. `action_specs`
   become neutral per-action capability descriptors; `get_manifest` stays a
   generic manifest command. No wire break; the "for the AI" framing in proto
   comments, kernel commands and lifecycle events is reworded to neutral
   wording. (Chosen over stripping into Kairo: the feature is generic — any
   frontend benefits — and removing it would force a wire-version bump for a
   cosmetic gain.)
3. **Device discovery + bridge** (F2/F3): **keep in the kernel as dumb
   pass-through — observability and transport, not interpretation** (revised
   2026-08-16 after the impact analysis; supersedes the earlier "raw snapshot
   + discovery plugin" decision). Rationale: the kernel must track devices
   anyway for auth (JWT `sub` = device_id); `GET /devices` is the same shape
   as `GET /plugins`/`/metrics` (kernel-owned runtime state, pass-through) and
   is observability, not domain logic. The `role: client` bridge is transport —
   the kernel connecting outward as a client, symmetric to the WS gateway —
   and is exactly what lets remote vynkor instances / VPS deployments attach.
   `vyn token mint` (auth) and QR pairing stay as auth tooling. **What moves
   out is only interpretation and UX**: capabilities semantics, friendly
   device-management views, `device.<cap>` mirroring semantics → plugin/web
   consuming the raw surfaces.
4. **Event-store alternative** (F6): **keep the SQLite outbox and harden it**.
   Close S2 (private 0o700 runtime dir + ownership check) and PERF-2
   (`spawn_blocking`/dedicated writer task); amend the manifesto to carve out
   the delivery outbox. At-least-once across kernel restarts requires
   durability; SQLite embedded is zero-ops and already tested. In-memory
   + pluggable persistence and delegating to the `database` plugin (chicken-
   and-egg) are rejected.

---

## 8. Definition of done for this work

- `cargo test --all --all-features` green; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean;
  `cargo fmt --check` clean.
- Cross-repo protocol changes (F4) follow the wire bump process (proto header +
  `PROTOCOL_VERSION` + Cargo.toml in one commit; vendored copies re-synced;
  R8-05 drift guard stays green).
- Docs updated in the same PR (README §1 for any manifesto wording change;
  `AUDIT.md` statuses flipped to FIXED with evidence).
