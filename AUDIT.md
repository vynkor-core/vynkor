# Veyron Codebase Audit

Date: 2026-07-07 (initial) · **Reconciled: 2026-08-11** (full re-audit on `develop` @ `c93342b`)
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

## Priority Recommendation

Original-priority ordering for the 2026-08-11 findings (all now shipped):

1. **N1** (router hot-path payload clone vs README claim) — closed as non-issue; wire v0.2.0 already routes `Arc<[u8]>`; `Arc::ptr_eq` regression tests added.
2. **N2** (permission form-sensitivity in clamp + `validate_plugin_def`) — both sites normalize via `normalize_permission`; covered for both config forms.
3. **N5** (`fmt --check` violation) — fixed; DoD gate restored.
4. **N3/N4** (config bounds validation, daemon-start TOCTOU) — fixed; config clamps zero numerics with `warn!`, daemon start now waits on a readiness handshake.

Remaining open:
1. **M7** (C++/Python fuzz harness) — the only remaining substantive coverage gap; libFuzzer for `framing.cpp` + Python header/frame fuzz.
2. **M9** (zero-value enum renumber) — **FIXED (P11-03, 2026-08-13)** via the protocol v1.5 bump: `ACTION_UNKNOWN = 0` added, `COMMAND_UNKNOWN` moved to 0, OK/ERROR shifted. The interim lint (T-16) remains as a construction-site guard.
