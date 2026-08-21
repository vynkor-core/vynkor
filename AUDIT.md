# Veyron Codebase Audit

Date: 2026-07-07 (initial) · **Reconciled: 2026-08-11** (full re-audit on `develop` @ `c93342b`) · **Delta audit: 2026-08-14** (`develop` @ `2d16ebf` — post-reconciliation code + previously un-audited performance/UX surfaces) · **Architecture (dumb-core) audit: 2026-08-16** (manifesto compliance — domain logic in the kernel; see "Architecture audit — dumb-core" below; fix plan in `docs/DUMB_CORE_AUDIT.md`) · **Full src audit (maintainability & comments): 2026-08-20** (manual read of all `src/` — 49 files, 14251 LOC, no agents)
Scope: full repo — kernel/IPC/events/api (`src/kernel`, `src/ipc`, `src/events`, `src/api`, `src/utils`), auth/plugin-lifecycle/marketplace (`src/auth`, `src/plugins`, `src/cli`, `src/marketplace`), and cross-SDK/protocol (`sdk/rust`, `sdk/cpp`, `sdk/python`, `proto/`, `wire/`, `tests/`, `fuzz/`).

Method: three parallel read-only code-audit passes. Findings below are deduplicated and grouped by severity. Each entry has file:line, concrete failure scenario, and fix direction. The 2026-08-11 reconciliation re-verified every finding against the current tree (codegraph + targeted reads) and annotated its status.

## Status summary (2026-08-11)

| Severity | Total | Fixed | Deferred |
|----------|-------|-------|----------|
| Critical | 2 | 2 | 0 |
| High     | 5 | 5 | 0 |
| Medium   | 11 | 10 | 1 (M7) |
| Low      | 2 | 2 | 0 |
| **Total** | **20** | **19** | **1** |

**New findings from the 2026-08-11 pass:** N1 (moderate), N2–N5 (low) — see below.
**Still open:** M7 (C++/Python fuzz harness). M9 (zero-value enum renumber) shipped with protocol v1.5 (P11-03, 2026-08-13). Both tracked in `ROADMAP.md`.

**Delta audit (2026-08-14):** 13 new findings, tracked in
`ROADMAP.md` ("Immediate — Delta audit findings") with priorities P0–P3.
Security: **S1** (Medium, P0 — registry signature doesn't bind `status`/
`archive_url` → revocation bypass + download redirect) **— FIXED 2026-08-14**;
**S2** (Low-Med, P1 —
events DB in `/tmp/veyron`), **S3** (Low, P1 — RUSTSEC-2026-0204
`crossbeam-epoch`), **S5** (Low, P2 — internals leak into plugin-facing
errors), **S4** (Low, P3 — `anyhow`/`number_prefix` advisories) remain OPEN.
Performance: **PERF-1**/**PERF-2** (Medium, P1 — router blocking sends; sync
SQLite on the async runtime), **PERF-3** (Low-Med, P2 — per-message clones +
O(n) scans), **PERF-4** (Low, P3 — double CRC / sync zstd / `/proc` reads /
WS copies). UX: **UX-1** (Medium, P2 — body-less REST errors, 200-on-failure),
**UX-2** (Low-Med, P2 — Debug repr leaks), **UX-3**/**UX-4** (Low, P3 —
config validation, CLI polish). M7 remains deferred.

---

## Critical

### C1. HTTP admin API has no authorization check — any valid JWT can start/stop/restart any plugin
- **STATUS (2026-08-11): FIXED** — `require_kernel_admin` middleware (`src/api/middleware.rs:64-76`) now gates the lifecycle routes and is wired into `create_router_full` (`src/api/server.rs:17`), matching the IPC path.
- **Files:** `src/api/middleware.rs:19-43` (`auth_middleware`), `src/api/routes.rs:87-124` (`start_plugin`/`stop_plugin`/`restart_plugin`), `src/api/server.rs:71-78`
- **Issue:** `auth_middleware` validates JWT signature/expiry only — it never inspects `claims.permissions`. The lifecycle routes call `state.manager.start/stop/restart` with zero permission check. Compare to the IPC path (`src/ipc/protocol.rs:529-549`), which correctly requires `PermissionType::PermissionKernelAdmin` before executing an equivalent `KernelCommand`. This is the same class of gap the recent "gate kernel action routing on provider permission" commit (`2ff1a22`) fixed on IPC but missed on HTTP.
- **Impact:** any plugin holding *any* valid JWT (even one scoped to `network` only) can stop/start/restart every other supervised plugin, including privileged ones.
- **Fix:** require `PERMISSION_KERNEL_ADMIN` in `claims.permissions` before executing lifecycle routes — add as middleware layer or inline check, mirroring `check_permission`.

### C2. C++ SDK has zero real integration test coverage — tests spawn the Rust reference plugin, not C++
- **STATUS (2026-08-11): FIXED** — C++ SDK now ships a real gtest suite built via CMake (`sdk/cpp/tests/`: `test_client.cpp`, `test_env.cpp`, `test_fragmentation.cpp`, `test_session_close.cpp`, `test_send_action.cpp`, `test_plugin_ping.cpp`) covering framing, MAC, fragmentation, session close, and the plugin run loop against a live kernel.
- **File:** `tests/integration/test_sdk_cpp.rs:1-91`
- **Issue:** the sole "C++ SDK" test spawns `target/debug/echo_plugin_rs` (Rust binary). No C++ binary is ever built/spawned/tested end-to-end. Combined with M6/M7 below (no C++ unit tests or fuzz coverage for framing), the SDK with manual memory management (`sdk/cpp/src/framing.cpp`) has no verification of its wire-parsing path against a live kernel at all.
- **Fix:** wire a real C++ echo-plugin binary into CI (CMake build) and point this test at it.

---

## High

### H1. Single-threaded router serializes all IPC fan-out behind per-message blocking timeouts — one slow plugin stalls the whole kernel
- **STATUS (2026-08-11): FIXED** — the router hot path is now non-blocking: `forward` (`protocol.rs:1001`), `broadcast`, and `EventBus::deliver` (`bus.rs:129`) use `try_send`; a slow/full target drops the frame with a counter, never stalls the shared router task. `try_send_envelope` (`protocol.rs:1147`) is the non-blocking primitive and `notify_forced_termination` documents the shared-loop invariant.
- **Files:** `src/ipc/protocol.rs:648-654` (`forward`), `src/ipc/protocol.rs:725-734` (`broadcast`), `src/events/bus.rs:127-153` (`EventBus::deliver`)
- **Issue:** `MessageRouter::run_with_context` is the sole consumer of the inbound message channel — every IPC message (unicast, broadcast, kernel commands, event-bus publishes) is processed serially in one task. `forward`/`broadcast`/`deliver` each `.await tokio::time::timeout(50ms, write_tx.send(...))` inline in this loop; `broadcast` loops over *every* registered plugin, so cost is `O(n) * up to 50ms`.
- **Impact:** a plugin that stops draining its own write channel (slow/malicious/compromised) causes every subsequent forward/broadcast/event targeting it to cost up to 50ms *on the shared router task*, stalling routing for all other plugins. Trivially triggered by any plugin with IPC-send permission or wildcard event subscription.
- **Fix:** move the bounded send off the shared router task — spawn per-target send tasks, or use `try_send` + bounded per-connection retry queue instead of blocking the router loop.

### H2. `config.yaml` permissions are inert at runtime — two disconnected permission surfaces
- **STATUS (2026-08-11): FIXED** — T-04 registration clamp (`protocol.rs:333-344`) narrows JWT/manifest-claimed permissions to the operator's config.yaml allowlist (`config_permissions`, built in `orchestrator.rs:157-163`); `validate_plugin_def` (`loader.rs:247-256`) cross-checks at boot and HTTP start. Residual form-sensitivity (exact-match vs `normalize_permission`) was tracked as **N2** and is now also FIXED (see N2 below).
- **Files:** `src/utils/config.rs:30-34`, `src/plugins/loader.rs:225-259` (`validate_plugin_def`), `src/ipc/protocol.rs:245-278`, `src/auth/permissions.rs:17-60`
- **Issue:** `config.yaml`'s `permissions:` list is cross-checked against `plugin.json` only at load time (`validate_plugin_def`). Runtime enforcement (`check_permission`) uses `PluginRegistry.manifest.permissions`, populated straight from the JWT's `claims.permissions` (or self-declared manifest if JWT auth is off) at registration time — with no link back to the operator-configured `config.yaml` list.
- **Impact:** an operator can restrict a plugin to `["network"]` in `config.yaml`, but the JWT actually issued to that plugin process can carry broader claims (e.g. `PERMISSION_KERNEL_ADMIN`), and the kernel enforces those claims verbatim. `docs/PLUGIN_REGISTRY_SCHEMA.md` implies config.yaml is the enforcement boundary; it isn't.
- **Fix:** either have the kernel mint the JWT/capability token from `config.yaml`'s list itself, or re-validate `claims.permissions` against `PluginDef.permissions` at registration and reject/narrow anything not operator-granted.

### H3. `POST /plugins/:id/start` bypasses `validate_plugin_def` — no kernel-compat or permission cross-check on HTTP-triggered start
- **STATUS (2026-08-11): FIXED** — `start_plugin` (`routes.rs:88`) calls `validate_plugin_def` before spawning and returns 403/422 on failure.
- **Files:** `src/api/routes.rs:87-103`, `src/plugins/loader.rs:41-148`
- **Issue:** `start_plugin` builds the spawn config via `PluginLoader::config_from_def` and calls `manager.start` directly, skipping `validate_plugin_def` (kernel-version compat check, unknown-permission rejection, config-cross-check) — that function is only called from boot-time `load_all`.
- **Impact:** a plugin rejected at boot (incompatible version, ungranted permission) can still be started later via HTTP with no re-validation, as long as it's listed in `config.yaml`.
- **Fix:** call `validate_plugin_def` inside `start_plugin` before spawning; return 422/403 on failure.

### H4. C++ and Python SDKs never send `EventAck` — every event to a stock plugin is silently dropped after retry budget
- **STATUS (2026-08-11): FIXED** — C++ (`sdk/cpp/include/veyron/plugin.hpp`) and Python (`sdk/python/veyron/plugin.py` + `client.ack_event`, `client.py:144`) SDKs route events to `on_event` and auto-ack on success, mirroring Rust.
- **Files:** `sdk/cpp/include/veyron/plugin.hpp:38-74`, `sdk/cpp/include/veyron/client.hpp`/`client.cpp` (no `ack_event`), `sdk/python/veyron/plugin.py:67-83`, `sdk/python/veyron/client.py`, vs. `sdk/rust/src/plugin.rs:134-143` (auto-acks). Kernel side: `src/events/store.rs:120-128`.
- **Issue:** Rust auto-calls `client.ack_event()` on successful `on_event`. C++ has no dedicated event path or `ack_event()` method at all — events fall through to generic `on_message`. Python similarly routes `Event` to `on_message` with no ack helper. Kernel marks un-acked events `dead` after `max_retries`.
- **Impact:** every event delivered to a stock C++/Python plugin is retried then permanently dropped unless the author hand-builds an `EventAck` Envelope — undocumented, unsupported by either SDK's public API.
- **Fix:** add `on_event`/auto-ack machinery + `ack_event()` client method to C++ and Python SDKs, mirroring Rust.

### H5. Rust SDK swallows `on_message` handler errors — `run()` reports success even after a fatal failure
- **STATUS (2026-08-11): FIXED** — `serve()` captures the handler error and propagates it after `on_shutdown()` instead of returning `Ok(())` (`sdk/rust/src/plugin.rs:122-166`).
- **File:** `sdk/rust/src/plugin.rs:144-157`
- **Issue:** `Err(_) => break` discards the error; function falls through to `on_shutdown().await` as the return value, so `run()`/`serve()` returns `Ok(())` even when the handler failed. Inconsistent with C++ (exception propagates through `catch(...)` → `on_shutdown()` → rethrow) and Python (exception propagates via `finally`).
- **Impact:** a supervisor/process-exit-code check built against the Rust SDK won't observe handler failures the other two SDKs surface loudly.
- **Fix:** propagate the error out of `serve()` (call `on_shutdown().await?` then `return Err(e)`).

---

## Medium

### M1. Error-count budget can be reset for the exact connection accruing errors
- **STATUS (2026-08-11): FIXED** — the tracked-error map prunes by staleness (`ERROR_BUDGET_IDLE_TTL`, `protocol.rs:104,242-245`), not registration state (T-08).
- **File:** `src/ipc/protocol.rs:190-196`
- **Issue:** when tracked-error map hits `max_tracked_error_conns` (8192), `retain` prunes by current registration status only, not staleness. An unregistered connection actively generating an error burst gets wiped along with the rest, restarting its counter at 1.
- **Impact:** a connection that keeps the map at/above threshold can bypass the per-connection error throttle indefinitely.
- **Fix:** prune by idle/LRU or last-error timestamp, not registration state.

### M2. WebSocket gateway has no cap on concurrent connections
- **STATUS (2026-08-11): FIXED** — `max_ws_connections` enforced before upgrade (`websocket.rs`, T-09); default 1024 (`config.rs:256-258`).
- **File:** `src/api/websocket.rs` `ws_handler` (line 47), vs. `src/ipc/server.rs:80-87` (UDS has explicit `max_connections`)
- **Issue:** no equivalent ceiling for WS upgrades; each accepted socket spawns its own task+channel. WS messages funnel into the same single-threaded router (H1), so unbounded WS connections widen that attack surface.
- **Fix:** add `max_ws_connections` config, enforce before upgrade.

### M3. `get_plugin_logs` `lines` query param has no explicit upper bound
- **STATUS (2026-08-11): FIXED** — explicit `min(n, MAX_LOG_LINES)` clamp at the route (`MAX_LOG_LINES = 10_000`).
- **File:** `src/api/routes.rs:126-141`
- **Issue:** `q.lines.unwrap_or(100)` unclamped before passing to `manager.logs`. Currently bounded incidentally by the log ring buffer (`log_buffer_lines`, default 1000) — not an intentional API-boundary validation.
- **Fix:** explicit `min(n, MAX_LOG_LINES)` at the route.

### M4. Marketplace trust model has no independent signature over registry/archives
- **STATUS (2026-08-11): FIXED** — registry entries carry a maintainer Ed25519 signature verified against a pinned public key (`verify_entry_signature`, `registry.rs:73`).
- **File:** `src/marketplace/installer.rs:123-131`, `src/marketplace/registry.rs:10-11`
- **Issue:** archive integrity is checked against the `sha256` listed in `registry.json` itself, fetched from a single hardcoded (or operator-supplied) URL. No detached signature independent of that channel.
- **Impact:** compromise of the registry-serving channel lets an attacker publish a malicious archive with a self-consistent hash.
- **Fix:** add maintainer-signed manifest (detached sig verified against a pinned public key) before trusting `sha256`/`archive_url`.

### M5. JWT secret has no minimum-strength enforcement
- **STATUS (2026-08-11): FIXED** — `MIN_JWT_SECRET_BYTES` bail at kernel start (`orchestrator.rs:121-129`).
- **File:** `src/auth/jwt.rs:19-27`
- **Issue:** `JwtValidator::new` accepts any non-empty secret for HS256 with no length/entropy check. Algorithm is correctly hardcoded (no alg-confusion), but a weak configured secret is brute-forceable offline.
- **Fix:** reject/warn on `jwt_secret` shorter than 32 bytes where the validator is constructed (`src/kernel/orchestrator.rs:116-131`).

### M6. No unit test coverage of C++ frame parsing against malformed input
- **STATUS (2026-08-11): FIXED** — C++ framing/reassembly adversarial coverage in the CMake gtest suite (`sdk/cpp/tests/test_fragmentation.cpp`); Python framing/MAC covered by `tests/python/test_framing_mac.py`.
- **File:** `sdk/cpp/tests/` (no `test_framing.cpp`)
- **Issue:** nothing exercises `read_frame_full` (`sdk/cpp/src/framing.cpp:156-218`) against bad magic, truncated header/payload, CRC mismatch, oversized length, corrupt zstd frames, or MAC failure — exactly the highest memory-risk code path (manual `memcpy`/buffer sizing).
- **Fix:** add adversarial-input unit tests for `read_frame_full`/`pack_frame_mac`.

### M7. No fuzz coverage for C++/Python framing/decompression
- **STATUS (2026-08-11): DEFERRED** — still open; Rust `cargo-fuzz` targets only. This is the single remaining substantive coverage gap (see Priority Recommendation). Tracked in `ROADMAP.md`.
- **File:** `fuzz/fuzz_targets/*` (Rust `wire` crate only)
- **Issue:** C++ (`sdk/cpp/src/framing.cpp`) and Python (`sdk/python/veyron/framing.py`) reimplement CRC32/header packing/bounded-decompress with no fuzz harness, despite C++ doing manual buffer arithmetic.
- **Fix:** add a libFuzzer harness for `framing.cpp`.

### M8. `FRAME_READ_TIMEOUT` slow-loris protection missing in C++/Python SDKs
- **STATUS (2026-08-11): FIXED** — per-frame read timeout in both SDKs: `read_frame_full_with_timeout` + `FRAME_READ_TIMEOUT_MS` (`sdk/cpp/include/veyron/framing.hpp:113-119`), `FRAME_READ_TIMEOUT = 10.0` (`sdk/python/veyron/framing.py:25`), both mirroring `wire/src/framing.rs`.
- **Files:** `wire/src/framing.rs:66,179-196` (10s timeout, Rust only) vs. `sdk/cpp/src/framing.cpp:110-120` (`recv_exact`, plain blocking read) and `sdk/python/veyron/framing.py:127-150` (plain `readexactly`)
- **Impact:** a peer that sends a valid header declaring a large payload then stalls hangs the C++/Python receive loop indefinitely (per-connection DoS).
- **Fix:** wrap payload/MAC read phase in a per-frame timeout in both SDKs.

### M9. `ActionStatus`/`CommandStatus` proto enums default to OK (zero-value footgun)
- **STATUS (2026-08-13): FIXED** — shipped with the protocol v1.5 bump (P11-03): `ActionStatus` gains `ACTION_UNKNOWN = 0` (OK/ERROR/... → 1..7), `CommandStatus` moves `COMMAND_UNKNOWN` to 0 (OK/ERROR → 1/2). The interim T-16 lint remains as a construction-site guard. See `ROADMAP.md` P11-03.
- **File:** `proto/veyron_protocol.proto:138-144,165-170`
- **Issue:** `ACTION_OK = 0`, `COMMAND_OK = 0` — unlike every other status enum in the file (`*_UNKNOWN = 0` pattern). A missed `set_status()` call anywhere silently reports success.
- **Fix:** wire-breaking to renumber now; at minimum add a lint/test asserting every construction site sets `status` explicitly. Track for next protocol version bump.

### M10. Proto file manually copied to three locations with no CI drift check
- **STATUS (2026-08-11): FIXED** — R8-05 byte-identity drift test across `wire/proto`, `sdk/python/proto`, `sdk/cpp/proto` (`tests/unit/test_proto_sync.rs`). The fmt violation that file caused is resolved (see N5).
- **Files:** `wire/proto/`, `sdk/cpp/proto/`, `sdk/python/proto/` (currently identical, but `sdk/cpp/CMakeLists.txt:27` says "re-copy by hand")
- **Fix:** add CI diff/checksum check across the three copies, or generate SDK copies from one source at build time.

### M11. C++ SDK has no fragmentation support — silently mis-parses fragmented frames
- **STATUS (2026-08-11): FIXED** — C++ fragmentation implemented: `absorb_fragment`/bounded reassembly + `send_fragmented` (`sdk/cpp/src/client.cpp:103-157,198-…`), gated by `MAX_REASSEMBLY_STREAMS`/`MAX_PAYLOAD_SIZE`.
- **Files:** `sdk/cpp/src/framing.cpp`/`client.cpp` (no `send_fragmented`/reassembly) vs. `sdk/rust/src/client.rs:355-420` and `sdk/python/veyron/client.py:164-197` (both implement bounded reassembly)
- **Impact:** a C++ plugin receiving a fragmented frame hands raw bytes straight to `Envelope::ParseFromArray`, which fails or silently misparses.
- **Fix:** port fragmentation to C++, or make `read_frame_full` explicitly reject `FLAG_FRAGMENTED` with a clear error.

---

## Low

### L1. `required_permission_for_action` checks the action *provider's* permission, not the requester's
- **STATUS (2026-08-11): FIXED** — T-19: `required_permission_for_action` (`permissions.rs:12-17`) now requires BOTH provider and requester to hold the permission for `http_request`; peer unicast is gated by `check_ipc_send`/`check_ipc_target` inside `forward` (`protocol.rs:941-966`).
- **Files:** `src/auth/permissions.rs:10-15`, `src/ipc/protocol.rs:413-430`
- **Issue:** for a routed `ActionRequest`, only the provider plugin's permission (e.g. `PERMISSION_NETWORK`) is checked — the requesting plugin needs no permission at all. An unprivileged plugin can transitively trigger network requests via any network-capable provider.
- **Note:** may be intentional design (provider-declares-authorization model) but should be explicitly documented given it's easy to misread the recent permission-gating work as requester-side.

### L2. `strncpy` silently truncates long socket paths in C++ client
- **STATUS (2026-08-11): FIXED** — explicit `sun_path` length check throws (`client.cpp:44-48`) with regression test (`test_client.cpp: RejectsOverlongSocketPath`).
- **File:** `sdk/cpp/src/client.cpp:26-28`
- **Issue:** not a buffer overflow (bounded copy into zero-initialized struct), but a path longer than `sizeof(sun_path)-1` (~107 bytes) is silently truncated rather than rejected, risking connect-to-wrong-path.
- **Fix:** check length explicitly and throw if too long.

---

## New findings — 2026-08-11 pass

### N1. `forward()` clones the full payload on every hop — contradicts the "zero copies" claim (Moderate)
- **STATUS (2026-08-11): CLOSED — non-issue as written.** `Frame.payload` is already `Arc<[u8]>` in the `wire` submodule (v0.2.0, `wire/src/framing.rs:69-81`), so `msg.frame.payload.clone()` in `forward`/`broadcast` is a cheap refcount bump, not a full `Vec<u8>` heap copy. The finding predates the wire-crate split (`.worktrees/wire-crate-split`) and was stale by the time the 2026-08-11 audit landed. Regression tests now lock in the sharing semantics: `forward_shares_payload_without_copy` / `broadcast_shares_payload_without_copy` (`tests/unit/test_router.rs`, `Arc::ptr_eq` on a 64 KiB payload). README §3 needs no change — the "zero copies" claim holds.
- **Files:** `src/ipc/protocol.rs:995`, vs. README manifesto §3 ("forwarded with zero copies")
- **Issue:** the hot unicast path does `payload: msg.frame.payload.clone()` — a full `Vec<u8>` heap copy of every forwarded payload (up to 1 MiB) per frame. The event path already shares bytes via `Arc<[u8]>` (`src/events/bus.rs:121`); the router's forward path doesn't. `broadcast` has the same pattern.
- **Impact:** unnecessary per-message allocation on the shared router task; at high throughput this is the dominant router cost and contradicts the documented design.
- **Fix:** route a payload-sharing frame (`Arc<[u8]>`) through the router loop, mirroring `EventBus::deliver`; update README if the sharing semantics change.
- **Tracked:** ROADMAP N1.

### N2. Permission comparison is form-sensitive: clamp + config cross-check use exact match, runtime checks normalize (Low, fails-closed)
- **STATUS (2026-08-11): FIXED** — both comparison sites now normalize via `normalize_permission` (made `pub(crate)`): the T-04 clamp builds a normalized `HashSet` (`protocol.rs:336-343`) and `validate_plugin_def` matches with a normalized `any` over `def.permissions` (`loader.rs:250-260`). Covered by `registration_clamps_jwt_permissions_to_config_allowlist` (now parametrized for both the proto and lowercase config forms, `tests/unit/test_router.rs`) and `config_lowercase_perm_matches_manifest_proto_form` (`tests/unit/test_manifest_enforcement.rs`, incl. negative control).
- **Files:** `src/ipc/protocol.rs:336` (`manifest.permissions.retain(|p| allowed.contains(p))`), `src/plugins/loader.rs:249` (`def.permissions.contains(perm)`), vs. `src/auth/permissions.rs:21-25,36` (`normalize_permission`)
- **Issue:** the T-04 registration clamp and the boot-time config cross-check compare permission strings with exact equality, but runtime `check_permission` accepts both the lowercase documented form (`network`) and the proto name (`PERMISSION_NETWORK`). A config.yaml `permissions: [network]` with a token/manifest claiming `PERMISSION_NETWORK` silently strips the permission at registration (warn only) or refuses boot in `validate_plugin_def`.
- **Impact:** no escalation (fails-closed), but a configuration footgun that silently downgrades or blocks legitimate plugins — and `normalize_permission` already exists for exactly this.
- **Fix:** normalize both sides in the clamp and the config cross-check; extend `registration_clamps_jwt_permissions_to_config_allowlist` (`tests/unit/test_router.rs:948`) to cover both forms.
- **Tracked:** ROADMAP N2.

### N3. `load_config` performs no numeric bounds validation (Low)
- **STATUS (2026-08-11): FIXED** — `load_config` clamps `router_channel_capacity`, `max_connections`, `watchdog_interval_secs`, `watchdog_timeout_secs` from `0` to the documented defaults with a `warn!` (`src/utils/config.rs`, `clamp_invalid_numerics`). Fields are unsigned so negatives already fail serde. Covered by `load_config_clamps_zero_numerics_to_defaults` and `load_config_preserves_sane_numerics` (config `mod tests`).
- **File:** `src/utils/config.rs:318`
- **Issue:** `serde_yaml` → `Config` with no clamping — `router_channel_capacity: 0` (rendezvous channel), `max_connections: 0`, negative watchdogs are all accepted silently.
- **Fix:** clamp/validate numerics at parse (fall back to defaults or error loudly).
- **Tracked:** ROADMAP N3.

### N4. Daemon start reports success before the child holds the pid-file lock (Low, TOCTOU)
- **STATUS (2026-08-11): FIXED** — readiness handshake via `UnixStream::pair()`: `daemonize_and_run` passes the write end to the re-exec'd child (`VEYRON_READY_FD`, `FD_CLOEXEC` cleared in `pre_exec`) and blocks (10s timeout) for a `"{pid}\n"` line; `run_foreground` emits it only after the exclusive flock + pid write. The parent publishes the pid file and reports success only after the line (which must match the child pid); a child that dies or times out is SIGKILLed, reaped, its pid file removed, and the start errors out (smoke-verified: happy path `start → status → stop`; failure path exits 1 with "kernel child exited before signaling readiness", no stray child, no pid file).
- **Files:** `src/main.rs:213-236` (`daemonize_and_run`), `src/main.rs:238-264` (`run_foreground`)
- **Issue:** `daemonize_and_run` spawns the re-exec'd child, writes its pid, and returns success. The child then acquires the exclusive flock in `run_foreground` — if a competing instance wins the lock first, the child aborts with "already running" while the parent already told the operator "started".
- **Fix:** readiness handshake — the child reports success (exit status or explicit ready line) before the parent confirms.
- **Tracked:** ROADMAP N4.

### N5. `cargo fmt --check` fails on `tests/unit/test_proto_sync.rs:43` (Low, DoD violation)
- **STATUS (2026-08-11): FIXED** — `cargo fmt` run across the tree; `cargo fmt --check` now exits 0.
- **File:** `tests/unit/test_proto_sync.rs:43` (unformatted closure, introduced `61aec96`)
- **Issue:** the repo's Definition of Done requires `cargo fmt --check` clean; this is the only offender.
- **Fix:** run `cargo fmt` on the file and commit.
- **Tracked:** ROADMAP N5.

---

## Delta audit — 2026-08-14 (post-reconciliation)

Fresh full-repo audit on `develop` @ `2d16ebf` (delta since `c93342b`: 84
files, +7671/−1729 — marketplace registry v2 (R10-03), installed-state store
(R10-02), plugin enable/disable (R10-04), manifest v2 per-action permissions,
Landlock/seccomp/shim). Method: three parallel read-only passes (security
surface / performance / API+UX) + `cargo audit` + targeted codegraph
verification. The 2026-08-11 findings re-confirmed shipped; the new findings
below are in post-audit code or previously un-audited surfaces (perf/UX).
All 13 are **OPEN** — prioritized plan in `ROADMAP.md` ("Immediate — Delta
audit findings", priorities P0–P3).

### S1. Registry entry signature does not bind `status`/`archive_url` — revocation bypass + download redirect (Medium, P0)

- **Files:** `src/marketplace/registry.rs:65-70` (`signature` field: signed
  over `"{slug}:{version}:{sha256}"` only), `registry.rs:85-87`
  (`is_revoked`), `installer.rs:181` (`is_revoked` gate),
  `registry.rs:507-522` (`resolve_relative_archive_urls`)
- **Issue:** `verify_entry_signature` verifies an Ed25519 signature over
  `{slug}:{version}:{sha256}`. `status` (the revocation bit), `archive_url`,
  `min/max_kernel_version` and `permissions` are NOT covered. A compromised
  registry-serving channel (the exact threat model the signature was added
  for — M4/T-11) can:
  1. flip `status: revoked → stable` — the entry still verifies, the
     `is_revoked` gate passes, and a revoked plugin installs (**revocation
     bypass** — R10-03's "revocation outlives the TTL" is defeated by the
     same channel compromise it was built to survive);
  2. redirect `archive_url` to an arbitrary URL — the kernel fetches it before
     the sha256 check fails (request forgery / internal-network scanning;
     content integrity survives via the signed sha256, so no code execution);
  3. loosen `min/max_kernel_version` and `permissions` on the entry.
- **Impact:** revoked plugins install; SSRF-class request forgery from the
  operator's host; compat/permission gates weakened.
- **Fix:** sign the full canonical entry (at minimum
  `slug:version:sha256:status:archive_url:min_kernel_version:max_kernel_version`).
- **Regression check:** flipping `status` or `archive_url` on a signed entry
  fails `verify_entry_signature`.
- **Status (2026-08-14): FIXED** — `signed_message` covers the full canonical
  entry, as served (relative `archive_url` verifies in raw form); relative-URL
  resolution moved from `fetch_registry_from` into `install()` (after
  verification, before the download — a forged URL is never fetched); cache
  schema bumped to v2. Tests:
  `signature_rejected_when_{status,archive_url,kernel_bounds}_tampered`,
  `relative_archive_url_verifies_in_as_served_form`,
  `fetch_keeps_relative_archive_url_as_served`,
  `install_rejects_{unverified,archive_url_tamper}_before_download`.

### S2. `data_dir: /tmp/veyron` puts the events SQLite DB in world-writable /tmp (Low-Med, P1)

- **Files:** `config.yaml:8`, `src/events/store.rs:13-16` (`EventStore::new`:
  `create_dir_all` + symlink-following `Connection::open`), `src/utils/config.rs`
- **Issue:** every other runtime path (socket, pid, log, marketplace
  state/cache) was hardened to a per-user private dir (M-09); `data_dir` is
  the one exception and contradicts the config file's own comment ("never the
  shared /tmp").
- **Impact:** on a multi-user host, a local user can pre-create `/tmp/veyron`
  before the kernel starts, then read or modify the event store — including
  forging `pending` events that the retry worker (`bus.rs:160-180`)
  redelivers to subscribers — or symlink `events.db` elsewhere.
- **Fix:** default `data_dir` to the per-user private runtime dir; create the
  store dir 0o700 with an ownership check.
- **Status (2026-08-18): FIXED** — `default_data_dir()` uses
  `veyron_wire::socket::default_private_dir()` (XDG_RUNTIME_DIR pattern,
  same as M-09); `EventStore::new` rejects world-writable dirs (`mode &
  0o002`); shipped via PR #35.

### S3. RUSTSEC-2026-0204 `crossbeam-epoch` 0.9.18 (Low, P1)

- **File:** `Cargo.lock:505` — invalid pointer dereference in the
  `fmt::Pointer` impl; reached via `metrics-exporter-prometheus 0.15.3 →
  metrics-util 0.17.0`.
- **Fix:** `cargo update -p crossbeam-epoch` → 0.9.20 (verified, one package).
- **Status (2026-08-14): FIXED** — 0.9.20 (PR #20); `cargo audit` reports no
  RUSTSEC-2026-0204.

### S4. Dependency advisories: `anyhow` unsoundness + `number_prefix` unmaintained (Low, P3)

- **Files:** `Cargo.lock`
- **Issue:** RUSTSEC-2026-0190 (`Error::downcast_mut` unsoundness, anyhow
  1.0.102); RUSTSEC-2025-0119 (`number_prefix` unmaintained). Both warnings,
  not vulnerabilities.

### S5. Internals leak into plugin-facing errors (Low, P2)

- **Files:** `src/ipc/protocol.rs:322` (`auth failed: {e}` — raw jsonwebtoken
  error detail to the registering plugin), `src/ipc/protocol.rs:654`
  (`ActionResponse.error = format!("{:?}", status)` — Debug enum name)
- **Fix:** map to stable, documented wire error codes/messages.

### PERF-1. Router kernel replies block on `.send().await` — one slow plugin stalls all IPC (Medium, P1)

- **Files:** `src/ipc/protocol.rs:1145-1149` (`send_envelope` awaits the
  target's write channel from the single shared router task), `:1011,1085`
  (`try_send` on peer forwards — the T-03 fix), `src/ipc/connection.rs:135`
  (64-slot write channel)
- **Issue:** peer forwarding is non-blocking, but every kernel reply (Pong,
  acks, error frames) still `.await`s a full 64-slot channel on the shared
  router task. One plugin that stops draining its channel stalls the whole
  IPC fabric.
- **Fix:** `try_send` + bounded overflow handling, or a per-connection send
  task.

### PERF-2. Synchronous SQLite + std Mutex on the async runtime in the router path (Medium, P1)

- **Files:** `src/events/store.rs:9,38` (`std::sync::Mutex<Connection>` held
  during disk I/O), `src/events/bus.rs:85` (`store.persist` from the router
  task via `EventBus::publish`), `src/ipc/protocol.rs:929` (`mark_delivered`),
  `src/events/bus.rs:160-180` (retry worker)
- **Impact:** blocking SQLite writes under a std mutex on tokio workers in
  the hottest path (every event publish / ack).
- **Fix:** `tokio::task::spawn_blocking` or a dedicated writer task.

### PERF-3. Per-message full `PluginEntry` clones + O(n) registry scans (Low-Med, P2)

- **Files:** `src/plugins/registry.rs:159` (`get` clones the entry incl. the
  manifest proto; ~4 per forwarded message via `get_by_conn_id` +
  `check_ipc_send` + `check_ipc_target` + `get`), `:204-217`
  (`find_action_provider` O(P)), `:339-344` (`count_pending_actions_for`),
  `:366-375` (`find_pending_internal_id` per chunk), `:185-190` (`list`
  clone-all per broadcast)
- **Fix:** `Arc<PluginEntry>` / split hot fields; action→provider index.

### PERF-4. Hot-path constant-factor costs (Low, P3)

- **Files:** `../veyron-wire/src/framing.rs:137-152,240` (synchronous zstd in
  async tasks; double CRC32 per outbound frame), `src/plugins/supervisor.rs:852,864`
  (sync `/proc` reads in the watchdog loop), `src/api/websocket.rs:220,246-258`
  (double payload copy per WS frame)
- **Fix:** drop the redundant second CRC; offload zstd; move `/proc` reads off
  the async runtime.

### UX-1. REST errors are bare `StatusCode` with no body; lie-prone statuses (Medium, P2)

- **Files:** `src/api/routes.rs` (bare `StatusCode` returns; 422 collapses
  invalid-manifest vs spawn-failure; `stop_plugin` returns 200 even when the
  stop failed, `routes.rs:115`), `src/api/rate_limit.rs:38-43` (the only
  body'd error), `src/api/websocket.rs:61,75` (plain-text upgrade failures)
- **Fix:** JSON error envelope (code/message/retryable); honest stop status;
  document the API (OpenAPI or README reference).

### UX-2. Debug repr leaks into public API shapes (Low-Med, P2)

- **Files:** `src/api/routes.rs:59` (`PluginInfo.state = format!("{:?}",
  e.state)` — a Rust Debug enum name is the public field)
- **Fix:** stable, documented string/enum values.

### UX-3. Config validation gaps + silent parse-error swallowing (Low, P3)

- **Files:** `src/plugins/loader.rs:19-23` (unknown `restart:` silently →
  `on-failure`, while `max_fs_access` warns — two conventions), `src/utils/config.rs`
  (bad `log_level` → EnvFilter matches nothing → silent no-logs; binary
  defaults port 8000 vs shipped `config.yaml` 8888), `src/main.rs:85-123`
  (non-start subcommands `.unwrap_or_default()` on config load errors)
- **Fix:** consistent validation + warnings; surface load errors to all CLI
  subcommands, not just `start`.

### UX-4. CLI polish (Low, P3)

- **Files:** `src/cli/mod.rs` (sparse subcommand `about` text, hardcoded
  version string), `src/cli/plugin.rs:135` (`vyn plugin logs` prints the raw
  JSON array; mixed ✓/⚠/plain output style)

**Confirmed sound on re-check (delta):** MAC scheme + header coverage, fragment
reassembly bounds, UDS 0o600 + non-socket refusal, JWT min-secret + HS256-only,
T-04 clamp + form normalization, per-action permission dual-check (manifest v2
fail-closed on unknown perms), zip-slip + sha256 + signature + atomic rename +
revocation check in the installer, seccomp/Landlock fail-closed, WS frame
parser bounds, rate-limit keyed on verified `sub`, no sensitive values logged
(WS token withheld, `jwt_secret` only as length). No regressions in the
previously-hardened areas.

---

## Architecture audit — dumb-core (2026-08-16)

Fresh manifesto-compliance audit: does the kernel stay a "dumb byte router +
process supervisor" (README §1, ROADMAP Manifesto), or has domain logic crept
into the core? Method: full module responsibility map (`src/kernel`, `src/api`,
`src/plugins`, `src/events`, `src/ipc`, `src/marketplace`, `src/bridge`,
`src/cli`, `src/auth`), wire-protocol schema review, and a DB-usage trace
(SQLite event store). Full report + fix plan: `docs/DUMB_CORE_AUDIT.md`.

**Verdict: manifesto declared, code partially drifted.** The IPC/supervision/
sandbox/auth core is genuinely dumb; four blocks of product-level logic have
grown into the kernel (marketplace app-store client, device-fleet domain,
AI tool-calling surface, hardcoded action→permission policy), and the events
SQLite DB technically contradicts the manifesto's literal "no databases"
clause (it is infrastructure, not application state — see DC-5). All five
findings below are **OPEN** (2026-08-16); fix plan in `docs/DUMB_CORE_AUDIT.md`.

### DC-1. Marketplace / plugin app-store client embedded in the kernel (Medium)

- **Files:** `src/marketplace/registry.rs` (1509 L: `DEFAULT_REGISTRY_URL` → veyron-plugins GitHub, `:15-16`; maintainer Ed25519 key pinned in kernel source, `:38-39`; kernel-compat policy, `:626-661`), `src/marketplace/installer.rs` (822 L: download→sha256→zip→atomic-rename pipeline, installed.json ledger, drop-in config write; **hardcoded business rule `sandbox = plugin_id != "network"`, `:647`**), `src/marketplace/state.rs` (154 L), `src/cli/plugin.rs` (`vyn plugin list/search/install/remove/enable/disable`)
- **Issue:** a full plugin distribution/app-store client (catalog fetch, signature verification, revocation governance, install/uninstall, upgrade detection, package-state ledger) is compiled into the kernel. Package management and marketplace governance are product features, not byte-routing.
- **Impact:** kernel grows product-specific policy (which registry, which maintainer key, which plugin is exempt from sandboxing); every marketplace change ships a kernel release.
- **Fix:** extract to a `marketplace` plugin (or separate binary) that drives the kernel only through the existing plugin-lifecycle surface (`plugins.d/` drop-ins). The kernel may keep signed-archive verification only if it stays a security boundary.
- **Status (2026-08-16): OPEN.**

### DC-2. Device-fleet domain model in the kernel (D-01…D-14) (Medium)

- **Files:** `src/plugins/registry.rs` (`DeviceMeta` `:17-24`; `devices` DashMap `:91`; upsert on registration `:206-229`; offline transition `:239-247`; `record_pong`/`last_seen` `:250-260`; `get_device`/`list_devices` `:271-278`), `src/api/routes.rs` (`DeviceInfoView` `:107-117`, `list_devices` `:119-136`), `src/api/server.rs:89` (`GET /devices`), `src/kernel/commands.rs:61-78` (`list_devices` IPC command), `src/cli/device.rs` (QR pairing embedding the master `jwt_secret`, `:26-36,126-150`), `src/cli/token.rs` (per-device JWT minting), `src/cli/devices.rs`, `src/bridge/mod.rs` (810 L — `role: client` mirrors local plugins to a host as `device.<cap>`), wire proto `PluginRegister` device fields `:79-88`, `DeviceInfo/DeviceOs/DeviceState` `:107-133`
- **Issue:** device identity, capabilities negotiation, online/offline state machine, discovery surfaces, per-device JWT minting, QR-pairing UX and a remote-device bridge are product features embedded in the core. Defensible slice: `device_id` in the JWT `sub` is auth infrastructure. The discovery/interpretation/pairing surfaces are not.
- **Impact:** the kernel owns a whole device-fleet product domain; changing device UX ships a kernel release.
- **Fix:** keep device identity in the kernel (auth); move discovery surfaces (`GET /devices`, `list_devices`, `vyn devices`), pairing tooling and the bridge into plugins / companion tools.
- **Status (2026-08-16): OPEN.**

### DC-3. AI tool-calling surface baked into protocol and kernel (Low-Med)

- **Files:** wire proto `ActionSpec`/`ActionRisk` — *"tool schema for the AI (D-08)"*, `:159-173`; `src/kernel/commands.rs` `get_manifest` (`:79-127`, comment: *"serve a plugin's manifest (incl. action_specs) to the AI"*); `src/events/bus.rs` `plugin_lifecycle_payload` (`:223-259` — action_specs embedded in `system.plugin_joined/left` *"so the AI can enumerate callable actions from the joined event alone"*)
- **Issue:** the kernel is explicitly shaped for an AI-agent frontend (README's Kairo framing). Tool-schema interpretation (risk levels, `requires_confirmation`, params_schema) is domain logic.
- **Fix:** policy decision required — either accept `action_specs` as a generic manifest feature (document it as such) or move tool-schema interpretation to the AI plugin and strip it from lifecycle events.
- **Status (2026-08-16): OPEN (decision).**

### DC-4. Hardcoded action→permission policy (Low)

- **File:** `src/auth/permissions.rs:12-17` — `required_permission_for_action("http_request") → PERMISSION_NETWORK`
- **Issue:** the kernel hardcodes knowledge of a specific plugin's ("network") action name as the fallback permission map. The data-driven v2 path (`registry.action_requirement`, `loader.rs:74-90`) supersedes it, but the fallback remains and the comment says new sensitive actions must be added to the kernel.
- **Fix:** drop the fallback; require v2 per-action permission declarations (fail-closed on undeclared sensitive actions).
- **Status (2026-08-16): OPEN.**

### DC-5. Events SQLite DB vs the manifesto's "no databases" clause (Info/Low-Med)

- **Files:** `src/events/store.rs` (single table `events(event_id, event_type, payload_json, status, created_at, retry_count)`, `:17-26`), `src/events/bus.rs` (`persist` on publish `:84-89`, retry worker `:180-200`), `src/kernel/orchestrator.rs:91` (opens at boot; failure non-fatal), `src/ipc/protocol.rs:1024-1028` (EventAck → `mark_delivered`)
- **Issue:** the DB is a **delivery outbox for at-least-once event delivery** — infrastructure, not application state (the plugin registry is honestly in-memory DashMap; marketplace state is JSON files). But the manifesto's literal "no databases" wording is violated; `payload_json` BLOB transiently stores full plugin event payloads (bounded: 1h retention prune); open audit items S2 (world-writable `/tmp` default `data_dir`) and PERF-2 (sync rusqlite under `std::sync::Mutex` on tokio workers) sit on this path.
- **Fix:** amend the manifesto to carve out the delivery outbox explicitly ("no databases *for application state*; the event-delivery outbox is an exception"); keep the DB; close S2 + PERF-2 (0o700 private runtime dir + `spawn_blocking`/dedicated writer task).
- **Status (2026-08-16): OPEN (manifesto wording + S2/PERF-2).**

**Confirmed dumb-core-clean (do not re-litigate without new evidence):** zero-parse
frame routing + MAC + fragmentation (`src/ipc`, `wire/`), process supervision
and restart policy (`src/plugins/supervisor.rs`), sandbox isolation — namespaces,
cgroup v2 pids, Landlock, seccomp (`runner/shim/seccomp/fsaccess`), JWT + frame
MAC + default-deny permissions (`src/auth`), event-bus delivery mechanics, API
gateway plumbing, metrics, TLS. No domain logic found in `src/kernel/orchestrator.rs`,
`src/ipc/protocol.rs` routing paths, or the supervisor/runner stack.

---

## Informational / Follow-ups

- **Framing code relocated to `wire/` crate:** `src/ipc/framing.rs` is now a re-export shim; actual length-prefix/allocation logic lives in `wire/src/framing.rs`, which was out of scope for the kernel-focused pass but was covered by the SDK/proto pass (see H4, M6-M11) — no gap, just noting the split (`.worktrees/wire-crate-split` present in repo).
- **PID-namespace isolation is now tracked in `ROADMAP.md` Phase 9 (R9-02),** not here — the README §5 "tracked in AUDIT.md" pointer for the shim supervisor is stale and its fix is ROADMAP R9-06.
- Extensive prior hardening confirmed sound on re-check: UDS socket perms/TOCTOU, fragment reassembly bounds, frame MAC ordering, action-response provider-spoofing prevention (`take_pending_action_if_provider`), rate-limiter key pruning, zip-slip protection in marketplace installer, resource limits applied regardless of `sandbox:true`, supervisor restart backoff, JWT expiry/algorithm hardcoding, constant-time MAC comparison across all three SDKs, no pickle/eval/shell-subprocess in Python SDK, disciplined `reserved` field usage in proto. No regressions found in these areas — do not re-litigate without new evidence.
- Verification evidence (2026-08-11): `cargo test --all --all-features` → 45 lib + 212 unit + 73 integration passed; `clippy --all-targets --all-features -- -D warnings` → clean; `cargo fmt --check` → clean (N5).

---

## Summary by Severity

| Severity | Count |
|----------|-------|
| Critical | 2 |
| High     | 5 |
| Medium   | 11 |
| Low      | 2 |
| **Original total** | **20** |

Fixed: **19/20**. Deferred: M7.
M9 (zero-value enum renumber) shipped with the protocol v1.5 bump
(P11-03, 2026-08-13) — `ActionStatus`/`CommandStatus` now have
`*_UNKNOWN = 0`, see ROADMAP.
New (2026-08-11): N1 (moderate), N2–N5 (low) — **all resolved**: N1 closed as
non-issue (wire v0.2.0 already shares the payload via `Arc<[u8]>`; sharing
locked in by regression tests), N2–N5 fixed with tests/evidence above.
Delta (2026-08-14): 13 new findings, **all open** — prioritized P0–P3 in
`ROADMAP.md`; see the "Delta audit — 2026-08-14" section above for details.

## Priority Recommendation

Original-priority ordering for the 2026-08-11 findings (all now shipped):

1. **N1** (router hot-path payload clone vs README claim) — closed as non-issue; wire v0.2.0 already routes `Arc<[u8]>`; `Arc::ptr_eq` regression tests added.
2. **N2** (permission form-sensitivity in clamp + `validate_plugin_def`) — both sites normalize via `normalize_permission`; covered for both config forms.
3. **N5** (`fmt --check` violation) — fixed; DoD gate restored.
4. **N3/N4** (config bounds validation, daemon-start TOCTOU) — fixed; config clamps zero numerics with `warn!`, daemon start now waits on a readiness handshake.

Remaining open:
1. **M7** (C++/Python fuzz harness) — the only remaining substantive coverage gap; libFuzzer for `framing.cpp` + Python header/frame fuzz.
2. **M9** (zero-value enum renumber) — **FIXED (P11-03, 2026-08-13)** via the protocol v1.5 bump: `ACTION_UNKNOWN = 0` added, `COMMAND_UNKNOWN` moved to 0, OK/ERROR shifted. The interim lint (T-16) remains as a construction-site guard.

Delta (2026-08-14) ordering (priorities P0–P3, tracked in `ROADMAP.md`):
1. **P0 — S1** (registry signature must bind the full entry: `status`/`archive_url`/compat). Trust-anchor correctness; everything else waits. **FIXED 2026-08-14.**
2. **P1 — S3** (one-command `crossbeam-epoch` CVE fix), **S2** (`data_dir` off shared /tmp — **FIXED 2026-08-18, PR #35**), **PERF-1** (router kernel replies off the shared-task `.send().await`), **PERF-2** (event-store SQLite off the async runtime).
3. **P2 — UX-1** (JSON error envelope + honest stop status), **S5** (stable wire error codes), **UX-2** (stable `PluginInfo.state`), **PERF-3** (`Arc<PluginEntry>` + action→provider index).
4. **P3 — PERF-4** (double CRC / sync zstd / `/proc` reads / WS copies), **UX-3** (config validation consistency), **UX-4** (CLI polish), **S4** (dependency advisories).

---

## Full src Audit — 2026-08-20 (maintainability & comments)

**Date:** 2026-08-20
**Scope:** `src/` — 49 файлов, 14251 LOC (kernel, api, auth, bridge, cli, events, ipc, plugins, marketplace, utils). Proto — single source of truth в `veyron-wire`, здесь только `proto.rs` реэкспорт.
**Method:** ручное line-by-line чтение каждого файла, без делегатов/агентов, как запрошено. Проверены: стиль комментов, размер файлов, DRY, error-handling, консистентность.
**Overall verdict:** кодбейз дисциплинированный, manifesto-compliant, security-first. Главная проблема — перерост: 6 файлов превышают лимит 250 LOC в 3–6 раз, комментарии дублируются и мешают чтению.

### Метрики

| Файл | LOC | Статус |
|---|---|---|
| `marketplace/registry.rs` | 1509 | 🔴 >6× лимита |
| `ipc/protocol.rs` | 1389 | 🔴 >5× |
| `plugins/supervisor.rs` | 933 | 🔴 |
| `utils/config.rs` | 922 | 🔴 |
| `marketplace/installer.rs` | 822 | 🔴 |
| `bridge/mod.rs` | 810 | 🔴 |
| `ipc/connection.rs` | 797 | 🔴 |
| `plugins/registry.rs` | 571 | 🟡 |
| остальные 41 файл | 12–538 | 🟢 |

Лимит 250 LOC из `references/` (programming skill) нарушен системно.

### A. Комментарии — аудит

**Что хорошо:**
- Везде объясняют `почему`, а не `что` — соответствует `CLAUDE.md` (lowercase, terse, commit-message tone). Лучшие примеры: `ipc/connection.rs:30-40`, `plugins/runner.rs:29-43`, `plugins/shim.rs:1-24` — без коммента про `pid_for_children + CLONE_THREAD = EINVAL` код непонятен.
- Каждый security-fix трассируется: `BUG-006`, `AUDIT M-09`, `T-11`, `R9-03`, `D-07` — позволяет найти `ROADMAP.md`.
- ToCТОU/namespace/Landlock rationale — образцовые.

**Что не так:**

**A1. Over-commenting (Medium).** Соотношение коммент/код ~1:1. В `utils/config.rs` каждое из 42 полей `Config` имеет 2–3 строки доки, в `ipc/protocol.rs` каждая ветка `match` — 5 строк. Тривиальное пересказывается: `// 1024 still bounds a runaway plugin while surviving ordinary session baselines` — достаточно `// caps runaway, survives desktop baseline`.

**A2. Tag soup без глоссария (Low-Med).** `T-11`, `S1`, `VULN-020`, `BUG-006`, `R9-02` непрозрачны для новичка. Нужен `docs/COMMENT_TAGS.md`: `tag → issue → файл`.

**A3. Несогласованный стиль (Low).** Смесь `/// Convenience constructor...` (Capital + period), `// kernel-assigned id...` (lowercase, no period), `//!` модульные доки. Выбрать один для `//` inline.

**A4. Дублирование объяснений (Low).** `socket 0o600 — не 0o777` объясняется 4 раза (`config.rs:272`, `ipc/server.rs:52`, `main.rs:479`, `utils/tls.rs:50`). Вынести в `docs/SECURITY.md` и ссылаться `// see docs/SECURITY.md#uds-0600`.

**A5. Тесты вперемешку с прод-кодом (Info).** `registry.rs` 800 LOC прод + 700 LOC тестов в одном файле — скролл. Рассмотреть `registry/tests.rs` или `#[cfg(test)] mod` в отдельном файле.

**Рекомендация по комментам:** оставить `почему` у нетривиальной логики (shim PID-ns, reassembly `buffered_bytes`, cgroup probing, `pre_exec` fd dance). Удалить пересказ очевидного. Ввести `docs/COMMENT_TAGS.md`. Привести `//` к lowercase без точки.

### B. Архитектура и DRY

**B1. Монолиты — главный риск (High).**
- `ipc/protocol.rs` (1389) — роутер + 12 обработчиков (`PluginRegister`, `ActionRequest`, `SessionClose`, `KernelCommand`...). Разбить: `ipc/router.rs` + `ipc/handlers/{register,action,session,event,kernel}.rs`.
- `marketplace/registry.rs` (1509) — `cache + fetch + verify + parse + resolve`. Разбить: `marketplace/registry/{cache,fetch,verify,parse}.rs`.
- `plugins/supervisor.rs` (933) — `spawn_internal` 200 LOC + `monitor_loop` + `watchdog_loop` + `graceful_shutdown`. Вынести `supervisor/spawn.rs`, `supervisor/watchdog.rs`.

**B2. Дублирование хелперов (Medium).**
- `target_bytes` / `frame_target` / `build_frame` скопированы в 5 местах: `ipc/protocol.rs:503,2400`, `bridge/mod.rs:506,501`, `events/bus.rs:202`, `plugins/supervisor.rs:383`, `api/websocket.rs:283`. Вынести в `ipc/helpers.rs` или `veyron_wire`.
- `resolve_ws_url` / `resolve_advertise_url` / `resolve_relative_archive_urls` — 3 копии URL-резолва (`bridge/mod.rs:356`, `cli/device.rs:160`, `marketplace/registry.rs:534`). Вынести в `utils/url.rs`.

**B3. `utils/config.rs` — God struct (Medium).**
- `Config` 42 поля, `Default` дублирует `default_*()` фns — легко рассинхронится. `clamp_invalid_numerics` клампит только 4 поля (`router_channel_capacity`, `max_connections`, `watchdog_*`), но `max_archive_bytes=0` или `max_ws_connections=0` не клампятся — inconsistent.
- Фикс: `#[derive(Default)]` + `#[serde(default="...")]` единообразно, клампить все `0`-invalid numerics, или валидировать и `bail!`.

**B4. `api/server.rs` — God constructor (Low-Med).**
- `create_router_full(10 args)` заглушен `clippy::too_many_arguments`. Передать `RouterConfig` struct.
- `tokio::spawn(prune limiter)` внутри конструктора роутера — в тестах спавнится фоновая задача без `JoinHandle`, течет. Вынести в `Kernel::run`.

**B5. `kernel/orchestrator.rs` (470) делает всё.** TLS resolve + `bind_ip` логика + bridge spawn + supervisor + watchdog + `disconnect_loop` ×2 + `graceful_shutdown`. Вынести `orchestrator/bind.rs`, `orchestrator/shutdown.rs`.

### C. Код-качество

**C1. Error handling — 3 системы (Low).**
- `VeyronError` + `anyhow::Error` + `Result<_, String>` (`auth/jwt.rs:58,83`). `validate()` возвращает `String` — ломает единообразие. Унифицировать на `VeyronError`.
- `main.rs` форматирует `e.to_string()` и теряет chain.

**C2. Глобальные Atomics (Low).**
- `MSG_SEQ`, `ACTION_CORRELATION_SEQ`, `EVENT_PUBLISH_SEQ` (`ipc/protocol.rs:30-32`) — process-wide, никогда не резетятся в тестах. Тесты зависят от порядка. Добавить `#[cfg(test)] fn reset_for_test()`.

**C3. `Mutex<Connection>` в `events/store.rs:9` (Medium — дублирует PERF-2).**
- Синхронный `rusqlite` под `std::sync::Mutex` блокирует tokio worker на каждом `publish`/`ack`. Нужно `spawn_blocking` или `sqlx`/dedicated writer task. `unwrap_or_else(|p| p.into_inner())` глушит poison — логируй.

**C4. `api/websocket.rs:229` дублирует `veyron_wire` фрейминг (Low-Med).**
- Кастомный `parse_frame` без `COMPRESSED/FRAGMENTED` — любой фикс фрейминга правится в 2 местах. Реюзать `veyron_wire::framing::read_frame` или вынести WS-фрейминг в wire.

**C5. `utils/logging.rs` — дублирование (Low).**
- 4 ветки `if json { with otel } else` дублируют 80% `fmt::layer()`. Вынести в `let fmt = fmt::layer()...`. `Registry::init()` паникует при втором вызове — в тестах упадет, сделай `try_init()`.

**C6. Deprecated API (Low).**
- `rand::thread_rng()` в `auth/jwt.rs:96` deprecated — заменить на `rand::rng()` / `OsRng`.

**C7. Неиспользуемый `BLOOM` / dead code (Info).**
- `workspace` `veyron-wire` — проверить `cargo clippy -- -D warnings` на `dead_code`.

### D. Безопасность — подтверждено sound, мелкие nits

Подтверждено sound на re-check (delta 2026-08-14 + этот аудит): MAC `FLAG_MAC_PRESENT` + `serialize_header` coverage, fragment reassembly bounds (`buffered_bytes`, `max_reassembly_streams`, `total` mismatch), UDS `0o600` + `O_NOFOLLOW` + non-socket refusal, JWT `MIN_JWT_SECRET_BYTES` + `HS256`-only + `aud/jti` nonce, T-04 clamp + `normalize_permission`, per-action dual-check (provider+requester), zip-slip + `sha256` + Ed25519 + atomic rename + `is_revoked`, seccomp/Landlock fail-closed, WS parser bounds, rate-limit keyed on `VerifiedSub`, no token leak в логах.

Nits этого аудита:
- `validate_slug` (`installer.rs:614`) и `validate_plugin_id` (`registry.rs:547`) — два разных regex для одного понятия, унифицировать.
- `jwt_secret` длина проверяется только в `orchestrator.rs:123`, `mint_device_token` не проверяет — добавить.
- `unsafe` в `main.rs:391` `pre_exec` — `BorrowedFd::borrow_raw(ready_fd)` валиден только потому что `ready_fd` dup через `CommandExt` `pre_exec` — добавить `debug_assert!` + коммент.

### E. Что поправить — приоритет

**P0 (до след. релиза):**
1. Разбить `ipc/protocol.rs` и `marketplace/registry.rs` — review невозможен.
2. Вынести `target_bytes/frame_target/build_frame` и `resolve_*_url` в `ipc/helpers` / `utils/url`.
3. Пофиксить `events/store.rs` — `spawn_blocking` для sqlite (дубль PERF-2).
4. Унифицировать `auth/jwt::validate() -> VeyronError`, заменить `thread_rng`.

**P1 (гигиена):**
5. Ввести `docs/COMMENT_TAGS.md`, сократить дублирующие комменты, привести `//` к lowercase.
6. Заменить `create_router_full(10 args)` на struct, убрать спавн prune из конструктора.
7. Починить `Config::Default` дублирование + клампить все `0`-invalid numerics.
8. Добавить `reset_for_test()` для глобальных секвенсов.

**P2 (полировка):**
9. Вынести `drain_to_log`, `proc_resource_usage` в `plugins/metrics.rs`.
10. Заменить `unwrap_or_else(p.into_inner())` — логировать poison.

### F. Методология этого аудита

Ручное чтение, без агентов, как запрошено. Каждый из 49 файлов открыт через `read`, проверены комменты и код. Метрики LOC через `wc -l`, `grep` не использовался для логики — только для подсчета.

**Auditor:** Sisyphus (muse-spark-1.2) · manual pass · 2026-08-20
**Commit audited:** `develop` HEAD на момент чтения (14k LOC src/ snapshot выше)
