# Veyron Codebase Audit

Date: 2026-07-07
Scope: full repo — kernel/IPC/events/api (`src/kernel`, `src/ipc`, `src/events`, `src/api`, `src/utils`), auth/plugin-lifecycle/marketplace (`src/auth`, `src/plugins`, `src/cli`, `src/marketplace`), and cross-SDK/protocol (`sdk/rust`, `sdk/cpp`, `sdk/python`, `proto/`, `wire/`, `tests/`, `fuzz/`).

Method: three parallel read-only code-audit passes. Findings below are deduplicated and grouped by severity. Each entry has file:line, concrete failure scenario, and fix direction.

---

## Critical

### C1. HTTP admin API has no authorization check — any valid JWT can start/stop/restart any plugin
- **Files:** `src/api/middleware.rs:19-43` (`auth_middleware`), `src/api/routes.rs:87-124` (`start_plugin`/`stop_plugin`/`restart_plugin`), `src/api/server.rs:71-78`
- **Issue:** `auth_middleware` validates JWT signature/expiry only — it never inspects `claims.permissions`. The lifecycle routes call `state.manager.start/stop/restart` with zero permission check. Compare to the IPC path (`src/ipc/protocol.rs:529-549`), which correctly requires `PermissionType::PermissionKernelAdmin` before executing an equivalent `KernelCommand`. This is the same class of gap the recent "gate kernel action routing on provider permission" commit (`2ff1a22`) fixed on IPC but missed on HTTP.
- **Impact:** any plugin holding *any* valid JWT (even one scoped to `network` only) can stop/start/restart every other supervised plugin, including privileged ones.
- **Fix:** require `PERMISSION_KERNEL_ADMIN` in `claims.permissions` before executing lifecycle routes — add as middleware layer or inline check, mirroring `check_permission`.

### C2. C++ SDK has zero real integration test coverage — tests spawn the Rust reference plugin, not C++
- **File:** `tests/integration/test_sdk_cpp.rs:1-91`
- **Issue:** the sole "C++ SDK" test spawns `target/debug/echo_plugin_rs` (Rust binary). No C++ binary is ever built/spawned/tested end-to-end. Combined with M6/M7 below (no C++ unit tests or fuzz coverage for framing), the SDK with manual memory management (`sdk/cpp/src/framing.cpp`) has no verification of its wire-parsing path against a live kernel at all.
- **Fix:** wire a real C++ echo-plugin binary into CI (CMake build) and point this test at it.

---

## High

### H1. Single-threaded router serializes all IPC fan-out behind per-message blocking timeouts — one slow plugin stalls the whole kernel
- **Files:** `src/ipc/protocol.rs:648-654` (`forward`), `src/ipc/protocol.rs:725-734` (`broadcast`), `src/events/bus.rs:127-153` (`EventBus::deliver`)
- **Issue:** `MessageRouter::run_with_context` is the sole consumer of the inbound message channel — every IPC message (unicast, broadcast, kernel commands, event-bus publishes) is processed serially in one task. `forward`/`broadcast`/`deliver` each `.await tokio::time::timeout(50ms, write_tx.send(...))` inline in this loop; `broadcast` loops over *every* registered plugin, so cost is `O(n) * up to 50ms`.
- **Impact:** a plugin that stops draining its own write channel (slow/malicious/compromised) causes every subsequent forward/broadcast/event targeting it to cost up to 50ms *on the shared router task*, stalling routing for all other plugins. Trivially triggered by any plugin with IPC-send permission or wildcard event subscription.
- **Fix:** move the bounded send off the shared router task — spawn per-target send tasks, or use `try_send` + bounded per-connection retry queue instead of blocking the router loop.

### H2. `config.yaml` permissions are inert at runtime — two disconnected permission surfaces
- **Files:** `src/utils/config.rs:30-34`, `src/plugins/loader.rs:225-259` (`validate_plugin_def`), `src/ipc/protocol.rs:245-278`, `src/auth/permissions.rs:17-60`
- **Issue:** `config.yaml`'s `permissions:` list is cross-checked against `plugin.json` only at load time (`validate_plugin_def`). Runtime enforcement (`check_permission`) uses `PluginRegistry.manifest.permissions`, populated straight from the JWT's `claims.permissions` (or self-declared manifest if JWT auth is off) at registration time — with no link back to the operator-configured `config.yaml` list.
- **Impact:** an operator can restrict a plugin to `["network"]` in `config.yaml`, but the JWT actually issued to that plugin process can carry broader claims (e.g. `PERMISSION_KERNEL_ADMIN`), and the kernel enforces those claims verbatim. `docs/PLUGIN_REGISTRY_SCHEMA.md` implies config.yaml is the enforcement boundary; it isn't.
- **Fix:** either have the kernel mint the JWT/capability token from `config.yaml`'s list itself, or re-validate `claims.permissions` against `PluginDef.permissions` at registration and reject/narrow anything not operator-granted.

### H3. `POST /plugins/:id/start` bypasses `validate_plugin_def` — no kernel-compat or permission cross-check on HTTP-triggered start
- **Files:** `src/api/routes.rs:87-103`, `src/plugins/loader.rs:41-148`
- **Issue:** `start_plugin` builds the spawn config via `PluginLoader::config_from_def` and calls `manager.start` directly, skipping `validate_plugin_def` (kernel-version compat check, unknown-permission rejection, config-cross-check) — that function is only called from boot-time `load_all`.
- **Impact:** a plugin rejected at boot (incompatible version, ungranted permission) can still be started later via HTTP with no re-validation, as long as it's listed in `config.yaml`.
- **Fix:** call `validate_plugin_def` inside `start_plugin` before spawning; return 422/403 on failure.

### H4. C++ and Python SDKs never send `EventAck` — every event to a stock plugin is silently dropped after retry budget
- **Files:** `sdk/cpp/include/veyron/plugin.hpp:38-74`, `sdk/cpp/include/veyron/client.hpp`/`client.cpp` (no `ack_event`), `sdk/python/veyron/plugin.py:67-83`, `sdk/python/veyron/client.py`, vs. `sdk/rust/src/plugin.rs:134-143` (auto-acks). Kernel side: `src/events/store.rs:120-128`.
- **Issue:** Rust auto-calls `client.ack_event()` on successful `on_event`. C++ has no dedicated event path or `ack_event()` method at all — events fall through to generic `on_message`. Python similarly routes `Event` to `on_message` with no ack helper. Kernel marks un-acked events `dead` after `max_retries`.
- **Impact:** every event delivered to a stock C++/Python plugin is retried then permanently dropped unless the author hand-builds an `EventAck` Envelope — undocumented, unsupported by either SDK's public API.
- **Fix:** add `on_event`/auto-ack machinery + `ack_event()` client method to C++ and Python SDKs, mirroring Rust.

### H5. Rust SDK swallows `on_message` handler errors — `run()` reports success even after a fatal failure
- **File:** `sdk/rust/src/plugin.rs:144-157`
- **Issue:** `Err(_) => break` discards the error; function falls through to `on_shutdown().await` as the return value, so `run()`/`serve()` returns `Ok(())` even when the handler failed. Inconsistent with C++ (exception propagates through `catch(...)` → `on_shutdown()` → rethrow) and Python (exception propagates via `finally`).
- **Impact:** a supervisor/process-exit-code check built against the Rust SDK won't observe handler failures the other two SDKs surface loudly.
- **Fix:** propagate the error out of `serve()` (call `on_shutdown().await?` then `return Err(e)`).

---

## Medium

### M1. Error-count budget can be reset for the exact connection accruing errors
- **File:** `src/ipc/protocol.rs:190-196`
- **Issue:** when tracked-error map hits `max_tracked_error_conns` (8192), `retain` prunes by current registration status only, not staleness. An unregistered connection actively generating an error burst gets wiped along with the rest, restarting its counter at 1.
- **Impact:** a connection that keeps the map at/above threshold can bypass the per-connection error throttle indefinitely.
- **Fix:** prune by idle/LRU or last-error timestamp, not registration state.

### M2. WebSocket gateway has no cap on concurrent connections
- **File:** `src/api/websocket.rs` `ws_handler` (line 47), vs. `src/ipc/server.rs:80-87` (UDS has explicit `max_connections`)
- **Issue:** no equivalent ceiling for WS upgrades; each accepted socket spawns its own task+channel. WS messages funnel into the same single-threaded router (H1), so unbounded WS connections widen that attack surface.
- **Fix:** add `max_ws_connections` config, enforce before upgrade.

### M3. `get_plugin_logs` `lines` query param has no explicit upper bound
- **File:** `src/api/routes.rs:126-141`
- **Issue:** `q.lines.unwrap_or(100)` unclamped before passing to `manager.logs`. Currently bounded incidentally by the log ring buffer (`log_buffer_lines`, default 1000) — not an intentional API-boundary validation.
- **Fix:** explicit `min(n, MAX_LOG_LINES)` at the route.

### M4. Marketplace trust model has no independent signature over registry/archives
- **File:** `src/marketplace/installer.rs:123-131`, `src/marketplace/registry.rs:10-11`
- **Issue:** archive integrity is checked against the `sha256` listed in `registry.json` itself, fetched from a single hardcoded (or operator-supplied) URL. No detached signature independent of that channel.
- **Impact:** compromise of the registry-serving channel lets an attacker publish a malicious archive with a self-consistent hash.
- **Fix:** add maintainer-signed manifest (detached sig verified against a pinned public key) before trusting `sha256`/`archive_url`.

### M5. JWT secret has no minimum-strength enforcement
- **File:** `src/auth/jwt.rs:19-27`
- **Issue:** `JwtValidator::new` accepts any non-empty secret for HS256 with no length/entropy check. Algorithm is correctly hardcoded (no alg-confusion), but a weak configured secret is brute-forceable offline.
- **Fix:** reject/warn on `jwt_secret` shorter than 32 bytes where the validator is constructed (`src/kernel/orchestrator.rs:116-131`).

### M6. No unit test coverage of C++ frame parsing against malformed input
- **File:** `sdk/cpp/tests/` (no `test_framing.cpp`)
- **Issue:** nothing exercises `read_frame_full` (`sdk/cpp/src/framing.cpp:156-218`) against bad magic, truncated header/payload, CRC mismatch, oversized length, corrupt zstd frames, or MAC failure — exactly the highest memory-risk code path (manual `memcpy`/buffer sizing).
- **Fix:** add adversarial-input unit tests for `read_frame_full`/`pack_frame_mac`.

### M7. No fuzz coverage for C++/Python framing/decompression
- **File:** `fuzz/fuzz_targets/*` (Rust `wire` crate only)
- **Issue:** C++ (`sdk/cpp/src/framing.cpp`) and Python (`sdk/python/veyron/framing.py`) reimplement CRC32/header packing/bounded-decompress with no fuzz harness, despite C++ doing manual buffer arithmetic.
- **Fix:** add a libFuzzer harness for `framing.cpp`.

### M8. `FRAME_READ_TIMEOUT` slow-loris protection missing in C++/Python SDKs
- **Files:** `wire/src/framing.rs:66,179-196` (10s timeout, Rust only) vs. `sdk/cpp/src/framing.cpp:110-120` (`recv_exact`, plain blocking read) and `sdk/python/veyron/framing.py:127-150` (plain `readexactly`)
- **Impact:** a peer that sends a valid header declaring a large payload then stalls hangs the C++/Python receive loop indefinitely (per-connection DoS).
- **Fix:** wrap payload/MAC read phase in a per-frame timeout in both SDKs.

### M9. `ActionStatus`/`CommandStatus` proto enums default to OK (zero-value footgun)
- **File:** `proto/veyron_protocol.proto:138-144,165-170`
- **Issue:** `ACTION_OK = 0`, `COMMAND_OK = 0` — unlike every other status enum in the file (`*_UNKNOWN = 0` pattern). A missed `set_status()` call anywhere silently reports success.
- **Fix:** wire-breaking to renumber now; at minimum add a lint/test asserting every construction site sets `status` explicitly. Track for next protocol version bump.

### M10. Proto file manually copied to three locations with no CI drift check
- **Files:** `wire/proto/`, `sdk/cpp/proto/`, `sdk/python/proto/` (currently identical, but `sdk/cpp/CMakeLists.txt:27` says "re-copy by hand")
- **Fix:** add CI diff/checksum check across the three copies, or generate SDK copies from one source at build time.

### M11. C++ SDK has no fragmentation support — silently mis-parses fragmented frames
- **Files:** `sdk/cpp/src/framing.cpp`/`client.cpp` (no `send_fragmented`/reassembly) vs. `sdk/rust/src/client.rs:355-420` and `sdk/python/veyron/client.py:164-197` (both implement bounded reassembly)
- **Impact:** a C++ plugin receiving a fragmented frame hands raw bytes straight to `Envelope::ParseFromArray`, which fails or silently misparses.
- **Fix:** port fragmentation to C++, or make `read_frame_full` explicitly reject `FLAG_FRAGMENTED` with a clear error.

---

## Low

### L1. `required_permission_for_action` checks the action *provider's* permission, not the requester's
- **Files:** `src/auth/permissions.rs:10-15`, `src/ipc/protocol.rs:413-430`
- **Issue:** for a routed `ActionRequest`, only the provider plugin's permission (e.g. `PERMISSION_NETWORK`) is checked — the requesting plugin needs no permission at all. An unprivileged plugin can transitively trigger network requests via any network-capable provider.
- **Note:** may be intentional design (provider-declares-authorization model) but should be explicitly documented given it's easy to misread the recent permission-gating work as requester-side.

### L2. `strncpy` silently truncates long socket paths in C++ client
- **File:** `sdk/cpp/src/client.cpp:26-28`
- **Issue:** not a buffer overflow (bounded copy into zero-initialized struct), but a path longer than `sizeof(sun_path)-1` (~107 bytes) is silently truncated rather than rejected, risking connect-to-wrong-path.
- **Fix:** check length explicitly and throw if too long.

---

## Informational / Follow-ups

- **Framing code relocated to `wire/` crate:** `src/ipc/framing.rs` is now a re-export shim; actual length-prefix/allocation logic lives in `wire/src/framing.rs`, which was out of scope for the kernel-focused pass but was covered by the SDK/proto pass (see H4, M6-M11) — no gap, just noting the split (`.worktrees/wire-crate-split` present in repo).
- Extensive prior hardening confirmed sound on re-check: UDS socket perms/TOCTOU, fragment reassembly bounds, frame MAC ordering, action-response provider-spoofing prevention, rate-limiter key pruning, zip-slip protection in marketplace installer, resource limits applied regardless of `sandbox:true`, supervisor restart backoff, JWT expiry/algorithm hardcoding, constant-time MAC comparison across all three SDKs, no pickle/eval/shell-subprocess in Python SDK, disciplined `reserved` field usage in proto. No regressions found in these areas — do not re-litigate without new evidence.

---

## Summary by Severity

| Severity | Count |
|----------|-------|
| Critical | 2 |
| High     | 5 |
| Medium   | 11 |
| Low      | 2 |

## Priority Recommendation

1. **C1** (HTTP admin authz bypass) — fix immediately, it's a live privilege-escalation path on any deployment exposing the REST API.
2. **H2/H3** (config.yaml permission enforcement gap, HTTP start bypassing validation) — same family as C1, fix together.
3. **H1** (router fan-out DoS) — architectural, needs design thought before patching; track as its own workstream.
4. **H4/H5** (SDK event-ack, error propagation) — cross-SDK consistency bugs, needed before recommending C++/Python SDKs for production plugins.
5. **C2 + M6/M7** (C++ test/fuzz gaps) — close before trusting C++ SDK in adversarial/multi-tenant deployments.
