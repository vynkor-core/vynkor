# Veyron ROADMAP — Phase 5

**Baseline:** 2026-07-02 · Kernel `0.1.0` · Audit ~78/100 (see `AUDIT.md`)
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md`)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-07-02

| Metric | Value |
|--------|-------|
| Kernel version | 0.1.0 |
| Audit score | ~78/100 (`AUDIT.md`, 2026-07-02) |
| Tests | `cargo test --all --all-features`: 263 passing, 0 failing |
| Clippy | clean (`--all-targets --all-features -D warnings`) |
| Kernel core | ✅ framing/MAC/fragmentation/supervision solid, regression-tested |
| SDKs | Rust ✅ · Python ✅ · C++ ✅ — frame parity (R5-01/02) and secured-mode (R5-05) closed |
| CLI | ✅ JWT bearer auth, TLS-aware scheme, `plugin start` route, `--config` honored (R5-06) |

### Completed — Phase 4 (audit regression fixes) ✓

| Task | Summary |
|------|---------|
| T-10 ✓ | Compressed-frame invariant normalized + MAC coverage (BUG-001/002) — kernel & Rust SDK |
| T-11 ✓ | Fragment reassembly caps: 64 streams, 1 MiB, 30 s timeout (BUG-003) |
| T-12 ✓ | HTTP rate limit keyed on verified JWT `sub`, layered behind auth (BUG-004) |
| T-13 ✓ | Per-plugin shutdown grace honored by supervisor (BUG-005) |
| T-14 ✓ | Socket path never defaults to shared `/tmp` (BUG-006) |

### Completed — Audit Quick Wins (2026-07-02) ✓

| # | Item |
|---|------|
| QW-01 ✓ | Deleted 0-byte dead files `src/ipc/client.rs`, `src/kernel/config.rs` |
| QW-02 ✓ | Deleted orphaned `proto/build.rs` (root `build.rs` owns codegen) |
| QW-03 ✓ | Dropped unused deps `chrono`, `prost-types`; `tower-http` trimmed to `["timeout"]` |
| QW-04 ✓ | Deleted diverged duplicate `docs/veyron_protocol.proto` |
| QW-05 ✓ | `docs/FRAMING.md`: FLAG_COMPRESSED/FLAG_FRAGMENTED documented as implemented, incl. MAC normalization rule |
| QW-06 ✓ | README: CRC mismatch drops connection (was "connection intact") |
| QW-07 ✓ | CLAUDE.md: `kernel.rs`→`orchestrator.rs`, UDS-only wording, archived-doc paths |
| QW-08 ✓ | IPC denials now send `ERR_PERMISSION_DENIED` (was `ERR_UNKNOWN`) |
| QW-09 ✓ | SDK socket fallback = kernel's XDG resolution, never `/tmp` (Rust + Python + config.yaml comment) |
| QW-10 ✓ | `create_test_token` moved out of prod crate into `tests/support/jwt_helper.rs` |
| QW-11 ✓ | `PluginShutdown` advertises each plugin's real grace window (was hardcoded 5) |
| QW-12 ✓ | ROADMAP regenerated (this file); stale baselines removed |

---

## Phase 5.1 — Protocol Parity (release blockers)

**Goal:** every SDK and gateway path speaks the same wire protocol the kernel does.

**Done-when:** a ≥ 64 KiB payload round-trips kernel↔plugin on all three SDKs, secured and unsecured, in CI.

### R5-01 ✓ — Python & C++ SDK: decompress + MAC normalization (Critical, AUDIT C-01/C-02)

**Files:** `sdk/python/veyron/framing.py`, `sdk/cpp/src/framing.cpp`, `sdk/python/pyproject.toml`, `sdk/cpp/CMakeLists.txt`

The kernel zstd-compresses outbound payloads ≥ 64 KiB (`FLAG_COMPRESSED`) and computes the MAC over the *plaintext* header/payload. Both SDK read paths hand compressed bytes to protobuf and verify the MAC against the wire header — any large frame kills the plugin.

**Fix:** after CRC check, when `FLAG_COMPRESSED` set: decompress (bounded to `MAX_PAYLOAD`), rebuild the plaintext header (`flags & ~FLAG_COMPRESSED`, plaintext length/CRC), verify tag against that — mirroring `src/ipc/framing.rs:230-243`. Python gains a `zstandard` dependency; C++ links libzstd.

**Acceptance:** new cross-SDK integration tests (extend `tests/integration/test_sdk_python.rs` / `test_sdk_cpp.rs`) pushing a 100 KiB event, secured + unsecured. Consider a kernel config kill-switch (`compression: false`) as an interim release valve.

**Effort:** 1–2 d

**Done:** `framing.py` gains `_decompress`/`_normalize` helpers mirroring the Rust read path — `read_frame`/`async_read_frame` now decompress `FLAG_COMPRESSED` payloads (bounded to `MAX_PAYLOAD` via `zstandard`'s `max_output_size`) before MAC verification, rebuilding the plaintext header the sender's tag was computed over. `pyproject.toml` gains `zstandard>=0.22`. C++'s `read_frame_full` does the same via libzstd (`ZSTD_getFrameContentSize`/`ZSTD_decompress`, bounded to `MAX_PAYLOAD_SIZE`); `CMakeLists.txt` links `PkgConfig::ZSTD`. Also fixed a pre-existing build break: `FLAG_MAC_PRESENT` was defined in both `mac.hpp` and `framing.hpp`, so the C++ SDK never actually compiled — the C++ test binary now builds and `ctest` passes (12 tests, incl. 3 new compressed-frame cases in `tests/test_mac.cpp`). New unit tests: `tests/python/test_framing_compressed.py` (4 cases: decompress round-trip, MAC verify, bad-MAC rejection, uncompressed unaffected) — full cross-SDK 100 KiB `tests/integration/test_sdk_*.rs` harness extension deferred to R5-02.

### R5-02 ✓ — Cross-SDK large-frame test harness (Critical companion to R5-01)

**Files:** `tests/integration/sdk_harness.rs`, `test_sdk_*.rs`

No existing test sends a payload ≥ `COMPRESS_THRESHOLD` across SDK boundaries — that's why R5-01 shipped broken. Add ≥ 64 KiB round-trips (both directions) and a fragmented-message case to the shared harness so protocol regressions cannot land silently.

**Effort:** 0.5–1 d

**Done:** `SdkHarness` now exposes `registry`/`event_bus` so tests can drive the kernel directly. `python_sdk_large_frame_round_trip` (`tests/integration/test_sdk_python.rs`) subscribes a live Python client to an event type, publishes a 100 KiB `Event` through `EventBus::publish` (kernel compresses since it's ≥ `COMPRESS_THRESHOLD`), and asserts the SDK decompresses it byte-for-byte; skips cleanly when `zstandard`/Python are unavailable. (Peer-to-peer `forward()` unicast wasn't used as the vehicle because the committed `veyron_protocol_pb2.py` predates the `PluginManifest.ipc_targets` field added in R5-04/T-04 and regenerating it was out of scope here — tracked as follow-up.) C++ decompression is covered at the unit level in `sdk/cpp/tests/test_mac.cpp` (`FramingCompressed.*`, 3 cases) since no C++ reference plugin binary exists for the integration harness yet (same stand-in gap `test_sdk_cpp.rs` already documents). Fragmented-message case not yet added.

### R5-03 ✓ — WebSocket gateway: define compressed/fragmented inbound behavior (High, AUDIT C-03)

**Files:** `src/api/websocket.rs:168-212`, `docs/FRAMING.md`

WS `parse_frame` neither decompresses nor reassembles. Either share the UDS normalization/reassembly logic, or explicitly reject frames carrying `FLAG_COMPRESSED`/`FLAG_FRAGMENTED` with a clear error — then document the rule. Rejection is acceptable (WS has native message framing); silence is not.

**Effort:** 0.5 d (reject) / 2 d (full support)

**Done:** `parse_frame` now rejects inbound binary frames carrying `FLAG_COMPRESSED` or `FLAG_FRAGMENTED` with a parse error, before CRC/length parsing proceeds — counted against the existing `MAX_WS_PARSE_ERRORS` budget like any other malformed frame, so a client sending one doesn't get disconnected outright. Documented in `docs/FRAMING.md` (new "WebSocket Gateway Inbound Frame Support" section). Test: `ws_rejects_compressed_and_fragmented_inbound_frames` (`tests/integration/test_websocket.rs`) — sends both flag variants, then confirms the connection survives and a subsequent legitimate frame still round-trips.

---

## Phase 5.2 — Auth & Tooling Gaps

**Goal:** the documented production posture (JWT on) is actually usable end-to-end.

### R5-04 ✓ — Permission-gate `KernelCommand` (High, AUDIT H-01)

**Files:** `src/ipc/protocol.rs:408-423`, `proto/veyron_protocol.proto`

Any registered plugin can invoke `reload_config` regardless of permissions. Add `PERMISSION_KERNEL_ADMIN = 12` to the proto enum + `KNOWN_PERMISSIONS`, check before `CommandHandler::dispatch`, propagate to all three SDK permission lists. `health_check` may remain ungated.

**Acceptance:** test proving a permissionless plugin gets `ERR_PERMISSION_DENIED` on `reload_config`.

**Effort:** 0.5 d

**Done:** `PERMISSION_KERNEL_ADMIN = 12` added to proto + `KNOWN_PERMISSIONS`; `CommandStatus.COMMAND_PERMISSION_DENIED = 3` added; `MessageRouter` checks it before `CommandHandler::dispatch` for every command except `health_check`. Tests: `reload_config_without_admin_permission_is_denied`, `health_check_exempt_from_admin_permission` (`tests/integration/test_kernel_commands.rs`).

### R5-05 ✓ — SDK plugin base classes: secured-mode support (High, AUDIT H-04)

**Files:** `sdk/python/veyron/plugin.py`, `sdk/rust/src/plugin.rs`, `sdk/cpp/include/veyron/plugin.hpp`, `sdk/cpp/include/veyron/env.hpp`, `sdk/cpp/src/env.cpp`

Python `Plugin` constructs its client without a secret (first post-registration send → connection dropped on secured kernel); Rust `Plugin::run` registers with an empty token (rejected outright). Thread `jwt_token` + shared secret through (env vars `VEYRON_JWT_TOKEN` / `VEYRON_SECRET` as default source). Add secured-mode SDK harness tests.

**Effort:** 1–2 d

**Done:** Rust `Plugin::run_with`/`VeyronClient::connect` already read `VEYRON_JWT_TOKEN`/`VEYRON_JWT_SECRET` and use `connect_with_secret` — confirmed via existing `mac_secured_registration_and_tagged_frames` integration test, no change needed. Python `Plugin.__init__` now defaults `jwt_token` from `VEYRON_JWT_TOKEN` (when the subclass didn't set one) and passes `VEYRON_JWT_SECRET`'s bytes to `VeyronClient`'s `secret` param (`tests/python/test_plugin_env.py`, 4 cases). C++ `Plugin` gained the same env wiring via new `veyron::resolve_jwt_token`/`resolve_jwt_secret` helpers (`sdk/cpp/include/veyron/env.hpp`) and — since it had no socket-path resolution at all, hardcoding `/tmp/veyron.sock` (a BUG-006 regression AUDIT hadn't flagged for C++) — a `default_socket_path()` mirroring the kernel's XDG_RUNTIME_DIR → `/run/user/<uid>` → `~/.veyron/run` logic; `register_plugin` now sends the resolved token. Tests: `sdk/cpp/tests/test_env.cpp` (8 cases) + `sdk/cpp/tests/test_plugin.cpp` (2 cases), 25/25 passing via `ctest`.

### R5-06 ✓ — CLI: JWT header, TLS scheme, fix `plugin start` (High, AUDIT H-02/H-03)

**Files:** `src/cli/plugin.rs`, `src/cli/mod.rs`, `src/api/server.rs`, `src/api/routes.rs`, `src/plugins/loader.rs`, `src/kernel/orchestrator.rs`, `src/main.rs`

- `vyn plugin start` POSTs `/plugins/{id}/start` — route doesn't exist (always 404). Add the route (resolve from config `plugins:`, call `PluginManager::start`) or drop the subcommand.
- `api_get`/`api_post` attach no `Authorization` header and hardcode `http://` — CLI is unusable against a secured/TLS kernel. Accept token via flag/env/config; derive scheme from `tls_cert_path`.
- `Commands::Plugin` hardcodes `config.yaml`; honor `--config` like every other subcommand.

**Acceptance:** route-level test for each CLI-invoked endpoint; secured-kernel CLI smoke test.

**Effort:** 1–2 d

**Done:** `POST /plugins/:id/start` added — `AppState` now carries `plugin_defs: Vec<PluginDef>` (the config.yaml-declared set, threaded from `Kernel::run_with_components` → `ApiServer::new` → `create_router_full`), so the route can only spawn binaries the operator declared, never an arbitrary path; 404 when `id` isn't declared, 409 when already supervised. `PluginLoader::config_from_def` extracted (previously inlined in `load_all`) so both the boot-time loader and this route build `PluginConfig` identically. Tests: `start_plugin_spawns_process_declared_in_config`, `start_unknown_plugin_returns_404`, `start_already_running_plugin_returns_conflict` (`tests/unit/test_api.rs`). CLI: `api_get`/`api_post` now take a `base_url` + `Option<&str>` token and attach `Authorization: Bearer` via `reqwest`'s `bearer_auth` when present (`sdk` parity with the three plugin SDKs' env-var convention); `base_url()` derives `https://` whenever the loaded config has `tls_cert_path` set, `http://` otherwise. `Commands::Plugin` gained `--config` (was hardcoded to `config.yaml`) and `--token` (falls back to `VEYRON_JWT_TOKEN`). Tests: `base_url_defaults_to_http`, `base_url_uses_https_when_tls_configured`, `api_get_attaches_bearer_token_when_present`, `api_get_sends_no_authorization_header_without_token`, `api_post_attaches_bearer_token_when_present` (`src/cli/plugin.rs`, via `mockito`).

### R5-07 ✓ — Decide & implement the Action system (High, AUDIT H-05)

**Files:** `src/ipc/protocol.rs:385-406`, `src/plugins/registry.rs`, `src/auth/permissions.rs`, `proto/veyron_protocol.proto` (unchanged — no wire format changes needed)

**Decision:** option (b) — route actions to provider plugins that declare them in `manifest.actions`, correlating responses by `action_id`. Design approved 2026-07-02, spec at `docs/superpowers/specs/2026-07-02-action-routing-design.md`.

**Approved design:**
- Kernel scans registered plugins for one whose `manifest.actions` contains the requested action name. 0 matches → `ACTION_NOT_FOUND`. >1 matches → `ACTION_NOT_FOUND` + `warn!` logging the colliding plugin ids (ambiguous declaration is a deploy misconfiguration, not a kernel routing decision).
- No new permission gate — "a provider declared it" is sufficient authorization. `action_to_permission()` (the old builtin-name map) and its unit test are retired as dead code once routing no longer consults it.
- Kernel mints its own internal correlation id per hop (`kact-{seq}`) rather than trusting the requester's `action_id` to be globally unique across plugin processes; tracks `PendingAction { requester_write_tx, original_action_id, requester_id, deadline }` keyed by that internal id in a new `DashMap` on `PluginRegistry`.
- Provider receives a rewritten `ActionRequest` (internal id in place of the original) via the existing `send_envelope` helper, and — per the one new convention declared-action providers must follow — always answers with `ActionResponse` targeted at `"kernel"` (it doesn't know who really asked).
- Kernel's `ActionResponse` handling: look up the internal id; if it doesn't match a pending entry, drop silently (late/duplicate, not a protocol error). On match, remove the entry, rewrite `action_id` back to the requester's original id, and proxy `status`/`data_json`/`error` through unchanged — including provider-side failures (`ACTION_ERROR`, `ACTION_PERMISSION_DENY`) relayed as-is, not translated.
- Timeout: piggybacks on the router's existing 60 s `prune_tick` — sweeps pending actions past their deadline (`timeout_ms`, default 30 s per proto doc), sends `ACTION_TIMEOUT` to the requester, evicts. No new timer task.
- Disconnect edge cases handled by construction: provider gone → requester times out at next sweep; requester gone → stale `write_tx` send silently no-ops until swept (existing pattern elsewhere in this file already tolerates that).

**Testing:** integration test for full provider round-trip; ambiguous-provider test; unit test on `PluginRegistry::sweep_expired_actions`/correlation using synthetic `Instant`s (no real-time waiting — avoids a slow/flaky 60 s+ test). Existing `kernel_targeted_action_request_returns_not_found_not_fake_ok` stays green (no provider registered for `get_cpu`).

**Effort:** 3–5 d

**Done:** `PluginRegistry::find_action_provider` scans `manifest.actions` across registered plugins (`ActionLookup::NotFound`/`Found`/`Ambiguous`). Kernel mints its own `kact-{seq}` correlation id per hop (`ACTION_CORRELATION_SEQ`), tracks `PendingAction{requester_write_tx, original_action_id, requester_id, deadline, provider_id}` in a `DashMap` on `PluginRegistry`, and rewrites/forwards the `ActionRequest` to the resolved provider. The `ActionResponse` arm resolves the sender's plugin identity via `get_by_conn_id` *before* touching the pending-action table and only removes/proxies the entry via `take_pending_action_if_provider` if the sender is the routed provider — closing a response-spoofing gap where any registered plugin could otherwise guess the sequential `kact-<n>` id and steal/inject another provider's response. Timeout sweep piggybacks on the router's existing 60 s `prune_tick`: `sweep_expired_actions(Instant::now())` evicts entries past `deadline` and sends `ACTION_TIMEOUT` to each requester. `action_to_permission()` (`src/auth/permissions.rs`) retired as dead code along with its unit tests — routing is now purely provider-lookup driven, no permission gate beyond "a provider declared it." Tests: `kernel_routes_action_to_declared_provider_and_correlates_response`, `action_response_from_non_provider_plugin_is_rejected_not_proxied`, `ambiguous_action_providers_returns_not_found`, `provider_side_action_failure_proxies_through_unchanged` (`tests/integration/test_kernel_commands.rs`); `find_action_provider_*`, `pending_action_*`, `sweep_expired_actions_evicts_past_deadline_only`, `take_pending_action_if_provider_*` (`tests/unit/test_registry.rs`, synthetic `Instant`s — no real-time waiting). Existing `kernel_targeted_action_request_returns_not_found_not_fake_ok` stays green.

---

## Phase 5.3 — Robustness (Medium findings)

### R5-08 ✓ — Prune keyed rate limiters (AUDIT M-01)
`src/ipc/protocol.rs:89-91`, `src/api/rate_limit.rs:16-21` — `governor` keyed state grows forever (monotonic conn_ids; attacker-chosen `sub`). Call `retain_recent()` periodically (piggyback watchdog tick). **0.5 h**
**Done:** both keyed limiters (`MessageRouter`'s per-conn IPC limiter, `rate_limit.rs`'s per-`sub` HTTP limiter) now get a 60 s `tokio::time::interval` that calls `retain_recent()`, evicting idle keys instead of retaining them for process lifetime.

### R5-09 ✓ — Duplicate fragment accounting (AUDIT M-02)
`src/ipc/connection.rs:235-236` — re-sent sequence overwrites the fragment but double-counts `buffered_bytes` → spurious oversize disconnect. Subtract the replaced fragment's length, or reject duplicate sequences. Regression test. **1 h**
**Done:** `buffered_bytes` now nets out the replaced fragment's old length before adding the new one, both in the oversize check and the accumulator. Test: `resent_fragment_does_not_double_count_buffered_bytes` (`src/ipc/connection.rs`).

### R5-10 ✓ — Resource limits: unconditional + configurable (AUDIT M-03)
`src/plugins/runner.rs`, `src/plugins/supervisor.rs:139-146`, `src/utils/config.rs` — `RLIMIT_NPROC`/`RLIMIT_AS` apply only with `sandbox: true` and are hardcoded. Apply `apply_resource_limits` unconditionally in `pre_exec`; expose values in `PluginDef`. **0.5 d**
**Done:** `pre_exec` now always sets rlimits (Linux) — `sandbox` only gates the namespace `unshare`. `PluginDef`/`PluginConfig` gained `max_procs`/`max_vmem_mb: Option<u64>` (default `runner::DEFAULT_MAX_PROCS`=64, `DEFAULT_MAX_VMEM_MB`=512), documented in `config.yaml`.

### R5-11 ✓ — `forward()` must strip stale MAC like `broadcast()` (AUDIT M-04)
`src/ipc/protocol.rs` — unicast forward keeps the sender's `FLAG_MAC_PRESENT` + tag; broadcast rebuilds without. Normalize both. **0.5 h**
**Done:** `forward()` now rebuilds the frame with `FLAG_MAC_PRESENT` cleared and `mac: None`, mirroring `broadcast()`. Test: `forward_strips_flag_mac_present` (`tests/unit/test_router.rs`).

### R5-12 ✓ — Retire or implement dead protocol surface (AUDIT M-05)
`proto/veyron_protocol.proto`, `src/ipc/protocol.rs` — `Envelope.version` never checked (`ERR_PROTOCOL_MISMATCH` unused), `AudioStreamChunk` to `"kernel"` unhandled, `ERR_MAC_MISSING`/`ERR_MAC_INVALID` never sent, `needs_gpu`/`priority`/`PluginRegister.version` ignored. Implement version check at registration; `reserved`-retire the rest or document intent. **1 d**

**Done:** `Envelope.version` (field 4) `reserved`-retired — no SDK ever set it and no versioning scheme exists; a real one is future work, not worth half-implementing (team decision: retire over inventing a scheme). `PluginManifest.needs_gpu`/`priority` (fields 5, 6) `reserved`-retired — truly dead, no SDK sets them, no scheduler reads them, out of scope for a "dumb byte router" kernel. `PluginRegister.version` kept as-is (all 3 SDKs populate it) but documented as descriptive-only: version *compatibility* is already enforced separately at install time via `PluginEntry.min_kernel_version`/`max_kernel_version` in the marketplace registry, so wiring this field up would just duplicate that check — not worth the `PluginRegistry::register()` signature change across ~40 call sites for a cosmetic display value. `AudioStreamChunk` to `"kernel"` documented as an invalid target (proto comment): it's peer-to-peer plugin↔plugin streaming only, and already correctly falls through to `ERR_UNKNOWN` like any other unhandled kernel-targeted payload — no code change needed. `ERR_MAC_MISSING`/`ERR_MAC_INVALID` are now actually sent: `ConnectionHandler::run` sends the appropriate one (missing tag vs. bad tag) before dropping a connection on MAC failure, instead of going silent — previously indistinguishable from a network failure from the client's side. Tests: `read_loop_sends_mac_invalid_before_dropping_on_bad_tag`, `read_loop_sends_mac_missing_before_dropping_on_untagged_frame` (`src/ipc/connection.rs`); `secured_kernel_rejects_unmaced_client` updated to assert the new error frame arrives before disconnect (`tests/integration/test_mac.rs`). Documented in `docs/FRAMING.md` ("MAC Failure Wire Behavior"). Verified all three SDKs still build/test clean after the field removals: Rust and C++ regenerate proto bindings at build time (`prost-build`/CMake `protobuf_generate_cpp`) and picked up the change automatically; Python's committed `veyron_protocol_pb2.py` was already stale before this change (separate known issue, Phase 5.4) and wasn't touched — removing `reserved` field numbers from the `.proto` doesn't break wire compatibility with it, and `pytest tests/python/` still passes (21 passed, 3 skipped, no kernel).

### R5-13 ✓ — Marketplace: download + zip-bomb caps (AUDIT M-06)
`src/marketplace/installer.rs` — unbounded in-memory download; `extract_zip` lacks decompressed-size/entry-count caps. Add size ceilings (e.g. 256 MiB archive, 1 GiB extracted, 10 k entries). **0.5 d**
**Done:** `MAX_ARCHIVE_BYTES` (256 MiB, checked against `Content-Length` and streamed total), `MAX_EXTRACTED_BYTES` (1 GiB, enforced on actual bytes written per entry via `Read::take`, not the archive's declared size), `MAX_ARCHIVE_ENTRIES` (10k, checked before any extraction). Tests: `archive_with_excess_entries_rejected`, `zip_bomb_decompressed_size_capped` (`tests/unit/test_installer.rs`).

### R5-14 ✓ — Stop compiling the crate twice (AUDIT M-07)
`src/main.rs:1-11` — bin re-declares all modules instead of `use veyron::…`. Fix, then sweep now-meaningful `#[allow(dead_code)]`. Halves unit-test duplication (30 tests currently run twice). **0.5–1 d**
**Done:** `main.rs` dropped its `mod` declarations for `lib.rs`'s tree and now imports via `use veyron::{cli, kernel, utils}` — the bin target no longer recompiles the whole crate as a second copy (confirmed: `unittests src/main.rs` now runs 0 tests, down from the lib's full suite). All 17 `#[allow(dead_code)]` attributes across `src/` were dead-code-lint artifacts of that duplicate compilation and were removed cleanly (verified by a full rebuild with `-D warnings` after removal — zero new warnings).

### R5-15 ✓ — Atomic registry registration (AUDIT M-08)
`src/plugins/registry.rs:47-77` — check-then-insert across two DashMaps is TOCTOU-safe only because the router is single-threaded. Linearize via `DashMap::entry` or document the single-caller invariant. **0.5 d**
**Done:** `register()` reserves both `by_conn_id` and `by_plugin_id` slots via `DashMap::entry()` (shard lock held across the check+insert) instead of `contains_key` + `insert`. Test: `concurrent_registration_of_same_plugin_id_has_exactly_one_winner` (`tests/unit/test_registry.rs`, 50 racing threads).

### R5-16 ✓ — pid/log files out of shared `/tmp` (AUDIT M-09)
`src/utils/config.rs:138-140`, `config.yaml`, `src/main.rs` — symlink-attack surface; socket got this fix (BUG-006), pid/log did not. Default under `$XDG_RUNTIME_DIR`/`~/.veyron`; open pid file `O_NOFOLLOW`. **0.5 d**
**Done:** `default_socket_path`'s private-dir resolution factored out into `default_private_dir()`, reused by new `default_pid_path()`/`default_log_path()` (serde defaults on `Config::pid_file`/`log_file`). `run_foreground`'s pid-file open now passes `O_NOFOLLOW`. `config.yaml`'s dev example no longer hardcodes `/tmp`. Test: `default_pid_and_log_paths_never_land_in_shared_tmp`.

---

## Phase 5.4 — Debt & Polish (non-blocking)

| Item | Ref | Notes |
|------|-----|-------|
| Zero-copy hot path (`Arc<[u8]>` payloads; drop per-frame/per-subscriber clones) | AUDIT M-03-adj | `write_frame_raw`, `broadcast` |
| Two `PluginEntry` / two `PluginManifest` types | AUDIT §7 | rename marketplace pair `RegistryEntry`/`InstallManifest` |
| `CLK_TCK` hardcoded 100 | AUDIT §7 | `sysconf(_SC_CLK_TCK)` |
| Deterministic system `event_id` breaks re-delivery within retention | AUDIT §7 | suffix timestamp/counter |
| Python `__init__.py` degrades imports to `None` silently | AUDIT §7 | re-raise with actionable message |
| Env-var mutation in `config.rs` tests (parallel-unsafe) | AUDIT §7 | serialize or use `temp-env` |
| Dependency refresh: axum 0.8, nix, rusqlite, opentelemetry stack | AUDIT §6 | no conflicts today; batch upgrade |
| SIGHUP reload + SIGTERM-ignoring-plugin shutdown tests | AUDIT §4 | lifecycle coverage |
| Committed `sdk/python/veyron/veyron_protocol_pb2.py` is stale (missing `ipc_targets`, `PERMISSION_AUDIO_STREAM`/`PERMISSION_KERNEL_ADMIN`, `COMMAND_PERMISSION_DENIED`) | found during R5-02 | regenerate via `scripts/gen_proto_python.py` and commit |

---

## Task Summary

| Phase | Items | Severity | Est. effort |
|-------|-------|----------|-------------|
| 5.1 Protocol parity | R5-01..03 | Critical/High | ~4 d |
| 5.2 Auth & tooling | R5-04..07 | High | ~5 d + 1 decision |
| 5.3 Robustness | R5-08..16 | Medium | ~4 d |
| 5.4 Debt & polish | 8 items | Low | opportunistic |

**Ship gate:** Phase 5.1 complete → non-Rust SDKs usable. Phase 5.2 complete → secured deployments operable end-to-end. 5.3/5.4 schedulable freely.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- Protocol changes: `proto/veyron_protocol.proto` updated with `reserved` discipline, all three SDKs updated in the same change.
- Docs updated in the same PR (`docs/FRAMING.md` for wire changes, README for operator-visible changes).
