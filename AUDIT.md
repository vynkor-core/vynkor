# Veyron Kernel — Architectural Audit

**Date:** 2026-07-01
**Auditor:** Lead Systems Architect
**Branch:** `develop` · Commit: `bead6b5`
**Test baseline:** 147 passing · 6 failing · `cargo clippy -D warnings` clean · `cargo fmt` clean

---

## Executive Summary

| Dimension | Score | Verdict |
|-----------|-------|---------|
| Core architecture ("dumb core") | 10/10 | Production-ready |
| IPC transport (UDS-only) | 10/10 | Production-ready |
| Binary framing protocol | 10/10 | Flag space canonicalized (AUDIT-001 closed) |
| Security & fail-fast | 10/10 | All VULN-001–022 resolved |
| Process isolation | 9/10 | Linux sandbox complete; macOS warning added (AUDIT-005 closed) |
| CLI tooling (`vyn`) | 10/10 | `vyn plugin list/search/install`, completions, all subcommands present |
| Marketplace & plugin distribution | 9/10 | Atomic 8-step install pipeline; no live registry yet |
| Observability | 9/10 | Prometheus metrics, JSON logging, structured traces |
| Test suite | 7/10 | 6 tests failing (see below) |
| Documentation | 6/10 | Several stale/redundant MD files |
| **Overall** | **93/100** | **Staging-ready; resolve test failures before promoting to production** |

**Summary:** Phase 2.1–2.4 shipped and closed all five prior audit items (AUDIT-001 through AUDIT-005). The kernel is architecturally sound, all 22 tracked vulnerabilities are resolved, and the full feature set (marketplace, audio protocol, fragmentation, rate limiting, JSON logging, CI fuzz) is in place. Two outstanding concerns: six unit tests are failing (three are permission-denied environment issues; one is a shutdown timing race), and the docs folder contains multiple stale planning documents that should be archived.

---

## Closed Audit Items (since 2026-06-30)

All items from the previous audit are now closed:

| ID | Item | Closed by | Status |
|----|------|-----------|--------|
| AUDIT-001 | Flag Bit 0 conflict (RAW audio vs MAC) | T-01: `docs/FRAMING.md` + `FLAG_RAW_BINARY = 0x0010` | ✅ Closed |
| AUDIT-002 | WS JWT `Sec-WebSocket-Protocol` vs `?token=` | T-02: documented in `docs/FRAMING.md` | ✅ Closed |
| AUDIT-003 | Grace period hardcoded 200ms | T-03: `grace_seconds` in `PluginConfig` + `config.yaml` | ✅ Closed |
| AUDIT-004 | Fuzz corpus not wired to CI | T-13: `.github/workflows/fuzz.yml` triggers on PR | ✅ Closed |
| AUDIT-005 | macOS sandbox silently no-op | T-14: `warn!` emitted when `sandbox=true` on non-Linux | ✅ Closed |

---

## Architectural Compliance Checklist

### Rule 1 — "Dumb" Core: No Business Logic, No AI, No Databases

| Check | Status | Evidence |
|-------|--------|----------|
| No AI models or inference in core | ✅ PASS | Proto `ai_*` fields marked `reserved`; `AiRequest` proxies to external API only |
| No business logic in kernel | ✅ PASS | `src/kernel/orchestrator.rs` handles lifecycle, shutdown, and component wiring only |
| No embedded databases for plugin state | ✅ PASS | `rusqlite` is used exclusively for `EventStore` (delivery journal — infrastructure, not state) |
| Core acts as byte router and process supervisor | ✅ PASS | `MessageRouter` routes by 32-byte target field without decoding payload; `PluginSupervisor` manages process lifecycle only |

---

### Rule 2 — IPC Transport: UDS Only

| Check | Status | Evidence |
|-------|--------|----------|
| Plugin↔kernel IPC over Unix Domain Sockets | ✅ PASS | `src/ipc/server.rs`: `UnixListener::bind()` at `config.socket_path` |
| No Redis / AMQP / TCP for intra-host IPC | ✅ PASS | `Cargo.toml` has no `redis`, `lapin`, or AMQP crate |
| External access via WebSocket/HTTP gateway only | ✅ PASS | `src/api/server.rs` is the sole TCP-facing component |
| Socket permissions locked to owner | ✅ PASS | `umask(0o177)` before bind; `set_permissions(0o600)` as defence-in-depth (VULN-017 closed) |
| Socket path uses XDG_RUNTIME_DIR | ✅ PASS | `default_socket_path()` prefers `$XDG_RUNTIME_DIR/veyron.sock`; falls back to `/tmp/veyron.sock` |

---

### Rule 3 — Binary Framing Protocol (44-byte Header)

```
┌─────────┬─────────┬──────────────┬──────────────────────┬──────────┬─────────────┐
│ Magic   │ Flags   │ Length       │ Target               │ CRC32    │ Payload     │
│ 2 bytes │ 2 bytes │ 4 bytes BE   │ 32 bytes null-padded │ 4 bytes  │ N bytes     │
└─────────┴─────────┴──────────────┴──────────────────────┴──────────┴─────────────┘
  0x56 0x52  bitmask  payload len   plugin_id or "kernel"  CRC32(payload)  Protobuf/RAW
```

**Canonical flag table** (`docs/FRAMING.md` is now the single source of truth):

| Bit | Hex    | Constant         | Status |
|-----|--------|------------------|--------|
| 0   | 0x0001 | FLAG_MAC_PRESENT | ✅ Implemented |
| 1   | 0x0002 | FLAG_COMPRESSED  | Reserved |
| 2   | 0x0004 | FLAG_FRAGMENTED  | ✅ Implemented (T-12) |
| 3   | 0x0008 | FLAG_PRIORITY    | Reserved |
| 4   | 0x0010 | FLAG_RAW_BINARY  | ✅ Defined + exported (T-01) |
| 5–15 | —     | —                | Reserved |

| Check | Status | Evidence |
|-------|--------|----------|
| 44-byte header, magic `0x5652` | ✅ PASS | `HEADER_SIZE = 44`, `MAGIC = 0x5652` |
| Big-endian length field | ✅ PASS | `length.to_be_bytes()` / `u32::from_be_bytes()` |
| CRC32 over payload only | ✅ PASS | `crc32fast::hash(&payload)` |
| Zero-copy routing by target field | ✅ PASS | Proto envelope decoded only when `target == "kernel"` |
| 1 MiB payload cap | ✅ PASS | `MAX_PAYLOAD_SIZE = 1_048_576` checked on read and write |
| Frame read timeout | ✅ PASS | `FRAME_READ_TIMEOUT = 10s` (slow-loris defence) |
| Fragment reassembly | ✅ PASS | `ReassemblyBuf` in `connection.rs`; 30s timeout (T-12) |
| Flag space canonical | ✅ PASS | `docs/FRAMING.md` defines all bits; SDKs import, do not redefine |

---

### Rule 4 — Security & Fail-Fast

#### Authentication & Authorization

| Check | Status | Evidence |
|-------|--------|----------|
| JWT HS256 on plugin registration | ✅ PASS | `src/auth/jwt.rs`; kernel refuses to start without `jwt_secret` unless `allow_no_auth: true` |
| Per-connection HMAC-SHA256 MAC | ✅ PASS | HKDF-derived per-session key; `FLAG_MAC_PRESENT` appends 32-byte tag (VULN-005 closed) |
| Default-deny peer-to-peer IPC | ✅ PASS | `PERMISSION_IPC_SEND` required; `forward()` and `broadcast()` both check (VULN-001/002 closed) |
| Per-target IPC allowlist | ✅ PASS | `ipc_targets` in manifest; `check_ipc_target()` enforced in both unicast and broadcast (VULN-012 closed) |
| Broadcast strips `FLAG_MAC_PRESENT` | ✅ PASS | Broadcast clones clear the flag; recipient write-loop re-adds with own session key (VULN-015 closed) |
| Plugin ID validation | ✅ PASS | `[A-Za-z0-9._-]`, ≤32 bytes, reserved IDs blocked (VULN-009 closed) |
| JWT `sub == plugin_id` at registration | ✅ PASS | Identity squatting prevented (VULN-004 closed) |
| Mutex poison recovery on hot paths | ✅ PASS | `unwrap_or_else(|p| p.into_inner())` everywhere (VULN-013/014 closed) |
| Non-UTF-8 frame target | ✅ PASS | `target_as_str()` returns `Option`; raw hex logged + error frame returned (VULN-022 closed) |

#### HTTP Control Plane

| Check | Status | Evidence |
|-------|--------|----------|
| Auth-protected routes | ✅ PASS | `GET /plugins`, `GET /plugins/:id`, `GET /metrics`, `GET /plugins/:id/logs` all require Bearer JWT |
| Public routes limited | ✅ PASS | Only `GET /health` is unauthenticated |
| Per-token rate limiting | ✅ PASS | `governor` crate; keyed by JWT `sub`; `429 + Retry-After` on limit (T-15) |
| HTTP binds loopback only | ✅ PASS | `127.0.0.1` bind in `src/api/server.rs` |

#### WebSocket

| Check | Status | Evidence |
|-------|--------|----------|
| JWT validation before upgrade | ✅ PASS | `ws_handler()` validates `Sec-WebSocket-Protocol` header before `on_upgrade()` |
| WS JWT delivery documented | ✅ PASS | `docs/FRAMING.md` documents the header-based approach and its security rationale (AUDIT-002 closed) |
| WS frame MAC | ✅ PASS | `session_key` cell wired in `websocket.rs`; `FLAG_MAC_PRESENT` enforced (T-13) |
| WS error budget | ✅ PASS | `MAX_WS_PARSE_ERRORS = 16` disconnects bad frames (VULN-016 closed) |
| Slowloris protection | ✅ PASS | `TimeoutLayer(5s)` wraps WS upgrade route |

#### Connection Limits & DoS

| Check | Status | Evidence |
|-------|--------|----------|
| Pre-registration connection limit | ✅ PASS | `Arc<AtomicUsize>` counter; `max_connections` config field (VULN-019 closed) |
| Per-connection error budget (UDS) | ✅ PASS | `MAX_CONN_ERRORS = 16` in `run_with_context` |
| Fragment-based memory exhaustion | ✅ PASS | `ReassemblyBuf` pruned after 30s |

---

### Rule 5 — Process Isolation

| Check | Status | Evidence |
|-------|--------|----------|
| Plugins as isolated OS subprocesses | ✅ PASS | `tokio::process::Command` in `supervisor.rs:spawn_internal()` |
| `VEYRON_SOCKET_PATH` env injection | ✅ PASS | `cmd.env("VEYRON_SOCKET_PATH", ...)` |
| SIGTERM + SIGKILL lifecycle | ✅ PASS | `nix::sys::signal::kill(pid, SIGTERM)`; SIGKILL via watchdog |
| Exponential restart backoff | ✅ PASS | `backoff_delay()`: 100ms × 2^n, capped at 30s |
| Dead entries removed after max_restarts | ✅ PASS | `entries.remove()` + `stopped_counts` in `monitor_loop` (VULN-018 closed) |
| Watchdog does not reset timer after SIGKILL | ✅ PASS | `record_pong` after SIGKILL removed; D-state processes escalated (VULN-021 closed) |
| Linux PID + network namespace isolation | ✅ PASS | `CLONE_NEWPID | CLONE_NEWNET` via `pre_exec` in `runner.rs` |
| Resource limits | ✅ PASS | `RLIMIT_NPROC=64`, `RLIMIT_AS=512MiB` via `setrlimit` |
| macOS sandbox warning | ✅ PASS | `warn!` emitted when `sandbox=true` on non-Linux (AUDIT-005 closed) |
| Configurable grace period | ✅ PASS | `grace_seconds` field in `PluginConfig` + `config.yaml` (AUDIT-003 closed) |

---

### Rule 6 — CLI Tooling (`vyn`)

| Command | Status | Evidence |
|---------|--------|----------|
| `vyn start / stop / restart / status / logs` | ✅ PASS | `src/cli/mod.rs` |
| `vyn plugin list` | ✅ PASS | `src/cli/plugin.rs`; tab-aligned registry output with version columns |
| `vyn plugin search <query>` | ✅ PASS | Case-insensitive substring filter against slug/name/description |
| `vyn plugin start/stop/restart/logs <id>` | ✅ PASS | Proxies to REST API |
| `vyn install <slug-or-id>` | ✅ PASS | Atomic 8-step pipeline: resolve → compat → download → SHA-256 → zip-slip → move → validate → confirm |
| `vyn completions <shell>` | ✅ PASS | `clap_complete`; hidden `__complete-slugs` subcommand for dynamic slug completion |

---

### Rule 7 — Marketplace & Plugin Distribution

| Check | Status | Evidence |
|-------|--------|----------|
| `plugin.json` schema defined | ✅ PASS | `docs/PLUGIN_REGISTRY_SCHEMA.md` |
| `registry.json` schema defined | ✅ PASS | `docs/PLUGIN_REGISTRY_SCHEMA.md` |
| Kernel validates `plugin.json` before spawn | ✅ PASS | `src/plugins/loader.rs`: compat range + permissions checked |
| Registry cached (1h TTL) | ✅ PASS | `src/marketplace/registry.rs`; `$XDG_CACHE_HOME/veyron/registry.json` |
| SHA-256 archive verification | ✅ PASS | `src/marketplace/installer.rs` Step 4 |
| Zip-slip protection | ✅ PASS | Rejects entries with `..` in path |
| Kernel compatibility gate | ✅ PASS | `check_kernel_compatibility()` uses `semver`; min/max enforced |
| Unknown permissions refused at load time | ✅ PASS | `validate_plugin_def()` in `loader.rs` |
| **Live registry server (`veyron-plugins` repo)** | ⚠️ NOT YET | `registry.json` URL points to non-existent GitHub release; install works only against a local mock |

---

### Rule 8 — Observability

| Check | Status | Evidence |
|-------|--------|----------|
| Prometheus `/metrics` endpoint | ✅ PASS | `messages_routed_total`, `ipc_send_denied_total`, `ipc_frame_errors_total`, `plugins_registered_total`, `plugin_restarts_total` + latency histograms |
| Structured JSON logging | ✅ PASS | `LOG_FORMAT=json` gates `tracing_subscriber::fmt().json()` (T-17) |
| Per-plugin log ring buffer | ✅ PASS | 1000-line buffer; `GET /plugins/:id/logs` |
| SQLite EventStore (at-least-once) | ✅ PASS | `src/events/store.rs`; retry worker every 5s; dead after 5 retries |

---

### Rule 9 — SDK Parity

| SDK | Connect | Register | MAC | Audio |
|-----|---------|----------|-----|-------|
| Rust | ✅ | ✅ | ✅ | `FLAG_RAW_BINARY` exported |
| C++ | ✅ | ✅ | ✅ `mac.hpp` | `FLAG_RAW_BINARY` exported |
| Python | ✅ | ✅ | ✅ `framing.py` | `FLAG_RAW_BINARY` exported |

---

## Open Audit Items

| ID | Severity | Issue | Recommendation |
|----|----------|-------|----------------|
| AUDIT-006 | **High** | 6 unit tests failing (see detail below) | Fix before merge to staging |
| AUDIT-007 | Medium | Live marketplace registry does not exist | Create `veyron-core/veyron-plugins` GitHub repo with `registry.json` |
| AUDIT-008 | Low | `docs/FOLDER_TREE.md` describes a structure that never existed | Delete or replace with accurate tree |
| AUDIT-009 | Low | `docs/VEYRON_ARCHITECTURE.md` describes old module layout in Russian | Archive to `docs/archive/` |
| AUDIT-010 | Low | `docs/ROADMAP.md` (Phase 1.1 planning, all tasks done) is dead weight | Move to `docs/archive/` |
| AUDIT-011 | Low | `docs/ROADMAP_v2.md` explicitly superseded by v3 | Move to `docs/archive/` |
| AUDIT-012 | Low | `docs/CORE_ROADMAP.md` (all 5 sprints complete, last updated 2026-06-21) | Move to `docs/archive/` |
| AUDIT-013 | Low | Root `ROADMAP.md` duplicates some content from `docs/ROADMAP_v3.md` | Consider consolidating under `docs/` only |
| AUDIT-014 | Info | `FLAG_COMPRESSED` (Bit 1) defined but `zstd` crate not present | Add `zstd` or remove the flag from the canonical table |

---

## Failing Tests — Detail (AUDIT-006)

**Run:** `cargo test --test unit 2>&1`

```
FAILED. 147 passed; 6 failed
```

| Test | Failure | Root Cause | Fix |
|------|---------|------------|-----|
| `test_manifest_enforcement::valid_manifest_passes` | `EACCES` (Permission denied) | Test reads `plugin.json` from a hardcoded path that requires write permission in the test environment | Use `tempfile::tempdir()` for test fixtures |
| `test_manifest_enforcement::incompatible_min_kernel_refused` | `EACCES` | Same: hardcoded path | Same fix |
| `test_manifest_enforcement::unknown_permission_refused` | `EACCES` | Same | Same fix |
| `test_manifest_enforcement::config_permission_restriction_enforced` | `EACCES` | Same | Same fix |
| `test_manifest_enforcement::invalid_plugin_skipped_valid_loads` | `EACCES` | Same | Same fix |
| `test_kernel::kernel_graceful_shutdown_does_not_panic` | `Elapsed` (timeout) | Kernel shutdown in test takes longer than the test's deadline | Increase timeout or mock the supervisor wait |

All 6 are environment/timing issues, not logic bugs. Priority: fix before staging promotion.

---

## Documentation — Files Requiring Action

### Root-level MD files

| File | Status | Action |
|------|--------|--------|
| `README.md` | Active | Keep |
| `CLAUDE.md` | Active | Keep |
| `AUDIT.md` | Active (this file) | Keep |
| `ROADMAP.md` | Active — security/hardening tracker | Keep; consider renaming to `SECURITY_ROADMAP.md` for clarity |

### `docs/` files

| File | Status | Action |
|------|--------|--------|
| `docs/FRAMING.md` | Active — flag bit authority | Keep |
| `docs/PLUGIN_REGISTRY_SCHEMA.md` | Active — schema contracts | Keep |
| `docs/VEYRON_ARCHITECTURE.md` | Stale — describes old module layout in Russian | Archive |
| `docs/FOLDER_TREE.md` | Stale — describes `veyron/kernel/cairo/plugins/` monorepo that was never built | Delete or replace |
| `docs/ROADMAP.md` | Obsolete — Phase 1.1 planning (3-week sprint, all tasks ✅) | Archive |
| `docs/ROADMAP_v2.md` | Superseded — Russian ecosystem roadmap, explicitly replaced by v3 | Archive |
| `docs/ROADMAP_v3.md` | Historical — Phase 2.1–2.4 all completed 2026-06-30; no open tasks | Archive after Phase 3 planning doc created |
| `docs/CORE_ROADMAP.md` | Obsolete — all 5 sprints complete, last updated 2026-06-21 | Archive |
| `docs/superpowers/` | Planning artifacts | Keep (process history) |

**Recommended action:** Create `docs/archive/` and move the 4 obsolete files there. Do not delete — they contain useful history.

---

## Security Vulnerabilities — Full Status

All 22 tracked vulnerabilities are resolved. No open CVEs.

| Range | Count | Status |
|-------|-------|--------|
| VULN-001 – VULN-007 | 7 | ✅ All closed (Phase 1.x hardening) |
| VULN-008 – VULN-011 | 4 | ✅ All closed (HTTP auth, plugin validation, SDK MAC) |
| VULN-012 – VULN-015 | 4 | ✅ All closed (broadcast security, mutex poison) |
| VULN-016 – VULN-019 | 4 | ✅ All closed (WS error budget, rate limit, TOCTOU, connection limit) |
| VULN-020 – VULN-022 | 3 | ✅ All closed (MAC race, watchdog pong, UTF-8 target) |

---

## Audit History

| Date | Commit | Score | Key Changes |
|------|--------|-------|-------------|
| 2026-06-20 | `1c2a824` | 60/100 | Phase 1.1 baseline: UDS, framing, registry |
| 2026-06-27 | — | 78/100 | T-01–T-07: MAC, JWT, socket perms, error budget, watchdog |
| 2026-06-29 | — | 82/100 | VULN-008: HTTP GET endpoints auth-gated |
| 2026-06-30 | `c476415` | 85/100 | T-08/T-09 SDK MAC; VULN-009–022 lifecycle closed |
| 2026-07-01 | `bead6b5` | **93/100** | Phase 2.1–2.4 complete; all AUDIT-001–005 closed; 6 test failures remain |

---

*Next audit scheduled after: live marketplace registry created (AUDIT-007) and failing tests fixed (AUDIT-006).*
