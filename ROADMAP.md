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

### R6-01 — Plugin → event-bus publish path ✅ done

`EventBus::publish` (`src/events/bus.rs`) is only called from kernel-internal code (`src/ipc/protocol.rs`, `src/kernel/orchestrator.rs`, `src/plugins/supervisor.rs`). No wire message lets a plugin push an event in. Needed for `network` to emit `network.request_completed` (status, host, latency_ms, retry_count) instead of stdout-only logging.

**Needed:** new `EventPublish` envelope variant (or `Event` with a plugin→kernel direction), handled in `src/ipc/connection.rs`/`src/ipc/protocol.rs` next to `Subscribe`/`Unsubscribe`. Gate behind a new permission (e.g. `PERMISSION_EVENT_PUBLISH`) so a plugin can't spoof `system.*` events.

Fixed: new `EventPublish`/`EventPublishAck` wire messages and `EventPublishStatus` enum (`EVENT_PUBLISH_UNKNOWN=0, EVENT_PUBLISH_OK=1, EVENT_PUBLISH_ERROR=2, EVENT_PUBLISH_PERMISSION_DENY=3`) with new `PERMISSION_EVENT_PUBLISH` permission (value 13) added to `proto/veyron_protocol.proto` and mirrored to `sdk/cpp/proto/` and `sdk/python/proto/`. New `EventPublish` match arm in `src/ipc/protocol.rs` (after `Unsubscribe`) checks permission via `check_permission`, then structures the plugin-supplied `event_type` as `plugin.<sender_id>.<event_type>` with zero hardcoded business logic per the manifesto. New `EVENT_PUBLISH_SEQ: AtomicU64` kernel-assigned event id counter alongside `MSG_SEQ`/`ACTION_CORRELATION_SEQ`. Rust SDK gains `VeyronClient::publish_event()` (`sdk/rust/src/client.rs`) mirroring `send_action`'s pattern. Design doc: `docs/superpowers/specs/2026-07-06-plugin-event-publish-design.md` (revised 2026-07-08). Tests: `publish_without_permission_is_denied`, `publish_with_permission_namespaces_and_delivers_to_subscriber`, `two_plugins_publishing_same_event_type_land_on_distinct_namespaces`, `sdk_publish_event_returns_ack_and_delivers_to_subscriber` (all in `tests/integration/test_events.rs`). Scope: v1 is kernel + Rust SDK only; Python/C++ SDK helpers deferred as follow-up.

### R6-02 — Streaming action support (chunked request/response) ✅ done

`ActionRequest`/`ActionResponse` are single envelopes (`bytes params_json`/`data_json`) — no framing for a large body across multiple frames tied to one `action_id`. `send_fragmented` is client-side reassembly for one logical frame, not a multi-message stream. Needed for a real `http_request_stream` action.

**Options:** (a) new `ActionStreamChunk` message (`action_id`, `seq`, `bytes`, `final: bool`) routed by the kernel to the same requester across frames, or (b) let actions open a raw IPC channel (generalize `send_raw_audio`'s `FLAG_RAW_BINARY` path beyond audio) both sides drive manually.

Fixed: went with option (a). New `bool streaming` field on `ActionRequest`, plus new `ActionRequestChunk`/`ActionResponseChunk`/`ActionStreamAbort` messages and `ACTION_STREAM_BACKPRESSURE = 6` added to `ActionStatus` in `wire/proto/veyron_protocol.proto`, mirrored to `sdk/cpp/proto/` and `sdk/python/proto/` (additive, no renumbering). `PluginRegistry` gained `get_pending_action()` (read-only lookup by internal id) and `find_pending_internal_id()` (reverse lookup by `(requester_id, original_action_id)`) in `src/plugins/registry.rs`, used to translate `action_id` across the requester/provider boundary on each chunk hop. Kernel routing in `src/ipc/protocol.rs`: `ActionRequestChunk` forwards requester→provider with the id translated to the internal correlation id, `ActionResponseChunk` forwards provider→requester translated back to the original `action_id`, and both directions verify sender identity against the pending action's recorded requester/provider before trusting a chunk. A failed `try_send_envelope` on either hop (full/closed channel) never silently drops a chunk mid-stream: `abort_stream` removes the pending action, sends the requester an `ActionStreamAbort` plus a terminal `ActionResponse{status: ACTION_STREAM_BACKPRESSURE}`, and best-effort notifies the provider — all via non-blocking `try_send_envelope` so one wedged requester can't stall the shared router loop. New `action_stream_aborted_total{reason=...}` counter at the point an abort is applied. Rust SDK (`sdk/rust/src/client.rs`) gained `send_action_streaming` (sends the initial `ActionRequest{streaming: true}` and returns the `action_id` without waiting for a response), `send_request_chunk`/`send_response_chunk`, and `send_action` now recognizes an inbound `ActionStreamAbort` matching the `action_id` it's awaiting and returns `Err(VeyronError::Internal(..))` instead of discarding it as unrelated traffic. Tests: `get_pending_action_returns_clone_without_removing`, `find_pending_internal_id_matches_requester_and_original_action_id` (`tests/unit/test_registry.rs`); `kernel_forwards_request_chunks_to_provider_with_translated_action_id`, `kernel_forwards_response_chunks_to_requester_with_original_action_id`, `stream_backpressure_aborts_both_sides_and_terminates_with_backpressure_status`, `stream_backpressure_on_requester_channel_does_not_stall_router` (`tests/integration/test_kernel_commands.rs`); `send_action_streaming_sets_streaming_flag_and_returns_action_id`, `send_request_chunk_and_send_response_chunk_roundtrip`, `send_action_returns_error_when_stream_aborted_for_its_action_id` (`sdk/rust/tests/protocol.rs`). Scope: kernel + Rust SDK only, Python/C++ deferred as follow-up.

### R6-03 — Per-caller resource/rate limits at the kernel level ✅ done

`max_procs`/`max_vmem_mb` exist per-plugin in `config.yaml` (R5-10), but nothing limits one *calling* plugin from starving others via actions on a shared provider (e.g. `network` as the standard network path for all plugins).

**Needed:** kernel-enforced per-action-caller quotas, or a documented convention that the provider tracks caller ids from `ActionRequest` itself. **Open question to resolve first:** confirm whether `ActionRequest` carries a caller/requester id field today (proto + `src/ipc/protocol.rs` routing) — R5-07's action routing may already thread `requester_id` through `PendingAction`, worth checking before scoping new proto work.

Fixed: open question confirmed already-resolved before this work started — `PendingAction.requester_id` was already threaded through `src/ipc/protocol.rs` at the routing site. New `ActionStatus::ACTION_QUOTA_EXCEEDED = 5` added to `wire/proto/veyron_protocol.proto` and mirrored to `sdk/cpp/proto/` and `sdk/python/proto/` (additive, no renumbering). New `Config` fields `action_caller_rate_limit_rps`/`action_caller_max_concurrent` (`src/utils/config.rs`, both `Option<u32>`, default `None` = unlimited, documented in `config.yaml`) threaded through `Kernel::run_with_components` (`src/kernel/orchestrator.rs`) into `MessageRouter::run_with_context`. New `PluginRegistry::count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32` (`src/plugins/registry.rs`) scans `pending_actions` and counts entries matching both fields — a scan rather than a maintained counter, to avoid a 3-site desync risk across the registry's existing pending-action removal paths. Enforcement lives in `ActionRequest` routing (`src/ipc/protocol.rs`, `MessageRouter::run_with_context`/`handle_kernel_message`): two new guarded match arms, checked concurrency cap first (via `count_pending_actions_for`), then rate limit (via a new `governor`-keyed limiter, same pattern as the existing `ipc_limiter`, keyed by `(requester_id, provider_id)`); either exceeded sends `ActionResponse{status: ActionQuotaExceeded}` directly to the requester without forwarding to the provider. New metrics: `action_quota_denied_total{reason="concurrency"}` / `{reason="rate"}`. Design doc: `docs/superpowers/specs/2026-07-09-r6-03-caller-quota-design.md`. Tests: `count_pending_actions_for_counts_only_matching_requester_and_provider`, `count_pending_actions_for_reflects_removal` (`tests/unit/test_registry.rs`); `action_concurrency_cap_denies_third_concurrent_call_to_same_provider`, `action_rate_limit_denies_burst_above_configured_rps`, `action_quota_unset_leaves_routing_unlimited` (`tests/integration/test_kernel_commands.rs`).

### R6-04 — WebSocket / long-lived-connection action model ✅ done

Single `ActionRequest`→`ActionResponse` doesn't fit a persistent WebSocket-style session. Needed for any provider (e.g. `network`) that wants to expose a WS-like connection to callers.

**Options:** dedicated `Event`-based push model (open via action, kernel delivers frames as `Event`s to subscribers) or a new bidirectional-stream primitive. Largest of the four — needs its own design doc in this repo (mirrors `docs/superpowers/specs/2026-07-02-action-routing-design.md`'s process for R5-07) before any `network` work depends on it.

Fixed: went with the bidirectional-stream primitive, reusing R6-02's `ActionRequestChunk`/`ActionResponseChunk`/`ActionStreamAbort` wire shape unchanged. New `SessionClose{action_id, reason}` message (Envelope field 25) added to `wire/proto/veyron_protocol.proto` and mirrored to `sdk/cpp/proto/` and `sdk/python/proto/` — peer-initiated graceful termination; forced termination (backpressure, disconnect, idle timeout) still uses `ActionStreamAbort`. `PendingAction` (`src/plugins/registry.rs`) gained `streaming`, `session_accepted`, `last_activity` fields, plus `resolve_action_response()` (atomic accept-in-place-or-evict decision via `DashMap::entry()`), `touch_pending_action()`, and `sweep_idle_sessions()`. `sweep_expired_actions` (R5-07) now skips accepted sessions — the R5-07 deadline only ever governed the accept/reject window. Kernel routing (`src/ipc/protocol.rs`): the `ActionResponse` arm now calls `resolve_action_response` instead of unconditionally evicting — a streaming request's first `ACTION_OK` accepts the session in place; both chunk-forwarding arms bump `last_activity`; a new `SessionClose` arm resolves the sender as either the recorded requester (via `find_pending_internal_id`, mirroring the `ActionRequestChunk` arm) or the recorded provider (direct internal-id lookup, mirroring `ActionResponseChunk`), rejects if the session isn't yet accepted or the sender is neither peer, otherwise forwards to the other side and evicts. `abort_stream` was refactored into a shared `notify_forced_termination` (used by both backpressure/disconnect aborts and the new idle sweep) that only sends the legacy terminal `ActionResponse{ACTION_STREAM_BACKPRESSURE}` for pre-acceptance sessions — an accepted session already got its real `ActionResponse{OK}` and must not receive a second one. New `Config::session_idle_timeout_secs: Option<u32>` (`src/utils/config.rs`, `config.yaml`), default `None` = disabled, checked on the existing 60s prune tick. Rust SDK (`sdk/rust/src/client.rs`) gained `close_session(action_id, reason)`. Design doc: `docs/superpowers/specs/2026-07-10-r6-04-session-streaming-design.md`. Tests: `resolve_action_response_accepts_streaming_ok_without_evicting`, `resolve_action_response_evicts_streaming_error`, `resolve_action_response_evicts_non_streaming_ok`, `resolve_action_response_rejects_mismatched_provider`, `touch_pending_action_updates_last_activity`, `sweep_idle_sessions_evicts_only_accepted_and_idle`, `sweep_expired_actions_skips_accepted_sessions` (`tests/unit/test_registry.rs`); `session_streaming_accept_exchange_and_graceful_close`, `session_streaming_rejection_evicts_without_session_close`, `session_close_before_acceptance_is_rejected`, `session_close_from_third_party_is_rejected`, `session_idle_timeout_aborts_both_sides`, `session_idle_timeout_unset_leaves_accepted_session_open` (`tests/integration/test_kernel_commands.rs`); `close_session_sends_session_close_envelope`, `recv_distinguishes_session_close_from_stream_abort` (`sdk/rust/tests/protocol.rs`). Scope: kernel + Rust SDK only, Python/C++ deferred as follow-up (matches R6-02).

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

**T-03 — Single-threaded IPC router stalls kernel-wide on one slow plugin** ✅ done
`src/ipc/protocol.rs` (`forward`, `broadcast`), `src/events/bus.rs` (`deliver`). All fan-out sends `.await`ed a 50ms timeout inline on the shared router task; `broadcast`/event delivery looped all subscribers = `O(n)*50ms`. One non-draining plugin stalled routing for everyone. Fix: design doc `docs/superpowers/specs/2026-07-08-ipc-router-nonblocking-send-design.md` picked non-blocking `try_send` over spawning a task per send — spawning would let concurrent sends to the same target race out of delivery order, a protocol correctness regression for per-connection frame sequencing. All three call sites (`forward`, `broadcast`, `EventBus::deliver`) now use `try_send`; a full/closed channel drops the frame immediately instead of after up to 50ms, with the same counters as before (`ipc_forward_timeouts_total`, `broadcast_timeouts_total`, `events_dropped_total`). Tests: `tests/unit/test_router.rs` (`forward_to_full_channel_returns_without_waiting`, `broadcast_to_many_stuck_targets_does_not_multiply_delay`), `tests/unit/test_event_bus.rs` (`publish_to_many_stuck_subscribers_does_not_multiply_delay`).

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

**T-14 — No fuzz coverage for C++/Python framing/decompression** ✅ done
`fuzz/fuzz_targets/*` covers Rust `wire` crate only. Fix: libFuzzer harness for `sdk/cpp/src/framing.cpp`.
Fixed (C++): new `sdk/cpp/fuzz/fuzz_framing.cpp`, built via `-DVEYRON_BUILD_FUZZERS=ON` (Clang required for libFuzzer; off by default, doesn't affect normal GCC/Clang builds). Stages fuzz input through a memfd rather than a pipe — a pipe's ~64KiB buffer would deadlock the harness on oversized input before `read_frame_full`'s own length-field check gets a chance to reject it. Exercises both the no-key and MAC-verifying read paths (covers `FLAG_COMPRESSED`/`FLAG_MAC_PRESENT` handling). Smoke-tested 339k execs/20s, no crashes.
Fixed (Python): new `sdk/python/fuzz/fuzz_framing.py` (atheris, optional `veyron-sdk[fuzz]` dep in `sdk/python/pyproject.toml`), same two-path shape as the C++ harness (`read_frame` with and without a fixed MAC session key) against `io.BytesIO` — no live socket needed. Found and fixed a real bug within seconds: a `FLAG_COMPRESSED` frame whose payload isn't valid zstd let `zstandard.ZstdError` escape `read_frame` uncaught, instead of the `ValueError` every other malformed-frame path raises (bad magic, truncated header/payload/tag, CRC mismatch, oversized length). Fixed in `_decompress` (`sdk/python/veyron/framing.py`) by catching `zstandard.ZstdError` and re-raising as `ValueError`. Smoke-tested 5.28M execs/30s post-fix, no crashes. Regression test: `tests/python/test_framing_compressed.py::test_read_frame_rejects_garbage_compressed_payload`.

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

**T-18 — C++ SDK has no fragmentation support** ✅ done
`sdk/cpp/src/framing.cpp`/`client.cpp` vs. Rust (`sdk/rust/src/client.rs:355-420`) and Python (`sdk/python/veyron/client.py:164-197`). Fragmented frames silently mis-parsed by C++. Fix: port fragmentation, or make `read_frame_full` explicitly reject `FLAG_FRAGMENTED`.
Fixed: ported `FLAG_FRAGMENTED` send/receive to the C++ SDK, mirroring the rust/python SDKs. `framing.hpp`/`framing.cpp` gained `FRAG_HEADER_SIZE`, `FragmentHeader`, `pack_frag_header`/`parse_frag_header`, and an `extra_flags` parameter on `pack_frame`/`pack_frame_mac` (needed to OR in `FLAG_FRAGMENTED` alongside `FLAG_MAC_PRESENT`). `VeyronClient` (`client.hpp`/`client.cpp`) gained `send_fragmented()` (splits a payload into `FLAG_FRAGMENTED` frames on a fresh stream id) and `recv_frame()` (transparent reassembly via a `stream_id -> ReassemblyBuf` map, same bounds as the kernel/rust/python: `MAX_REASSEMBLY_STREAMS = 64`, 30s idle prune, 1 MiB reassembled cap); `recv()` now goes through `recv_frame()` instead of calling `read_frame_full` directly. Added a raw-fd `VeyronClient(int fd, ...)` constructor (mirrors rust's `from_stream`) so tests can drive a `socketpair()` pair without a listening UDS socket. Tests: new `sdk/cpp/tests/test_fragmentation.cpp` (5 cases: roundtrip via `recv()`, wire format matches `docs/FRAMING.md`, oversized-payload rejection, fragment-total mismatch within a stream, too-many-concurrent-streams) — all pass alongside the existing 40.

### Low

**T-19 — Action permission model checks provider only, not requester** ✅ done
`src/auth/permissions.rs:10-15`, `src/ipc/protocol.rs:413-430`. Unprivileged plugin can transitively trigger e.g. network requests via any provider with `PERMISSION_NETWORK`. May be intentional (provider-declares-authorization model) — needs explicit design decision/doc note, not silent assumption.
Decision: not intentional — closed as a permission-laundering gap. The provider-declares-authorization model from R5-07 still holds for actions with no entry in `required_permission_for_action` (undeclared/unrestricted actions), but for actions that *do* have a required permission, the provider's grant authorizes it to *perform* the action, not for arbitrary callers to *invoke* it. Fixed: the `ActionLookup::Found` permission-deny guard (`src/ipc/protocol.rs`) now checks `required_permission_for_action` against both `provider.plugin_id` and the requester's `sender_id`, denying with `ActionPermissionDeny` if either lacks it. Doc comments in `src/auth/permissions.rs` and inline at the routing site record the decision so it isn't a silent assumption again. Test: `tests/integration/test_kernel_commands.rs` (`kernel_denies_action_when_requester_lacks_required_permission`).

**T-20 — `strncpy` silently truncates long socket paths in C++ client** ✅ done
`sdk/cpp/src/client.cpp:26-28`. Not an overflow, but truncation is silent rather than rejected. Fix: explicit length check, throw if too long.
Fixed: `VeyronClient::connect()` now checks `socket_path_.size() >= sizeof(sun_path)` before the `strncpy`, throws `std::runtime_error` (and closes the just-opened fd) instead of silently truncating. Tests: `sdk/cpp/tests/test_client.cpp` (`RejectsOverlongSocketPath`, `AcceptsPathAtMaxLength`).

---

## Task Summary

| Phase | Items | Severity | Est. effort |
|-------|-------|----------|--------------|
| 6 Network plugin protocol support | R6-01..04 | ✅ done | R6-01..04 all landed |
| Audit remediation | T-01..20 | 2 Critical, 5 High, 11 Medium, 2 Low | T-01/T-05 fix together; rest are independent; T-01..T-15,T-17,T-18,T-20 ✅ done |

**Ship gate:** Phase 6 (R6-01..04) complete. R6-04 (long-lived streaming sessions: `SessionClose` graceful termination, accept-in-place `PendingAction` state, idle-timeout sweep, kernel + Rust SDK only) ✅ landed. R6-03 (per-caller action quota: concurrency cap + rate limit, keyed by `(requester_id, provider_id)`, off by default) ✅ landed. R6-02 (chunked `ActionRequestChunk`/`ActionResponseChunk` streaming with `ActionStreamAbort` fail-loud backpressure handling, kernel + Rust SDK only, Python/C++ deferred as follow-up) ✅ landed. T-01/T-05 (HTTP authz/validation bypass) ✅ landed — was the live privilege-escalation path on any deployment exposing the REST API. T-02 (C++ SDK integration coverage), T-03 (single-threaded IPC router/event-bus fan-out now uses non-blocking `try_send`, no more O(n)*50ms broadcast stall), T-04 (config.yaml/JWT permission binding), T-06 (C++/Python `EventAck`), T-07 (Rust SDK error propagation), T-08 (error-budget prune staleness), T-09 (WS connection cap), T-10 (`get_plugin_logs` lines clamp), T-11 (marketplace maintainer signature), T-12 (JWT secret min-strength check), T-13 (C++ malformed-frame unit tests), T-14 (C++ + Python fuzz harnesses), T-15 (C++/Python slow-loris timeout), T-17 (proto drift CI check), T-18 (C++ fragmentation support), T-19 (requester-side action permission check), and T-20 (strncpy path-length check) ✅ landed. Remaining open: T-16 (deferred to next protocol version bump).

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- Protocol changes: `proto/veyron_protocol.proto` updated with `reserved` discipline, all three SDKs updated in the same change.
- Docs updated in the same PR (`docs/FRAMING.md` for wire changes, README for operator-visible changes).
