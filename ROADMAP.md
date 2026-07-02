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
| SDKs | Rust ✅ · Python ⚠️ · C++ ⚠️ — non-Rust SDKs break on frames ≥ 64 KiB (R5-01) |
| CLI | ⚠️ dev-mode only: no JWT/TLS support, `plugin start` route missing (R5-06) |

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

### R5-01 — Python & C++ SDK: decompress + MAC normalization (Critical, AUDIT C-01/C-02)

**Files:** `sdk/python/veyron/framing.py`, `sdk/cpp/src/framing.cpp`, `sdk/python/pyproject.toml`, `sdk/cpp/CMakeLists.txt`

The kernel zstd-compresses outbound payloads ≥ 64 KiB (`FLAG_COMPRESSED`) and computes the MAC over the *plaintext* header/payload. Both SDK read paths hand compressed bytes to protobuf and verify the MAC against the wire header — any large frame kills the plugin.

**Fix:** after CRC check, when `FLAG_COMPRESSED` set: decompress (bounded to `MAX_PAYLOAD`), rebuild the plaintext header (`flags & ~FLAG_COMPRESSED`, plaintext length/CRC), verify tag against that — mirroring `src/ipc/framing.rs:230-243`. Python gains a `zstandard` dependency; C++ links libzstd.

**Acceptance:** new cross-SDK integration tests (extend `tests/integration/test_sdk_python.rs` / `test_sdk_cpp.rs`) pushing a 100 KiB event, secured + unsecured. Consider a kernel config kill-switch (`compression: false`) as an interim release valve.

**Effort:** 1–2 d

### R5-02 — Cross-SDK large-frame test harness (Critical companion to R5-01)

**Files:** `tests/integration/sdk_harness.rs`, `test_sdk_*.rs`

No existing test sends a payload ≥ `COMPRESS_THRESHOLD` across SDK boundaries — that's why R5-01 shipped broken. Add ≥ 64 KiB round-trips (both directions) and a fragmented-message case to the shared harness so protocol regressions cannot land silently.

**Effort:** 0.5–1 d

### R5-03 — WebSocket gateway: define compressed/fragmented inbound behavior (High, AUDIT C-03)

**Files:** `src/api/websocket.rs:168-212`, `docs/FRAMING.md`

WS `parse_frame` neither decompresses nor reassembles. Either share the UDS normalization/reassembly logic, or explicitly reject frames carrying `FLAG_COMPRESSED`/`FLAG_FRAGMENTED` with a clear error — then document the rule. Rejection is acceptable (WS has native message framing); silence is not.

**Effort:** 0.5 d (reject) / 2 d (full support)

---

## Phase 5.2 — Auth & Tooling Gaps

**Goal:** the documented production posture (JWT on) is actually usable end-to-end.

### R5-04 ✓ — Permission-gate `KernelCommand` (High, AUDIT H-01)

**Files:** `src/ipc/protocol.rs:408-423`, `proto/veyron_protocol.proto`

Any registered plugin can invoke `reload_config` regardless of permissions. Add `PERMISSION_KERNEL_ADMIN = 12` to the proto enum + `KNOWN_PERMISSIONS`, check before `CommandHandler::dispatch`, propagate to all three SDK permission lists. `health_check` may remain ungated.

**Acceptance:** test proving a permissionless plugin gets `ERR_PERMISSION_DENIED` on `reload_config`.

**Effort:** 0.5 d

**Done:** `PERMISSION_KERNEL_ADMIN = 12` added to proto + `KNOWN_PERMISSIONS`; `CommandStatus.COMMAND_PERMISSION_DENIED = 3` added; `MessageRouter` checks it before `CommandHandler::dispatch` for every command except `health_check`. Tests: `reload_config_without_admin_permission_is_denied`, `health_check_exempt_from_admin_permission` (`tests/integration/test_kernel_commands.rs`).

### R5-05 — SDK plugin base classes: secured-mode support (High, AUDIT H-04)

**Files:** `sdk/python/veyron/plugin.py`, `sdk/rust/src/plugin.rs`, `sdk/cpp/include/veyron/plugin.hpp`

Python `Plugin` constructs its client without a secret (first post-registration send → connection dropped on secured kernel); Rust `Plugin::run` registers with an empty token (rejected outright). Thread `jwt_token` + shared secret through (env vars `VEYRON_JWT_TOKEN` / `VEYRON_SECRET` as default source). Add secured-mode SDK harness tests.

**Effort:** 1–2 d

### R5-06 — CLI: JWT header, TLS scheme, fix `plugin start` (High, AUDIT H-02/H-03)

**Files:** `src/cli/plugin.rs`, `src/api/server.rs`, `src/main.rs:109`

- `vyn plugin start` POSTs `/plugins/{id}/start` — route doesn't exist (always 404). Add the route (resolve from config `plugins:`, call `PluginManager::start`) or drop the subcommand.
- `api_get`/`api_post` attach no `Authorization` header and hardcode `http://` — CLI is unusable against a secured/TLS kernel. Accept token via flag/env/config; derive scheme from `tls_cert_path`.
- `Commands::Plugin` hardcodes `config.yaml`; honor `--config` like every other subcommand.

**Acceptance:** route-level test for each CLI-invoked endpoint; secured-kernel CLI smoke test.

**Effort:** 1–2 d

### R5-07 — Decide & implement the Action system (High, AUDIT H-05)

**Files:** `src/ipc/protocol.rs:366-406`, `proto/veyron_protocol.proto`, `src/plugins/loader.rs:122-126`

`ActionRequest` checks permissions then returns `ACTION_OK` with empty data — a stub that misleads callers. Decision required (manifesto says dumb core → option b):
(a) kernel-executed built-in actions, or
(b) route actions to provider plugins that declare them in `manifest.actions` (currently only logged), correlating responses by `action_id`.
Interim: return `ACTION_NOT_FOUND` instead of fake success.

**Effort:** decision + 3–5 d (option b) / 0.5 h (interim honesty fix)

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

### R5-12 — Retire or implement dead protocol surface (AUDIT M-05)
`proto/veyron_protocol.proto`, `src/ipc/protocol.rs` — `Envelope.version` never checked (`ERR_PROTOCOL_MISMATCH` unused), `AudioStreamChunk` to `"kernel"` unhandled, `ERR_MAC_MISSING`/`ERR_MAC_INVALID` never sent, `needs_gpu`/`priority`/`PluginRegister.version` ignored. Implement version check at registration; `reserved`-retire the rest or document intent. **1 d**

### R5-13 — Marketplace: download + zip-bomb caps (AUDIT M-06)
`src/marketplace/installer.rs` — unbounded in-memory download; `extract_zip` lacks decompressed-size/entry-count caps. Add size ceilings (e.g. 256 MiB archive, 1 GiB extracted, 10 k entries). **0.5 d**

### R5-14 — Stop compiling the crate twice (AUDIT M-07)
`src/main.rs:1-11` — bin re-declares all modules instead of `use veyron::…`. Fix, then sweep now-meaningful `#[allow(dead_code)]`. Halves unit-test duplication (30 tests currently run twice). **0.5–1 d**

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
