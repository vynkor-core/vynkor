# Veyron Kernel — Architectural Audit

**Date:** 2026-06-30
**Auditor:** Lead Systems Architect
**Branch:** `develop` · Commit: `c476415`
**Test baseline:** All tests passing · `cargo clippy -D warnings` clean · `cargo fmt` clean

---

## Executive Summary

| Dimension | Score | Verdict |
|-----------|-------|---------|
| Core architecture ("dumb core") | 10/10 | Production-ready |
| IPC transport (UDS-only) | 10/10 | Production-ready |
| Binary framing protocol | 8/10 | One flag conflict |
| Security & fail-fast | 7/10 | Two spec gaps |
| Process isolation | 9/10 | Linux sandbox complete; macOS no-op |
| CLI tooling (`vyn`) | 7/10 | No install/marketplace commands |
| **Overall MVP readiness** | **85/100** | **Pre-production** |

**Summary:** The kernel core is architecturally sound and substantially compliant with the Veyron Manifesto. The IPC framing, routing, registry, supervision, and auth subsystems are implemented and hardened through multiple security audit cycles (VULN-001 through VULN-022 tracked and resolved). Two meaningful specification gaps remain: (1) Flag Bit 0 semantics conflict between the manifesto (RAW audio) and the implementation (MAC present), and (2) WebSocket JWT delivery uses the `Sec-WebSocket-Protocol` header rather than the mandated `?token=` URL query parameter. Neither blocks a dev-to-staging promotion, but both must be resolved before the system can claim full manifesto compliance.

---

## Architectural Compliance Checklist

### Rule 1 — "Dumb" Core: No Business Logic, No AI, No Databases

| Check | Status | Evidence |
|-------|--------|----------|
| No AI models or inference in core | ✅ PASS | Reserved proto fields `ai_request/response/ai_stream_chunk` are marked `reserved` — they will never ship in core |
| No business logic in kernel | ✅ PASS | `src/kernel/orchestrator.rs` handles only lifecycle, shutdown, and component wiring |
| No embedded databases for plugin state | ✅ PASS | `rusqlite` dep is used exclusively for `EventStore` (at-least-once delivery journal for system events — infrastructure, not business logic) |
| Core acts as byte router and process supervisor only | ✅ PASS | `MessageRouter` routes by 32-byte target field without touching payload content; `PluginSupervisor` manages only process lifecycle |

**Verdict:** Fully compliant. The `reserved` proto fields prove forward intent to keep AI concerns out of the core.

---

### Rule 2 — IPC Transport: UDS Only, No TCP/Redis/RabbitMQ

| Check | Status | Evidence |
|-------|--------|----------|
| Plugin↔kernel IPC over Unix Domain Sockets | ✅ PASS | `src/ipc/server.rs`: `UnixListener::bind()` · socket bound at `config.socket_path` |
| No Redis dependency | ✅ PASS | `Cargo.toml` has no `redis` crate |
| No RabbitMQ / AMQP dependency | ✅ PASS | No `amqp`, `lapin`, or `rabbitmq` deps |
| No TCP used for intra-host IPC | ✅ PASS | `tokio::net::UnixListener` — no `TcpListener` for plugin comms |
| External access via WebSocket/HTTP gateway only | ✅ PASS | `src/api/server.rs` (Axum) is the sole TCP-facing component; plugins never receive TCP connections directly |
| Socket permissions locked to owner | ✅ PASS | `0o600` set atomically via umask before `bind()`, then hardened with `set_permissions()` |

**Verdict:** Fully compliant. The WebSocket gateway is correctly positioned as the external-to-internal boundary; intra-host traffic stays on UDS.

---

### Rule 3 — Binary Framing Protocol (44-byte Header)

#### Frame Layout

```
┌─────────┬─────────┬──────────────┬──────────────────────┬──────────┬─────────────┐
│ Magic   │ Flags   │ Length       │ Target               │ CRC32    │ Payload     │
│ 2 bytes │ 2 bytes │ 4 bytes BE   │ 32 bytes null-padded │ 4 bytes  │ N bytes     │
└─────────┴─────────┴──────────────┴──────────────────────┴──────────┴─────────────┘
  0x56 0x52  bitmask  payload len   plugin_id or "kernel"  CRC32(payload)  Protobuf/RAW
  Total header: 44 bytes
```

| Check | Status | Evidence |
|-------|--------|----------|
| 44-byte header structure | ✅ PASS | `src/ipc/framing.rs:HEADER_SIZE = 44` · `serialize_header()` packs exactly 2+2+4+32+4 bytes |
| Magic `0x5652` ("VR") | ✅ PASS | `const MAGIC: u16 = 0x5652` · rejected immediately on mismatch |
| Big-Endian length field | ✅ PASS | `length.to_be_bytes()` / `u32::from_be_bytes()` |
| 32-byte null-padded target | ✅ PASS | Target copied with `copy_from_slice`, remainder zeroed by `[0u8; 32]` default |
| CRC32 over payload only | ✅ PASS | `crc32fast::hash(&payload)` · mismatch returns `FrameCrcMismatch` error |
| Zero-parsing routing by target field | ✅ PASS | `MessageRouter` switches on `target_as_str()` — proto envelope never decoded until `target == "kernel"` |
| Payload size enforcement (1 MiB cap) | ✅ PASS | `MAX_PAYLOAD_SIZE = 1_048_576` checked before read and before write |
| Frame read timeout (slow-loris defense) | ✅ PASS | `FRAME_READ_TIMEOUT = 10s` applied after first byte received |
| **Flag Bit 0 — RAW binary stream (PCM/Audio)** | ⚠️ **CONFLICT** | Manifesto: Bit 0 = RAW audio stream. Implementation: `FLAG_MAC_PRESENT = 0x0001` (Bit 0 = HMAC tag appended). These are mutually exclusive definitions on the same bit. |

**Flag Bit 0 Conflict — Detail:**

The manifesto specifies that `Flag Bit 0 = 1` signals a RAW binary payload (PCM audio), enabling the router to skip Protobuf parsing for low-latency audio streams. The current implementation repurposes Bit 0 as `FLAG_MAC_PRESENT`, indicating a 32-byte HMAC-SHA256 tag is appended after the payload.

The MAC feature is a net security gain and should be preserved. Resolution requires assigning audio RAW to a different bit (e.g., Bit 4, currently unused) and updating the spec.

**Roadmap flag definitions** (`ROADMAP_v2.md`) further diverge — they define COMPRESSED/FRAGMENTED/PRIORITY/ACK_REQUIRED, none of which are implemented. These are aspirational and do not constitute current violations, but the flag space needs formal canonicalization.

---

### Rule 4 — Security & Fail-Fast

#### 4a. WebSocket JWT Validation

| Check | Status | Evidence |
|-------|--------|----------|
| JWT validated before WebSocket upgrade | ✅ PASS | `ws_handler()` validates token in `axum::extract::ws::WebSocketUpgrade` handler before calling `on_upgrade()` |
| **JWT delivered via URL query param `?token=`** | ❌ **GAP** | Manifesto mandates `?token=...`. Implementation uses `Sec-WebSocket-Protocol: veyron, <jwt>` header. |

**Note on the deviation:** The header-based approach is actually superior security practice — tokens in URL query strings appear in server access logs, browser history, and proxy logs. The manifesto's `?token=` spec should be treated as a documentation error; the implementation is correct. If strict spec compliance is required for third-party client compatibility, this needs a protocol decision (not a security fix).

#### 4b. Marketplace Plugin Validation

| Check | Status | Evidence |
|-------|--------|----------|
| Plugin `.zip` archive hashed before install | ❌ **MISSING** | No marketplace install logic exists anywhere in the codebase |
| Plugin manifest (`plugin.json`) integrity check | ❌ **MISSING** | `src/plugins/loader.rs` loads plugins from `config.yaml` entries; no archive validation |
| `vyn install` command | ❌ **MISSING** | CLI (`src/cli/mod.rs`) has `start/stop/restart/status/logs` — no `install` subcommand |

The entire marketplace/plugin distribution layer is unimplemented. This is a Phase 2+ feature per the roadmap but should be called out explicitly.

#### 4c. Namespace Isolation (core.* protection)

| Check | Status | Evidence |
|-------|--------|----------|
| Clients cannot address internal kernel namespaces | ✅ PASS | `validate_plugin_id()` blocks `"kernel"` and `"*"` as reserved IDs at registration |
| Permission model prevents unauthorized actions | ✅ PASS | `check_permission()` enforces capability manifest; default-deny for IPC send |
| Per-target IPC allowlist | ✅ PASS | `ipc_targets` field in manifest; empty = deny-all |

#### 4d. Additional Security Posture (beyond manifesto)

| Feature | Status | Notes |
|---------|--------|-------|
| HMAC-SHA256 frame MAC | ✅ Implemented | Per-session key via HKDF; `FLAG_MAC_PRESENT` (Bit 0) |
| Per-connection error budget (16 errors → throttle) | ✅ Implemented | `MAX_CONN_ERRORS = 16` in `protocol.rs` |
| UDS connection limit | ✅ Implemented | `max_connections` config; excess connections receive EOF |
| Plugin ID injection prevention | ✅ Implemented | `[A-Za-z0-9._-]` allowlist, ≤32 bytes enforced |
| One registration per connection | ✅ Implemented | Second `PluginRegister` on same conn_id is rejected |
| Watchdog SIGKILL for unresponsive plugins | ✅ Implemented | Heartbeat ping/pong; SIGKILL on timeout |
| Fuzz harness | ✅ Implemented | 3 libFuzzer targets in `fuzz/` |

---

### Rule 5 — Process Isolation

| Check | Status | Evidence |
|-------|--------|----------|
| Plugins run as isolated OS subprocesses | ✅ PASS | `tokio::process::Command` in `supervisor.rs:spawn_internal()` |
| `VEYRON_SOCKET_PATH` injected via env | ✅ PASS | `cmd.env("VEYRON_SOCKET_PATH", &self.socket_path)` |
| SIGTERM on `stop_plugin()` | ✅ PASS | `nix::sys::signal::kill(pid, Signal::SIGTERM)` |
| Zombie reaping | ✅ PASS | `child.wait().await` in tokio::spawn — kernel waits for exit status |
| Exponential restart backoff | ✅ PASS | `backoff_delay()`: 100ms × 2^n, capped at 30s |
| Linux PID + network namespace isolation | ✅ PASS | `runner.rs:sandbox_pre_exec()` calls `unshare(CLONE_NEWPID | CLONE_NEWNET)` |
| Resource limits (Linux) | ✅ PASS | `RLIMIT_NPROC=64`, `RLIMIT_AS=512MiB` via `setrlimit` |
| SIGTERM grace on kernel shutdown | ✅ PASS | `graceful_shutdown()` sends `PluginShutdown` proto then waits 200ms |
| **macOS sandbox** | ⚠️ NO-OP | `#[cfg(target_os = "linux")]` gate — macOS builds skip namespace isolation entirely. Acceptable for dev; document as limitation. |

**Verdict:** Compliant on Linux production targets. macOS is development-only.

---

### Rule 6 — CLI Manager `vyn`

| Check | Status | Evidence |
|-------|--------|----------|
| Binary named `vyn` | ✅ PASS | `Cargo.toml`: `[[bin]] name = "vyn"` |
| `vyn start` | ✅ PASS | Supports `--foreground`, `--port`, `--config`, `--debug`; daemonizes |
| `vyn stop` | ✅ PASS | SIGTERM → SIGKILL fallback; PID file cleanup |
| `vyn restart` | ✅ PASS | Waits for old PID to die before spawning new instance |
| `vyn status` | ✅ PASS | PID probe via `kill(pid, 0)` |
| `vyn logs` | ✅ PASS | Tails log file, configurable line count |
| **`vyn install <plugin>`** | ❌ **MISSING** | No install/uninstall/list-available commands |
| **`vyn plugin ls/start/stop/restart`** | ❌ **MISSING** | Plugin lifecycle control is REST-only (HTTP API); no CLI wrapping |

---

## Security Vulnerabilities — Current Status

All VULN-001 through VULN-022 have been tracked and resolved across audit cycles. The following table summarizes the threat model:

| VULN | Threat | Status |
|------|--------|--------|
| VULN-004 | Plugin ID squatting without JWT | Mitigated via `allow_no_auth` explicit opt-in |
| VULN-007 | Error amplification from misbehaving plugin | Fixed: per-conn error budget (16 errors → throttle) |
| VULN-017 | TOCTOU on socket permissions | Fixed: umask before bind, explicit chmod as defence-in-depth |
| VULN-018 | Dead plugin entry persists in registry after max restarts | Fixed: `stopped_counts` map + `entries.remove()` on exhaustion |
| VULN-020 | MAC verification active before ack reaches plugin | Fixed: `EnableMac` queued after ack write |
| VULN-021 | Watchdog resets pong timer on SIGKILL, D-state masking | Fixed: pong not reset — watchdog keeps SIGKILLing |
| VULN-022 | Non-UTF-8 frame target causes silent routing fail | Fixed: `target_as_str()` returns `Option`; logged + error frame returned |

**Open items (not yet filed as VULN):**

| ID | Issue | Severity | Recommendation |
|----|-------|----------|----------------|
| AUDIT-001 | Flag Bit 0 conflict: manifesto=RAW audio, impl=MAC present | Medium | Canonicalize flag spec; move audio flag to Bit 4 |
| AUDIT-002 | WS JWT via `Sec-WebSocket-Protocol` vs mandated `?token=` | Low | Document as intentional deviation; update manifesto |
| AUDIT-003 | `graceful_shutdown()` waits only 200ms for plugin ack | Low | Make grace period configurable via `PluginShutdown.grace_seconds` |
| AUDIT-004 | Fuzz corpus not wired to CI | Low | Add `cargo fuzz run` to CI pipeline |
| AUDIT-005 | macOS sandbox is a no-op — `sandbox: true` silently does nothing | Low | Log warning when `sandbox: true` on non-Linux; or return error |

---

## Missing Critical Components

### For Full MVP

| Component | Priority | Description |
|-----------|----------|-------------|
| Flag space canonicalization | **P0** | Bit 0 conflict blocks compliant audio streaming. Write `docs/FRAMING.md` as the single source of truth for all flag bits. |
| `vyn install` command | **P1** | Plugin distribution requires `vyn install <path\|url>` with archive hash validation before the marketplace story holds |
| Compression flag (Bit 1) | **P1** | COMPRESSED flag is defined in roadmap but `zstd` is not in Cargo.toml; large AI response payloads will saturate UDS buffers without it |
| Fragmentation (Bit 2) | **P2** | FRAGMENTED flag planned for large AI responses; no reassembly logic exists |
| Python/C++ SDK MAC support | **P2** | SDKs must implement `FLAG_MAC_PRESENT` frame tagging to inter-operate with auth-enabled kernels |
| `vyn plugin` subcommands | **P2** | `vyn plugin list`, `vyn plugin start <id>`, `vyn plugin stop <id>` — currently REST-only |
| Prometheus scrape endpoint docs | **P3** | `/metrics` exists; no documentation on what it exports |

### For Production Hardening

| Component | Priority | Description |
|-----------|----------|-------------|
| CI fuzz integration | **P2** | `cargo fuzz` is local-only; libFuzzer targets should run in CI on each PR |
| Rate limiting on HTTP API | **P2** | Auth middleware validates token but no per-IP or per-token rate limit on REST endpoints |
| Socket path in `/run/` or `XDG_RUNTIME_DIR` | **P3** | Default `/tmp/veyron.sock` is world-listable in directory listing; prefer `/run/user/<uid>/veyron.sock` |
| Structured log output | **P3** | `tracing-subscriber` is configured but JSON output not enabled; production deployments need machine-parseable logs |

---

## Actionable Fixes — Prioritized

### Immediate (blocks spec compliance)

**P0-A: Canonicalize flag bit space**
- Create `docs/FRAMING.md` with authoritative flag table
- Assign: Bit 0 = `FLAG_MAC_PRESENT` (keep current), Bit 4 = `FLAG_RAW_BINARY` (audio/PCM)
- Update `src/ipc/framing.rs` to export `FLAG_RAW_BINARY`
- Update all three SDK framing layers

**P0-B: Document WS JWT delivery deviation**
- Add note to `docs/FRAMING.md` or API docs explaining `Sec-WebSocket-Protocol` header choice
- Update manifesto or architecture doc to reflect the decision

### Short-term (unblocks next phase)

**P1-A: `vyn install` command**
- Add `Commands::Install { path: String }` to `src/cli/mod.rs`
- Compute SHA-256 of entire archive before extraction
- Validate `plugin.json` signature against kernel's trust store

**P1-B: `zstd` compression (Flag Bit 1)**
- Add `zstd = "0.13"` to `Cargo.toml`
- Implement compress/decompress in `framing.rs` gated on `FLAG_COMPRESSED`
- Required before audio or AI payloads exceed 1 MiB threshold in practice

### Medium-term (production readiness)

**P2-A: Fragmentation (Flag Bit 2)**
- Implement fragment header (sequence, total, fragment_id) packed inside payload
- Reassembly buffer in `ConnectionHandler` with timeout to prevent fragment-based DoS

**P2-B: `vyn plugin` subcommands**
- Wrap HTTP API calls: `vyn plugin list|start|stop|restart|logs <id>`

**P2-C: CI fuzz integration**
- Add GitHub Actions job: `cargo +nightly fuzz run fuzz_frame_parse -- -max_total_time=60`

**P2-D: macOS sandbox warning**
- In `spawn_internal()`, when `config.sandbox && !cfg!(target_os = "linux")`, emit `warn!("sandbox requested but not supported on this OS")` 

---

## Historical Audit Trail

| Date | Phase | Score | Key Events |
|------|-------|-------|------------|
| 2026-06-20 | Phase 1.1 | 60/100 | Initial UDS, framing, registry baseline |
| 2026-06-27 | Phase 1.2 | 78/100 | T-01 through T-07: MAC, JWT, socket perms, error budget, watchdog |
| 2026-06-29 | VULN-008 | 82/100 | HTTP GET endpoints auth-gated |
| 2026-06-30 | Current | **85/100** | T-08/T-09 SDK MAC, VULN-011 closed |

---

*This document supersedes all previous AUDIT.md versions. Next audit scheduled post-Phase 2.1 (plugin marketplace).*
