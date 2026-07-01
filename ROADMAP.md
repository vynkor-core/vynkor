# Veyron ROADMAP — Phase 3

**Baseline:** 2026-07-01 · Kernel `0.1.0` · Audit score ~72/100 (regressed by Phase 3 compression)  
**Branch:** `develop` · Commit `234183a`  
**Phase 2 archive:** `docs/archive/` (all Phase 1–2 tasks complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-07-01

| Metric | Value |
|--------|-------|
| Kernel version | 0.1.0 |
| Audit score | pending re-audit (all BUG-001..006 fixed; was 72/100) |
| Open BUGs | none — BUG-001..006 all fixed, regression tests added |
| Open AUDIT items | AUDIT-007 (no live registry) |
| Tests | `cargo test --all --all-features`: 259 passing, 0 failing |
| SDKs | Rust ✅ · C++ ✅ · Python ✅ (MAC broken for ≥64 KiB frames, BUG-001) |

---

## Phase 3.1 — Bug Fixes

**Goal:** Zero failing tests. Must ship before any other phase work.

**Done-when:** `cargo test --all` exits 0.

---

### B-01 — Fix `test_manifest_enforcement` EACCES (5 tests)

**Files:** `tests/unit/test_manifest_enforcement.rs`

**Problem:** 5 tests write `plugin.json` fixtures to hardcoded paths that trigger `EACCES` in CI and restricted environments. All 5 fail with `called Result::unwrap() on an Err value: Os { code: 13, kind: PermissionDenied }`.

**Fix:** Replace hardcoded fixture paths with `tempfile::tempdir()`. Each test creates its own isolated temp directory, writes `plugin.json` there, and the directory is cleaned up on drop.

```rust
let dir = tempfile::tempdir().unwrap();
let plugin_json = dir.path().join("plugin.json");
std::fs::write(&plugin_json, serde_json::to_string(&manifest).unwrap()).unwrap();
```

**Acceptance:** All 5 `test_manifest_enforcement::*` tests pass on CI with no filesystem side effects.

**Effort:** 1–2 h

---

### B-02 — Fix `test_kernel::kernel_graceful_shutdown_does_not_panic` timeout

**Files:** `tests/unit/test_kernel.rs`

**Problem:** Test spins up the kernel and waits for graceful shutdown, but the deadline fires before the supervisor finishes. Fails with `Elapsed(())`.

**Fix:** Either (a) increase the test timeout to a value that accounts for SQLite EventStore flush + supervisor teardown, or (b) mock the supervisor/event-store in the unit test so the kernel shuts down synchronously. Prefer (b) — the unit test should not depend on real I/O.

**Acceptance:** `test_kernel::kernel_graceful_shutdown_does_not_panic` passes consistently in CI (no flakiness over 10 runs).

**Effort:** 1–2 h

---

### B-03 — Dead-code warnings in `installer.rs` and `supervisor.rs`

**Files:** `src/marketplace/installer.rs`, `src/plugins/supervisor.rs`

**Problem:** Diagnostics show:
- `installer.rs:33–34`: `events` and `actions` fields in `PluginManifest` never read
- `supervisor.rs:41`: `grace_seconds` field in `PluginConfig` never read

**Fix:** Either wire the fields to actual logic (preferred) or annotate with `#[allow(dead_code)]` only if deferral is intentional and documented.
- `grace_seconds`: wire into `graceful_shutdown()` — `PluginShutdown.grace_seconds` proto field exists; read it instead of hardcoding.
- `events`/`actions`: wire into the event bus subscription at plugin load time if plugin declares events in manifest.

**Acceptance:** `cargo build` produces zero `dead_code` warnings for these fields.

**Effort:** 1–2 h

---

## Phase 3.2 — Transport Hardening

**Goal:** Close the two remaining protocol gaps (`FLAG_COMPRESSED`, per-plugin IPC rate limit) and add TLS to the WS gateway.

---

### T-01 — zstd payload compression (FLAG_COMPRESSED, Bit 1)

**Files:** `src/ipc/framing.rs`, `sdk/rust/src/framing.rs`, `sdk/cpp/include/veyron/framing.hpp`, `sdk/python/veyron/framing.py`, `Cargo.toml`

**Problem:** `FLAG_COMPRESSED = 0x0002` is defined in `docs/FRAMING.md` but `zstd` is not in `Cargo.toml` and no compress/decompress logic exists. Without this, payloads over 64 KB (audio metadata, AI responses, log dumps) cannot be compressed before hitting the 1 MiB frame cap.

**What to do:**

Add to `Cargo.toml`:
```toml
zstd = "0.13"
```

In `src/ipc/framing.rs`, after CRC32 is computed and before writing to the socket:
```rust
let (payload_bytes, flags) = if payload.len() >= COMPRESS_THRESHOLD {
    let compressed = zstd::bulk::compress(payload, 3)?;
    if compressed.len() < payload.len() {
        (compressed, flags | FLAG_COMPRESSED)
    } else {
        (payload.to_vec(), flags)
    }
} else {
    (payload.to_vec(), flags)
};
```

On `read_frame`, after reading payload bytes:
```rust
let payload = if frame.flags & FLAG_COMPRESSED != 0 {
    zstd::bulk::decompress(&raw_payload, MAX_PAYLOAD_SIZE)?
} else {
    raw_payload
};
```

`COMPRESS_THRESHOLD = 65_536` (64 KB). Below threshold: no overhead for small control messages.

Mirror the constant in each SDK. Compression/decompression is the framing layer's responsibility — router and protocol code above it see only raw bytes.

**Acceptance:** Unit test: payload > 64 KB round-trips with `FLAG_COMPRESSED` set and decompressed correctly on the other side. Payload ≤ 64 KB skips compression. CRC32 is computed over the **compressed** bytes (what is actually written to the socket).

**Effort:** 3–4 h

---

### T-02 — Per-plugin IPC send rate limit

**Files:** `src/ipc/protocol.rs`, `src/utils/config.rs`, `config.yaml`

**Problem:** HTTP API has per-token rate limiting via `governor`, but there is no throttle on IPC send. A plugin can flood the router with `Envelope` messages at full socket speed. Per-connection error budget (16 errors → disconnect) only catches malformed frames, not valid high-volume ones.

**What to do:**

Add `ipc_rate_limit_rps: Option<u32>` to `Config` (default `None` = unlimited). When set, apply a `governor::RateLimiter` per `conn_id` in the router's unicast and broadcast paths. Key the limiter by `conn_id` (not `plugin_id`) so an unregistered connection cannot exhaust another plugin's quota.

```yaml
# config.yaml
ipc_rate_limit_rps: 500   # optional; per-plugin, per-second
```

On limit exceeded: send `ErrorMessage { code: ERR_RATE_LIMITED }` to the sender (do not disconnect — rate limit is a quota, not a protocol error). Increment `ipc_send_denied_total` Prometheus counter.

**Acceptance:** Integration test — plugin sends 600 messages in 1 second with limit set to 500 → first 500 succeed, remainder receive `ERR_RATE_LIMITED` without disconnecting.

**Effort:** 2–3 h

---

### T-03 — TLS for WebSocket gateway

**Files:** `src/api/server.rs`, `src/utils/config.rs`, `config.yaml`, `Cargo.toml`

**Problem:** WebSocket gateway binds on plain HTTP. External clients connecting over a network (not loopback) transmit JWT tokens and plugin messages in clear text.

**What to do:**

Add to `Cargo.toml`:
```toml
axum-server = { version = "0.7", features = ["tls-rustls"] }
```

Add to `Config`:
```rust
pub tls_cert_path: Option<PathBuf>,
pub tls_key_path:  Option<PathBuf>,
```

In `src/api/server.rs`, when both paths are set, bind `axum_server::tls_rustls::RustlsConfig` instead of plain `axum_server::Server`. When not set, fall back to plain HTTP (loopback deployments don't need TLS). Never require TLS — `allow_no_tls: bool` is implicit when cert/key are absent.

```yaml
# config.yaml (optional)
tls_cert_path: /etc/veyron/tls/cert.pem
tls_key_path:  /etc/veyron/tls/key.pem
```

**Acceptance:** Kernel starts with cert+key configured → `curl -k https://localhost:<port>/health` returns `{"status":"ok"}`. Without cert+key → plain HTTP unchanged.

**Effort:** 3–4 h

---

## Phase 3.3 — Plugin Lifecycle

---

### T-04 — Config hot-reload via `reload_config` KernelCommand

**Files:** `src/kernel/commands.rs`, `src/utils/config.rs`, `src/kernel/orchestrator.rs`

**Problem:** `KernelCommand { command: "reload_config" }` is dispatched and acknowledged but does nothing — the config is loaded once at startup and never re-read. `log_level` changes require a kernel restart.

**What to do:**

In `CommandHandler::dispatch`, `"reload_config"` arm:
1. Re-read `config.yaml` from the path used at startup (stored in `Arc<AtomicRef<Config>>` or passed through `OrchestratorState`).
2. Apply changes that are safe to change at runtime: `log_level`, `ipc_rate_limit_rps`, `watchdog_interval_secs`, `watchdog_timeout_secs`, `api_rate_limit_rps`, `api_rate_limit_burst`.
3. Changes that require restart (socket path, JWT secret, TLS cert): log `warn!` and skip.
4. Return `KernelCommandAck { success: true, data_json: "{\"reloaded\": [\"log_level\", ...]}" }`.

Also wire `SIGHUP` → emit `reload_config` internally, so `kill -HUP <pid>` reloads config without a plugin round-trip.

**Acceptance:** Unit test: start kernel, send `reload_config` command, verify `log_level` changes from `"info"` to `"debug"` without restart.

**Effort:** 2–3 h

---

### T-05 — Plugin dependency declaration and enforcement

**Files:** `proto/veyron_protocol.proto`, `src/plugins/loader.rs`, `docs/PLUGIN_REGISTRY_SCHEMA.md`

**Problem:** `plugin.json` has no way to declare that plugin A requires plugin B to be running before it can function. Kernel loads plugins in config order; if B comes after A, A may attempt IPC to B before B registers.

**What to do:**

Add to `plugin.json` schema (update `docs/PLUGIN_REGISTRY_SCHEMA.md`):
```json
{
  "requires": ["plugin-b", "plugin-c"]
}
```

In `src/plugins/loader.rs`, before spawning a plugin:
1. Check that all `requires` entries are either already registered in the registry or appear earlier in the load order.
2. If a required plugin is missing from config entirely → refuse to load with: `"Plugin '<id>' requires '<dep>' which is not in config"`.
3. If a required plugin is present but not yet registered (load ordering issue) → requeue this plugin for a second pass after the dependency spawns. Max 2 passes.
4. Cycle detection: if A requires B and B requires A → refuse both with: `"Circular dependency: <A> ↔ <B>"`.

**Acceptance:** Unit test: plugin A declares `requires: ["plugin-b"]`; plugin B loads first → A loads after B registers. Reversed order in config → kernel reorders automatically. Circular dep → both plugins refused, kernel stays up.

**Effort:** 3–4 h

---

### T-06 — Live marketplace registry

**Files:** `docs/PLUGIN_REGISTRY_SCHEMA.md`, external GitHub repo

**Problem:** `vyn install <slug>` fetches from `https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json` but that repo does not exist. Install works only against a local mock. No plugins are actually distributable.

**What to do:**

1. Create `github.com/veyron-core/veyron-plugins` repo with:
   - `registry.json` (sample with at least one real entry: `echo-plugin-rs` from `examples/echo_plugin_rs`)
   - Release tag `echo-rs-0.1.0` with a signed `.zip` archive
   - CI that validates `registry.json` schema on every push

2. In `src/marketplace/registry.rs`, add fallback URL config:
```yaml
registry_url: https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json
```
Default is the official URL; override for private registries.

**Acceptance:** `vyn plugin list` returns at least one real plugin. `vyn install echo-rs` installs the echo plugin binary and `plugin.json` validates.

**Effort:** 4–6 h (plus GitHub repo setup)

---

## Phase 3.4 — Observability

---

### T-07 — Per-plugin resource metrics in Prometheus

**Files:** `src/plugins/supervisor.rs`, `src/metrics.rs`

**Problem:** `/metrics` exposes message-level counters but no per-plugin system resource usage. Production deployments cannot alert on runaway plugins without process-level metrics.

**What to do:**

In `supervisor.rs` `monitor_loop`, every `watchdog_interval_secs`:
- Read `/proc/<pid>/stat` (Linux) for CPU time (user + system jiffies)
- Read `/proc/<pid>/status` for `VmRSS` (resident set size in KB)
- Expose as Prometheus gauges labeled by `plugin_id`:
  - `veyron_plugin_cpu_seconds_total{plugin_id="..."}`
  - `veyron_plugin_memory_rss_bytes{plugin_id="..."}`

Gate behind `#[cfg(target_os = "linux")]`. On non-Linux: gauges not registered (no-op, not error).

**Acceptance:** `GET /metrics` with a running plugin shows `veyron_plugin_memory_rss_bytes{plugin_id="echo"}` with a non-zero value. Non-Linux: build clean, no gauge registered.

**Effort:** 2–3 h

---

### T-08 — OpenTelemetry trace export

**Files:** `src/utils/logging.rs`, `Cargo.toml`, `config.yaml`

**Problem:** `tracing` spans exist throughout the kernel but no exporter is wired. Debugging cross-plugin latency requires correlating log lines by timestamp — there is no trace context propagation.

**What to do:**

Add to `Cargo.toml`:
```toml
opentelemetry        = { version = "0.24", features = ["trace"] }
opentelemetry-otlp   = { version = "0.17", features = ["grpc-tonic"] }
tracing-opentelemetry = "0.25"
```

In `src/utils/logging.rs`, when `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set, add the OTLP exporter as a second subscriber layer alongside the existing `tracing_subscriber::fmt`. Otherwise no-op — zero overhead when OTel is not configured.

Inject `trace_id` into `Envelope` header via a reserved proto field (add `trace_id: string = 30` to `Envelope`, using a free field number). Router propagates it without modification.

**Acceptance:** With `OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317` set, a registered plugin's `PluginRegister` → `PluginRegisterAck` round-trip appears as a parent span with child spans in Jaeger/Tempo. Without the env var: zero OTel code executes.

**Effort:** 4–6 h

---

## Phase 3.5 — SDK Integration Test Harness

**Goal:** All three SDKs tested against a real kernel, not mocks. Currently SDK tests are unit-only.

---

### T-09 — Shared SDK integration test harness

**Files:** `tests/integration/sdk_harness.rs` (new), `tests/integration/test_sdk_rust.rs` (extend), `tests/integration/test_sdk_cpp.rs` (new), `tests/integration/test_sdk_python.rs` (new)

**Problem:** `test_sdk.rs` unit tests mock the socket. Cross-language protocol compliance can only be verified against a live kernel. A C++ SDK bug that passes unit tests would only surface in production.

**What to do:**

Create a shared test helper `sdk_harness.rs` that:
1. Spins up the kernel on a temp socket path
2. Yields the socket path and a shutdown handle
3. Cleans up on drop

```rust
pub struct SdkHarness {
    pub socket_path: PathBuf,
    pub jwt_secret: String,
    shutdown_tx: oneshot::Sender<()>,
}

impl SdkHarness {
    pub async fn start() -> Self { ... }
}
impl Drop for SdkHarness {
    fn drop(&mut self) { ... }
}
```

Add integration tests for each SDK:
- **Rust SDK** (`test_sdk_rust.rs`): connect, register, ping, send ActionRequest, recv ActionResponse
- **C++ SDK** (`test_sdk_cpp.rs`): build `examples/echo_plugin_rs`, launch it, verify round-trip via kernel — uses `std::process::Command` to run the C++ binary
- **Python SDK** (`test_sdk_python.rs`): run `sdk/python` client script via `std::process::Command` against live kernel

**Acceptance:** `cargo test --test integration test_sdk` passes with all three SDKs exercised against a real kernel.

**Effort:** 4–6 h

---

## Phase 4 — Audit Regression Fixes

**Goal:** Close every bug in `AUDIT.md` (BUG-001..006). Phase 3 compression shipped two Critical protocol regressions; nothing else ships until they are fixed.

**Done-when:** all six bugs fixed, regression tests added, `cargo test --all --all-features` exits 0, audit score restored to ≥ 90.

---

### T-10 — Normalize compressed-frame invariant + MAC coverage (BUG-001, BUG-002)

**Priority:** P0 (Critical) · **Files:** `src/ipc/framing.rs`, `src/ipc/connection.rs`, `src/api/websocket.rs`, `tests/unit/test_framing.rs`

**Problem:** After `read_frame_body` decompresses a payload it leaves `FLAG_COMPRESSED` set and `length` at the compressed size (`framing.rs:230-235`). Two failures follow:

- **BUG-001 (MAC):** the write loop tags the frame *before* `write_frame_raw` compresses (`connection.rs:275-280`), so the tag covers the uncompressed header/payload; the receiver rebuilds the header from the compressed length + COMPRESSED bit and verifies against the decompressed payload → **every frame ≥ 64 KiB on an auth-enabled connection fails MAC and is dropped.**
- **BUG-002 (forwarding):** the router re-forwards the decompressed-but-still-flagged frame; `write_frame_raw` sees COMPRESSED already set, skips re-compression, emits plaintext bytes flagged COMPRESSED → the next peer calls `zstd::decompress` on plaintext → error → connection dropped.

**Fix — pick one canonical invariant and apply symmetrically:**
1. In `read_frame_body`, after decompressing: clear `FLAG_COMPRESSED` and set `length = decompressed_len` so the in-memory `Frame` invariant is "payload is always plaintext; flags/length describe the plaintext." This alone fixes BUG-002.
2. For MAC: define the tag to cover the **plaintext** header+payload on both ends. Since the read path now normalizes to plaintext, the receiver's `serialize_header` matches what the sender tagged pre-compression. Verify the write loop tags the plaintext frame (it already does) and that `write_frame_raw` does not alter the MAC after compressing (it must not — the tag is over plaintext).
3. Fix `test_framing.rs:302` — it currently asserts `FLAG_COMPRESSED must be set on received frame`, which encodes the bug. Flip it to assert the flag is **cleared** and `length == decompressed_len`.

**Acceptance:**
- New test: ≥ 64 KiB payload over a MAC-enabled connection round-trips and verifies (regression for BUG-001).
- New test: ≥ 64 KiB payload forwarded plugin→plugin arrives intact, connection stays up (regression for BUG-002).
- Received compressed frame has `FLAG_COMPRESSED` cleared and `length` == plaintext length.

**Effort:** 4–6 h

---

### T-11 — Cap fragment reassembly (BUG-003)

**Priority:** P0 (High, DoS) · **Files:** `src/ipc/connection.rs`

**Problem:** `ReassemblyBuf` has only a 30 s idle timeout — no cap on reassembled size, no cap on concurrent streams. `total` is a `u16` (≤ 65 535) × up to 1 MiB per fragment → reassembled frame bypasses `MAX_PAYLOAD_SIZE`; `length: payload.len() as u32` truncates above 4 GiB; unbounded `stream_id` keys let a peer buffer memory up to line-rate × 30 s with no ceiling.

**Fix:**
- Cap concurrent reassembly streams per connection (e.g. `MAX_REASSEMBLY_STREAMS = 64`); reject new stream_ids past the cap.
- Track cumulative buffered bytes per stream and per connection; abort the stream (drop connection) if a reassembled payload would exceed `MAX_PAYLOAD_SIZE`.
- Reject fragments whose `total` disagrees with the stream's first-seen `total`.

**Acceptance:** test — a stream whose fragments sum past `MAX_PAYLOAD_SIZE` is rejected before allocation; opening more than the stream cap drops the connection; per-connection buffered bytes are bounded.

**Effort:** 3–4 h

---

### T-12 — Rate-limit behind auth, not on forged `sub` (BUG-004)

**Priority:** P1 (Medium) · **Files:** `src/api/rate_limit.rs`, `src/api/middleware.rs`, `src/api/server.rs`

**Problem:** `extract_sub` decodes the JWT with `insecure_disable_signature_validation()` and keys the limiter on the resulting `sub` (`rate_limit.rs:48-63`). `sub` is attacker-controlled: rotating it per request bypasses the limit entirely; setting it to a victim's id burns the victim's bucket.

**Fix:** rate-limit unauthenticated traffic by source (peer/connection) *before* auth, and apply the per-`sub` quota only against **verified** claims (after signature check in/after `auth_middleware`). Do not key any limiter on unverified token fields.

**Acceptance:** test — forged tokens with rotating `sub` do not escape the per-source limit; a valid token's bucket cannot be exhausted by a third party supplying the same `sub` unsigned.

**Effort:** 2–3 h

---

### T-13 — Honor shutdown grace semantics (BUG-005)

**Priority:** P2 (Low) · **Files:** `src/plugins/supervisor.rs`, `src/kernel/orchestrator.rs`

**Problem:** `graceful_shutdown(&self, _default_grace_seconds: u32)` ignores its argument and uses `max()` of every plugin's `grace_seconds` — one plugin with a large grace delays SIGKILL for all, and the caller's `GRACE_SECONDS = 5` is dead.

**Fix:** SIGKILL each plugin on its own per-plugin deadline (SIGTERM all, then per-plugin timer to SIGKILL), using the passed default as the floor when a plugin's `grace_seconds == 0`. Remove the dead argument or actually consume it.

**Acceptance:** test — plugin A grace=1s, plugin B grace=10s → A receives SIGKILL ~1s after SIGTERM without waiting for B; passed default applies when grace is 0.

**Effort:** 2–3 h

---

### T-14 — Harden socket path fallback (BUG-006)

**Priority:** P3 (Low) · **Files:** `src/utils/config.rs`, `src/ipc/server.rs`

**Problem:** With `XDG_RUNTIME_DIR` unset, the socket falls back to the predictable world-writable `/tmp/veyron.sock` and is unconditionally `remove_file`d before bind.

**Fix:** when `XDG_RUNTIME_DIR` is absent, prefer a per-user private dir (e.g. `/run/user/<uid>` if present, else a 0o700 dir under `$HOME`); fail closed with a clear error rather than defaulting into shared `/tmp`. Do not `remove_file` a path the kernel does not own (stat it: refuse if it exists and is not a socket we created).

**Acceptance:** test — no `XDG_RUNTIME_DIR` → socket lands in a per-user 0o700 dir, not `/tmp`; a pre-existing non-socket at the path is refused, not deleted.

**Effort:** 1–2 h

---

## Task Summary

| ID | Phase | Title | Priority | Status |
|----|-------|-------|----------|--------|
| T-10 | 4 | Normalize compressed-frame invariant + MAC (BUG-001/002) | **P0** | Done |
| T-11 | 4 | Cap fragment reassembly (BUG-003) | **P0** | Done |
| T-12 | 4 | Rate-limit behind auth (BUG-004) | P1 | Done |
| T-13 | 4 | Honor shutdown grace semantics (BUG-005) | P2 | Done |
| T-14 | 4 | Harden socket path fallback (BUG-006) | P3 | Done |
| B-01 | 3.1 | Fix `test_manifest_enforcement` EACCES | P0 | Done |
| B-02 | 3.1 | Fix kernel shutdown timeout in unit test | P0 | Done |
| B-03 | 3.1 | Wire or suppress dead-code fields | P0 | Done |
| T-01 | 3.2 | zstd payload compression (FLAG_COMPRESSED) | P1 | Done |
| T-02 | 3.2 | Per-plugin IPC send rate limit | P1 | Done |
| T-03 | 3.2 | TLS for WebSocket gateway | P2 | Done |
| T-04 | 3.3 | Config hot-reload via `reload_config` + SIGHUP | P1 | Done |
| T-05 | 3.3 | Plugin dependency declaration and enforcement | P2 | Done |
| T-06 | 3.3 | Live marketplace registry | P1 | Done |
| T-07 | 3.4 | Per-plugin resource metrics (CPU, RSS) | P2 | Done |
| T-08 | 3.4 | OpenTelemetry trace export | P3 | Done |
| T-09 | 3.5 | SDK integration test harness (all 3 SDKs) | P1 | Done |

---

## Definition of Done

| Criterion | Phase |
|-----------|-------|
| `cargo test --all` exits 0 | 3.1 |
| Zero `dead_code` warnings on project build | 3.1 |
| Payloads ≥ 64 KB compressed transparently; CRC over compressed bytes | 3.2 |
| Per-plugin IPC rate limit enforced; `ERR_RATE_LIMITED` returned without disconnect | 3.2 |
| Kernel binds TLS when cert+key configured; plain HTTP when not | 3.2 |
| `reload_config` command re-applies safe config fields at runtime | 3.3 |
| `SIGHUP` triggers config reload | 3.3 |
| Plugin dependency ordering enforced; circular dep refused | 3.3 |
| `vyn install echo-rs` installs from live registry | 3.3 |
| `/metrics` shows per-plugin CPU and RSS on Linux | 3.4 |
| OTel trace spans exported when `OTEL_EXPORTER_OTLP_ENDPOINT` set | 3.4 |
| All 3 SDK integration tests pass against live kernel in CI | 3.5 |
| ≥ 64 KiB frame round-trips + verifies MAC on auth connection (BUG-001) | 4 |
| ≥ 64 KiB frame forwards plugin→plugin intact, connection stays up (BUG-002) | 4 |
| Received frame clears `FLAG_COMPRESSED`, `length` == plaintext length | 4 |
| Fragment reassembly bounded: stream cap + per-stream/conn byte cap; over-size rejected (BUG-003) | 4 |
| HTTP rate limit keyed only on verified claims / source (BUG-004) | 4 |
| Per-plugin shutdown grace honored; slow plugin does not gate others (BUG-005) | 4 |
| No `XDG_RUNTIME_DIR` → socket in per-user 0o700 dir, not `/tmp` (BUG-006) | 4 |
| Audit score restored ≥ 90 | 4 |

---

*Archive: `docs/archive/` contains Phase 1–2 planning docs and full VULN-001–022 / T-01–T-22 history.*
