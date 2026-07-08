# Veyron ROADMAP — Phase 6

**Baseline:** 2026-07-06 · Kernel `0.1.0` · Audit ~78/100 (see `AUDIT.md`)
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md`, all items complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-07-06

| Metric | Value |
|--------|-------|
| Kernel version | 0.1.0 |
| Audit score | ~78/100 (`AUDIT.md`, 2026-07-02) |
| Tests | `cargo test --all --all-features`: 266 passing, 0 failing |
| Clippy | clean (`--all-targets --all-features -D warnings`) |
| Phase 5 | ✅ all items complete — see `docs/archive/ROADMAP_v5.md` |

---

## Phase 6 — Network Plugin Protocol Support (candidate, not yet scheduled)

Source: `veyron-plugins/plugins/network/KERNEL_PROTOCOL_TODO.md` (gitignored local notes in that repo). All four items require changes here (proto and/or kernel), not in `veyron-plugins`.

### R6-01 — Plugin → event-bus publish path

`EventBus::publish` (`src/events/bus.rs`) is only called from kernel-internal code (`src/ipc/protocol.rs`, `src/kernel/orchestrator.rs`, `src/plugins/supervisor.rs`). No wire message lets a plugin push an event in. Needed for `network` to emit `network.request_completed` (status, host, latency_ms, retry_count) instead of stdout-only logging.

**Needed:** new `EventPublish` envelope variant (or `Event` with a plugin→kernel direction), handled in `src/ipc/connection.rs`/`src/ipc/protocol.rs` next to `Subscribe`/`Unsubscribe`. Gate behind a new permission (e.g. `PERMISSION_EVENT_PUBLISH`) so a plugin can't spoof `system.*` events.

### R6-02 — Streaming action support (chunked request/response)

`ActionRequest`/`ActionResponse` are single envelopes (`bytes params_json`/`data_json`) — no framing for a large body across multiple frames tied to one `action_id`. `send_fragmented` is client-side reassembly for one logical frame, not a multi-message stream. Needed for a real `http_request_stream` action.

**Options:** (a) new `ActionStreamChunk` message (`action_id`, `seq`, `bytes`, `final: bool`) routed by the kernel to the same requester across frames, or (b) let actions open a raw IPC channel (generalize `send_raw_audio`'s `FLAG_RAW_BINARY` path beyond audio) both sides drive manually.

### R6-03 — Per-caller resource/rate limits at the kernel level

`max_procs`/`max_vmem_mb` exist per-plugin in `config.yaml` (R5-10), but nothing limits one *calling* plugin from starving others via actions on a shared provider (e.g. `network` as the standard network path for all plugins).

**Needed:** kernel-enforced per-action-caller quotas, or a documented convention that the provider tracks caller ids from `ActionRequest` itself. **Open question to resolve first:** confirm whether `ActionRequest` carries a caller/requester id field today (proto + `src/ipc/protocol.rs` routing) — R5-07's action routing may already thread `requester_id` through `PendingAction`, worth checking before scoping new proto work.

### R6-04 — WebSocket / long-lived-connection action model

Single `ActionRequest`→`ActionResponse` doesn't fit a persistent WebSocket-style session. Needed for any provider (e.g. `network`) that wants to expose a WS-like connection to callers.

**Options:** dedicated `Event`-based push model (open via action, kernel delivers frames as `Event`s to subscribers) or a new bidirectional-stream primitive. Largest of the four — needs its own design doc in this repo (mirrors `docs/superpowers/specs/2026-07-02-action-routing-design.md`'s process for R5-07) before any `network` work depends on it.

---

## Audit Remediation — from `AUDIT.md` (2026-07-07)

Source: full-codebase audit, three parallel passes (kernel/IPC/events/api, auth/plugin-lifecycle/marketplace, cross-SDK/protocol). See `AUDIT.md` for full detail per item.

### Critical

**T-01 — HTTP admin API missing authorization check (start/stop/restart)** ✅ done
`src/api/middleware.rs:19-43`, `src/api/routes.rs:87-124`. `auth_middleware` checks JWT validity only, never `claims.permissions`. Any valid JWT can stop/start/restart any plugin via HTTP, no `PERMISSION_KERNEL_ADMIN` gate (unlike the equivalent IPC path). Fix: add permission check mirroring `check_permission`, gate on `PERMISSION_KERNEL_ADMIN`.
Fixed: `require_kernel_admin` middleware layer on start/stop/restart routes, checks JWT `claims.permissions` for `PERMISSION_KERNEL_ADMIN`, mirrors IPC `KernelCommand` gate (`src/ipc/protocol.rs:536`). Tests: `tests/unit/test_api.rs` (`admin_route_rejects_valid_token_lacking_kernel_admin_permission`, `admin_route_allows_token_with_kernel_admin_permission`).

**T-02 — C++ SDK has no real integration test coverage** ✅ done
`tests/integration/test_sdk_cpp.rs:1-91`. "C++ SDK" test spawns the Rust `echo_plugin_rs` binary, not C++. Zero end-to-end verification of `sdk/cpp/src/framing.cpp` against a live kernel. Fix: build real C++ echo-plugin binary via CMake in CI, point test at it.
Fixed: CI (`.github/workflows/ci.yml`) now installs C++ SDK build deps and builds `sdk/cpp/examples/echo_plugin.cpp` (real C++, links `sdk/cpp/src/{client,framing,mac,env}.cpp`) via CMake before `cargo test`. `test_sdk_cpp.rs` spawns that binary and drives a real kernel-brokered action round trip. Uncovered and fixed two real gaps blocking this: `sdk/cpp/src/env.cpp`'s `default_socket_path()` never read `VEYRON_SOCKET_PATH` despite docs claiming it did; C++ SDK's `register_plugin`/`Plugin` had no way to declare a `PluginManifest` at all (no permissions/actions/ipc_targets ever sent), so `find_action_provider` and IPC-send permission checks were unreachable from C++ plugins — added a `Plugin::manifest()` override hook and a manifest-accepting `register_plugin` overload.

### High

**T-03 — Single-threaded IPC router stalls kernel-wide on one slow plugin**
`src/ipc/protocol.rs:648-654` (`forward`), `:725-734` (`broadcast`), `src/events/bus.rs:127-153` (`deliver`). All fan-out sends `.await` a 50ms timeout inline on the shared router task; `broadcast` loops all plugins = `O(n)*50ms`. One non-draining plugin stalls routing for everyone. Fix: spawn per-target send tasks or use `try_send` + bounded retry queue instead of blocking the router loop. Needs design thought — own workstream.

**T-04 — `config.yaml` permissions not bound to runtime JWT claims** ✅ done
`src/utils/config.rs:30-34`, `src/plugins/loader.rs:225-259`, `src/ipc/protocol.rs:245-278`, `src/auth/permissions.rs:17-60`. Operator's `config.yaml permissions:` list only checked at boot (`validate_plugin_def`); runtime enforcement trusts JWT `claims.permissions` verbatim, no link back. A plugin scoped to `network` in config.yaml can still get `kernel_admin` via its JWT. Fix: mint JWT/capability token from config.yaml list, or re-validate claims against `PluginDef.permissions` at registration.
Fixed: `Kernel::run_with_components` (`src/kernel/orchestrator.rs`) builds a `plugin_id → config.yaml permissions` map and threads it into `MessageRouter::run_with_context` as `config_permissions`. At `PluginRegister` handling (`src/ipc/protocol.rs`), after JWT claims are applied to the manifest, permissions are clamped to the operator's allowlist for that plugin id — same "empty/absent list = unrestricted" convention as `validate_plugin_def`. Plugin ids not declared in config.yaml are left unclamped (back-compat for dynamically-registered/test plugins). Tests: `tests/unit/test_router.rs` (`registration_clamps_jwt_permissions_to_config_allowlist`, `registration_leaves_permissions_unclamped_for_plugin_not_in_config`).

**T-05 — `POST /plugins/:id/start` bypasses `validate_plugin_def`** ✅ done
`src/api/routes.rs:87-103`, `src/plugins/loader.rs:41-148`. HTTP start path skips kernel-compat/permission cross-check that boot-time `load_all` enforces. A plugin rejected at boot can still be started later via HTTP. Fix: call `validate_plugin_def` inside `start_plugin`, 422/403 on failure.
Fixed: `start_plugin` now calls `validate_plugin_def(def)` before spawning — `VeyronError::PermissionDenied` → 403, any other validation failure (kernel incompatibility, malformed manifest) → 422, matching boot-time `load_all` enforcement. Test: `tests/unit/test_api.rs` (`start_plugin_rejects_manifest_requesting_ungranted_permission`).

**T-06 — C++/Python SDKs never send `EventAck`, events silently dropped** ✅ done
`sdk/cpp/include/veyron/plugin.hpp:38-74`, `sdk/cpp/include/veyron/client.hpp`/`client.cpp` (no `ack_event`), `sdk/python/veyron/plugin.py:67-83`, `sdk/python/veyron/client.py`. Only Rust SDK auto-acks (`sdk/rust/src/plugin.rs:134-143`). Kernel marks un-acked events dead after `max_retries` — every event to a stock C++/Python plugin is retried then dropped. Fix: add `on_event`/auto-ack + `ack_event()` to both SDKs.
Fixed: added `VeyronClient::ack_event()`/`ack_event()` and `Plugin::on_event()` to both SDKs; the run loop now dispatches `Event` envelopes to `on_event` (instead of falling through to `on_message`) and sends `EventAck` on success — a throwing/raising handler skips the ack so the kernel retries, mirroring the Rust SDK. Tests: `sdk/cpp/tests/test_plugin_ping.cpp` (`PluginRunLoop.DispatchesEventToOnEventAndSendsAck`), `tests/python/test_plugin_ping.py` (`test_events_dispatched_to_on_event_and_acked_not_on_message`).

**T-07 — Rust SDK swallows `on_message` handler errors** ✅ done
`sdk/rust/src/plugin.rs:144-157`. `Err(_) => break` discards error, `run()`/`serve()` returns `Ok(())` even after handler failure — inconsistent with C++/Python which propagate. Fix: propagate error out of `serve()` after `on_shutdown()`.
Fixed: `serve()` now captures the `on_message` handler's `Err`, still runs `on_shutdown()`, then returns the captured error (`Ok(())` only when the loop exited cleanly). Test: `sdk/rust/tests/protocol.rs` (`plugin_serve_propagates_on_message_handler_error`).

### Medium

**T-08 — Error-count budget resettable by the offending connection itself** ✅ done
`src/ipc/protocol.rs:190-196`. Map prune at `max_tracked_error_conns` keyed on registration status only, not staleness — lets an unregistered abusive connection's counter reset to 1 repeatedly. Fix: prune by idle/LRU or last-error timestamp.
Fixed: `error_counts` now keyed `conn_id -> (count, last_error_at)`; the size-triggered prune (`src/ipc/protocol.rs`) evicts entries idle past `ERROR_BUDGET_IDLE_TTL` (300s) instead of filtering on `registry.is_registered`, so an unregistered connection can no longer keep evicting its own entry back to zero by staying unregistered. Test: `tests/unit/test_router.rs` (`unregistered_connection_error_budget_survives_map_prune`).

**T-09 — WebSocket gateway has no concurrent connection cap** ✅ done
`src/api/websocket.rs:47`, vs. `src/ipc/server.rs:80-87` (UDS has `max_connections`). Fix: add `max_ws_connections` config, enforce pre-upgrade.
Fixed: new `Config::max_ws_connections` (default 1024, `src/utils/config.rs`), threaded through `ApiServer`/`create_router_full` into `WsGateway` (`src/api/websocket.rs`). `ws_handler` reserves a slot via `open_conns.fetch_add` before calling `.on_upgrade`, backing out and returning 503 if it crossed the cap — same fetch-then-correct pattern as the IPC rate limiter, avoiding a check-then-increment race. Slot is released when `handle_socket` returns. Test: `tests/integration/test_websocket.rs` (`ws_upgrade_rejected_once_connection_cap_reached`).

**T-10 — `get_plugin_logs` `lines` param unclamped** ✅ done
`src/api/routes.rs:126-141`. Bounded only incidentally by ring buffer size. Fix: explicit `min(n, MAX_LOG_LINES)`.
Fixed: `src/api/routes.rs` clamps `?lines=` to a new `MAX_LOG_LINES` (10,000) constant via `.min()`, independent of the supervisor's own ring-buffer capacity. Test: `tests/unit/test_api.rs` (`logs_endpoint_clamps_huge_lines_param_instead_of_erroring`).

**T-11 — Marketplace has no signature independent of registry.json's own hash** ✅ done
`src/marketplace/installer.rs:123-131`, `src/marketplace/registry.rs:10-11`. sha256 check proves nothing about publisher trust if the serving channel is compromised. Fix: maintainer-signed manifest, pinned public key.
Fixed: new `RegistryEntry.signature` field (Ed25519, hex, 64 bytes) over `"{slug}:{version}:{sha256}"`; `verify_entry_signature` (`src/marketplace/registry.rs`) checks it against a compile-time-pinned `MAINTAINER_PUBLIC_KEY_HEX`, called from `install()` (`src/marketplace/installer.rs`) as a new Step 4b right after the sha256 check — independent trust root, since the signing key never touches the registry-serving infrastructure the sha256 already trusts. New `Config::marketplace_public_key` (`config.yaml`) lets private registries override the pinned key. Docs: `docs/PLUGIN_REGISTRY_SCHEMA.md` `signature` field. Tests: `src/marketplace/registry.rs` (`signature_verifies_with_matching_key_and_message`, `signature_rejected_when_sha256_tampered`, `signature_rejected_when_empty`, `signature_rejected_with_wrong_public_key`).

**T-12 — JWT secret has no minimum-strength check** ✅ done
`src/auth/jwt.rs:19-27`. Any non-empty secret accepted for HS256. Fix: reject/warn under 32 bytes at construction (`src/kernel/orchestrator.rs:116-131`).
Fixed: `Kernel::run_with_components` (`src/kernel/orchestrator.rs`) now checks `config.jwt_secret` length against `MIN_JWT_SECRET_BYTES` (32) before constructing the `JwtValidator`, `anyhow::bail!`s with the byte count on a too-short secret — same startup-refusal pattern as the existing `allow_no_auth` check. `JwtValidator::new` itself is unchanged (kept accepting any secret, since unit tests construct it directly with short fixed test secrets). Integration tests that spin up a secured kernel (`test_mac.rs`, `test_websocket.rs`, `test_metrics_counters.rs`) had their literal secrets lengthened to clear the new minimum. Tests: `tests/unit/test_kernel.rs` (`kernel_refuses_weak_jwt_secret`, `kernel_accepts_jwt_secret_at_minimum_length`).

**T-13 — No C++ unit tests for frame parsing against malformed input** ✅ done
`sdk/cpp/tests/` (no `test_framing.cpp`). Fix: adversarial-input tests for `read_frame_full`/`pack_frame_mac`.
Fixed: new `sdk/cpp/tests/test_framing.cpp` (registered in `sdk/cpp/CMakeLists.txt`) — bad magic, oversized length field (rejected before payload read), CRC mismatch, truncated header/payload/MAC tag, garbage `FLAG_COMPRESSED` payload, missing MAC on a secured connection, and empty-payload happy path. 9 tests, all pass alongside the existing 27.

**T-14 — No fuzz coverage for C++/Python framing/decompression** ✅ done (C++ half)
`fuzz/fuzz_targets/*` covers Rust `wire` crate only. Fix: libFuzzer harness for `sdk/cpp/src/framing.cpp`.
Fixed (C++): new `sdk/cpp/fuzz/fuzz_framing.cpp`, built via `-DVEYRON_BUILD_FUZZERS=ON` (Clang required for libFuzzer; off by default, doesn't affect normal GCC/Clang builds). Stages fuzz input through a memfd rather than a pipe — a pipe's ~64KiB buffer would deadlock the harness on oversized input before `read_frame_full`'s own length-field check gets a chance to reject it. Exercises both the no-key and MAC-verifying read paths (covers `FLAG_COMPRESSED`/`FLAG_MAC_PRESENT` handling). Smoke-tested 339k execs/20s, no crashes. Python framing (`sdk/python/veyron/framing.py`) fuzz coverage still open — no equivalent harness yet (e.g. via `atheris`).

**T-15 — `FRAME_READ_TIMEOUT` slow-loris protection missing in C++/Python** ✅ done
`wire/src/framing.rs:66,179-196` (Rust only) vs. `sdk/cpp/src/framing.cpp:110-120`, `sdk/python/veyron/framing.py:127-150` (plain blocking reads, no timeout). Fix: wrap payload/MAC read in per-frame timeout in both SDKs.
Fixed (C++): `recv_exact_deadline` (`sdk/cpp/src/framing.cpp`, poll-based) bounds everything after the first header byte to a deadline; new `read_frame_full_with_timeout(fd, session_key, frame_timeout_ms)` (default 10s via `read_frame_full`, mirrors `wire/src/framing.rs`'s `read_frame`/`read_frame_with_timeout` split) lets tests use a short timeout instead of waiting 10s. Idle connections between frames still block indefinitely — only mid-frame stalls are bounded. Tests: `sdk/cpp/tests/test_framing_timeout.cpp` (`IdleConnectionBetweenFramesDoesNotTimeOut`, `StalledMidFrameEventuallyDisconnectsRatherThanHanging`).
Fixed (Python): `async_read_frame` (`sdk/python/veyron/framing.py`) reads the first byte with no timeout, then wraps the rest in `asyncio.wait_for(..., timeout=frame_timeout)` (default `FRAME_READ_TIMEOUT = 10.0`), raising `ValueError("veyron: frame read timed out")`.
Also restored (unrelated pre-existing regression, blocking test verification): synchronous `read_frame(stream, session_key=None) -> bytes` was missing from `sdk/python/veyron/framing.py` entirely — both it and its two test files were on the lost-work list in `RECOVERY_NEEDED.md` from the 2026-07-03 subagent `git reset --hard` incident and never got re-authored. Restored (no timeout — operates on already-buffered `io.BytesIO`, not a live socket) and exported from `sdk/python/veyron/__init__.py`. One further pre-existing bug found but left alone as out of scope: `tests/python/test_framing_mac.py::test_async_read_frame_mac_verifies` expects `async_read_frame` to return `payload` directly, but it returns `(flags, payload)` — mismatch predates this session (confirmed via `git show HEAD:veyron/framing.py` in the `sdk/python` submodule).

**T-16 — `ActionStatus`/`CommandStatus` proto enums default to OK (zero-value footgun)**
`proto/veyron_protocol.proto:138-144,165-170`. `ACTION_OK = 0`/`COMMAND_OK = 0` unlike every other status enum's `*_UNKNOWN = 0` convention — missed `set_status()` silently reads as success. Fix: wire-breaking renumber, defer to next protocol version bump; add lint/test in the meantime asserting explicit status at every construction site.

**T-17 — Proto file hand-copied to three locations, no CI drift check** ✅ done
`wire/proto/`, `sdk/cpp/proto/`, `sdk/python/proto/`. Fix: CI diff/checksum check, or generate SDK copies from single source.
Fixed: new `.github/workflows/ci.yml` step ("Proto drift check (T-17)") runs first, before toolchain setup — `diff -u` of `wire/proto/veyron_protocol.proto` against both SDK copies, fails the job on any divergence. No behavior change to the three files themselves, all identical at time of fix.

**T-18 — C++ SDK has no fragmentation support**
`sdk/cpp/src/framing.cpp`/`client.cpp` vs. Rust (`sdk/rust/src/client.rs:355-420`) and Python (`sdk/python/veyron/client.py:164-197`). Fragmented frames silently mis-parsed by C++. Fix: port fragmentation, or make `read_frame_full` explicitly reject `FLAG_FRAGMENTED`.

### Low

**T-19 — Action permission model checks provider only, not requester**
`src/auth/permissions.rs:10-15`, `src/ipc/protocol.rs:413-430`. Unprivileged plugin can transitively trigger e.g. network requests via any provider with `PERMISSION_NETWORK`. May be intentional (provider-declares-authorization model) — needs explicit design decision/doc note, not silent assumption.

**T-20 — `strncpy` silently truncates long socket paths in C++ client**
`sdk/cpp/src/client.cpp:26-28`. Not an overflow, but truncation is silent rather than rejected. Fix: explicit length check, throw if too long.

---

## Task Summary

| Phase | Items | Severity | Est. effort |
|-------|-------|----------|--------------|
| 6 Network plugin protocol support | R6-01..04 | Candidate, unscheduled | ~1 decision + 1 design doc + impl TBD |
| Audit remediation | T-01..20 | 2 Critical, 5 High, 11 Medium, 2 Low | T-01/T-05 fix together; T-03 needs own design; rest are independent; T-01,T-02,T-04..T-15,T-17 ✅ done |

**Ship gate:** none set yet — R6-03's open question and R6-04's design doc should resolve before effort estimates firm up. T-01/T-05 (HTTP authz/validation bypass) ✅ landed — was the live privilege-escalation path on any deployment exposing the REST API. T-02 (C++ SDK integration coverage), T-04 (config.yaml/JWT permission binding), T-06 (C++/Python `EventAck`), T-07 (Rust SDK error propagation), T-08 (error-budget prune staleness), T-09 (WS connection cap), T-10 (`get_plugin_logs` lines clamp), T-11 (marketplace maintainer signature), T-12 (JWT secret min-strength check), T-13 (C++ malformed-frame unit tests), T-14 (C++ fuzz harness, C++ half), T-15 (C++/Python slow-loris timeout), and T-17 (proto drift CI check) ✅ landed. Remaining open: T-14 Python-half (atheris), T-16 (deferred to next protocol version bump), T-18, T-19 (needs design decision), T-20.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- Protocol changes: `proto/veyron_protocol.proto` updated with `reserved` discipline, all three SDKs updated in the same change.
- Docs updated in the same PR (`docs/FRAMING.md` for wire changes, README for operator-visible changes).
