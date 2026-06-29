# Veyron Kernel — Phase 1.1 Audit

**Date:** 2026-06-20  
**Auditor:** Systems review, post-Phase 1.1 completion  
**Commit:** `cbd974c` (develop branch)  
**Test result:** 80/80 pass — `cargo clippy -D warnings` clean — `cargo fmt` clean

---

## Phase 1.2 Security Audit Update

**Date:** 2026-06-27  
**Commit:** `86abd56` (develop branch)  
**Test result:** All tests pass (includes MAC integration tests, stress test, fuzz harness) — clippy clean — fmt clean

### What Changed Since Phase 1.1

All seven hardening targets (T-01 through T-07) are complete. Ten tracked vulnerabilities
have been resolved or formally mitigated. This addendum records the current security
posture; the Phase 1.1 audit below remains as historical baseline.

### Updated Threat Model

| Threat | Mitigation | Remaining Gap |
|--------|-----------|---------------|
| Rogue process connects to socket | JWT mandatory when `jwt_secret` set; kernel refuses start without it unless `allow_no_auth: true` | `allow_no_auth` shifts trust to operator config |
| Plugin claims another plugin's ID | `claims.sub == plugin_id` check at registration; JWT must match declared ID | Without JWT (`allow_no_auth`), squatting is accepted risk (see VULN-004 note) |
| Message tampering in transit | Per-connection HMAC-SHA256 over header+payload; bad tag drops connection | WS gateway enforces MAC on par with UDS (T-13). C++ and Python SDKs now implement MAC (T-08/T-09, VULN-011 fixed). |
| Peer-to-peer IPC abuse | Default-deny `PERMISSION_IPC_SEND`; per-target `ipc_targets` allowlist in manifest | — |
| Broadcast abuse | `PERMISSION_IPC_SEND` required for `target = "*"` | — |
| HTTP API misuse | Bound to `127.0.0.1`; auth-protected route group for sensitive endpoints | JWT optional for basic health/list when no `jwt_secret` |
| Plugin ID injection into events | `validate_plugin_id()` enforces `[A-Za-z0-9._-]` ≤32 bytes, blocks reserved names | — |
| Error-spam flooding | Per-connection error budget (16 errors) throttles without disconnecting | — |
| Oversized / malformed frames | Size limit 1 MiB; magic + CRC32 + timeout validated | — |
| Plugin resource exhaustion | RLIMIT_NPROC (64) + RLIMIT_AS (512 MiB) on Linux for supervised plugins | Self-connected (non-supervised) plugins have no limits |
| Fuzz attack on frame/proto parser | 3 libFuzzer targets cover frame parse, envelope decode, router pipeline | Continuous fuzz not wired to CI yet |

### Authentication (Updated)

Fully implemented. `src/auth/jwt.rs` validates HS256 tokens. `src/main.rs:115–123`
(`ensure_auth_configured()`) refuses to start the kernel unless `jwt_secret` is set in
config or the operator explicitly acknowledges `allow_no_auth: true`. At registration,
`claims.sub` must equal the declared `plugin_id` — prevents identity spoofing.

**Accepted risk (VULN-004):** When `allow_no_auth: true` is set, any process can claim
any plugin ID. This is an explicit operator opt-out, not a silent default. The setting
should only be used in isolated development environments.

### Authorization (Updated)

RBAC fully enforced:
- `ActionRequest` — checked against declared permissions
- Unicast (`target = "<id>"`) — requires `PERMISSION_IPC_SEND`; further scoped by `ipc_targets` allowlist
- Broadcast (`target = "*"`) — requires `PERMISSION_IPC_SEND`
- `KernelCommand` / `AiRequest` — unimplemented handlers return `ErrUnknown` (safe default)

### Socket Security (Updated)

`src/ipc/server.rs:26` applies `fs::set_permissions(0o600)` after bind. No longer
depends on umask. HTTP API rebound to `127.0.0.1` — not reachable from external
interfaces by default.

### Integrity (Updated)

HMAC-SHA256 per-connection MAC active when `jwt_secret` configured. Key derived via
HKDF-SHA256 from the shared secret + a per-connection nonce minted by the kernel.
Constant-time verification at `src/auth/frame_mac.rs:36`. Bad tag closes connection.
CRC-32 retained as a framing sanity check independent of MAC.

### Audit Logging (Updated)

Permission denials, CRC/magic errors, oversized frames, MAC failures are all logged
via `tracing` at `warn` level with structured fields. Security-relevant events are
now observable. Gap: no dedicated security event sink (syslog, SIEM) — tracing output
only.

### T-07: Fuzz + Soak Harness (New)

Three libFuzzer targets in `fuzz/`:
- `fuzz_frame_parse` — arbitrary bytes into `read_frame()`
- `fuzz_envelope_decode` — arbitrary bytes into protobuf `Envelope::decode()`
- `fuzz_router_pipeline` — arbitrary frames through the full router

Seed corpus covers valid frames, truncated headers, wrong magic, CRC mismatches,
oversized payloads, reserved target names, and malformed protobuf. Soak test at
`tests/integration/test_soak.rs` runs 5 s in CI; set `VEYRON_SOAK_SECS=86400`
for 24-hour overnight runs.

**CI:** `.github/workflows/fuzz.yml` runs all three targets weekly (Monday 03:00 UTC),
60 s per target. Crashes uploaded as artifacts. Also added `.github/workflows/ci.yml`
for per-PR build + test + clippy + fmt.

### SDK MAC Gap (VULN-011) — Fixed

C++ SDK (`sdk/cpp/src/framing.cpp`) and Python SDK (`sdk/python/veyron/framing.py`)
previously implemented CRC-32 framing only. Both SDKs now implement HMAC-SHA256 MAC:

- **Python (T-08):** `derive_session_key`/`compute_tag`/`verify_tag` added to `framing.py`;
  `VeyronClient(socket_path, secret=bytes)` derives session key from `PluginRegisterAck.session_nonce`.
- **C++ (T-09):** `mac.hpp/cpp` with HKDF-SHA256 + HMAC-SHA256 via OpenSSL; `pack_frame_mac` and
  `read_frame_full` in `framing.cpp`; `VeyronClient(path, secret)` is MAC-transparent.

C++ and Python plugins can now connect to hardened kernels (`jwt_secret` configured) without
disabling MAC. `allow_no_auth: true` remains supported for no-secret environments.

### Current Vulnerability Summary

| ID | Status | Notes |
|----|--------|-------|
| VULN-001 | ✅ Fixed | Default-deny unicast IPC |
| VULN-002 | ✅ Fixed | Default-deny broadcast IPC |
| VULN-003 | ✅ Mitigated | Secure-by-default; `allow_no_auth` explicit opt-out |
| VULN-004 | ✅ Fixed / Accepted risk | JWT `sub==plugin_id`; squatting accepted when `allow_no_auth` |
| VULN-005 | ✅ Fixed | HMAC-SHA256 per-connection MAC |
| VULN-006 | ✅ Mitigated | Explicit `0o600` after bind |
| VULN-007 | ✅ Fixed | Error budget 16 throttles floods |
| VULN-008 | ◐ Mitigated | `127.0.0.1` binding; JWT optional |
| VULN-009 | ✅ Fixed | `validate_plugin_id()` blocks injection |
| VULN-010 | ✅ Fixed | `/logs` in auth-protected route group |
| VULN-011 | ✅ Fixed | Python/C++ SDK MAC added (T-08/T-09) |

---

---

## 1. Executive Summary

Veyron is a plugin kernel written in Rust: a long-running daemon that loads, supervises, and routes messages between independently-developed plugin processes using a binary-framed protobuf protocol over Unix domain sockets.

**Key capabilities:**
- Accepts plugin connections over UDS, registers plugins with declared permissions, routes messages unicast or broadcast
- Supervises plugin processes (spawn, crash detection, restart with configurable policy)
- Enforces RBAC on kernel action requests
- Broadcasts lifecycle events (plugin joined, left) via pub/sub event bus
- Exposes a REST control plane for health checks, plugin inspection, and process management
- Provides a Rust SDK that hides framing and serialization from plugin authors

**Maturity:** Alpha. Core IPC, routing, and registry work end-to-end. Several production requirements are unimplemented (auth, log capture, resource limits). Suitable for internal development and C++ SDK integration; not production-deployable without the gaps listed in Section 8.

**Phase 1.2 readiness:** Proceed with C++ SDK integration. Wire protocol is stable and correct. One caveat: the C++ SDK's client logic beyond framing is a stub; the framing layer itself is now complete and verified.

**Strengths:**
1. Clean async architecture — Tokio throughout, no blocking I/O on async threads, minimal locking (DashMap for registry/event bus)
2. Correct binary framing — 44-byte header, CRC-32/ISO-HDLC, 1 MiB size limit, fully tested
3. Good test discipline — 80 tests, integration tests start a real kernel, no test doubles for the critical path

**Limitations:**
1. No authentication — any process that can reach the socket can register and send messages as any plugin ID
2. Supervisor is wired but inert unless the kernel explicitly calls `spawn_plugin()` — plugins that self-connect are not supervised
3. Several Phase 1.1 acceptance criteria remain partially incomplete: `/plugin/{id}/logs`, Python SDK, WebSocket gateway

---

## 2. Architecture Overview

### Responsibilities

The kernel owns four concerns: lifecycle, transport, routing, and control.

**Lifecycle:** The `vyn` CLI spawns a background process (re-exec self with `--foreground`), writes a PID file, and the foreground process runs the async Tokio runtime. On `vyn stop`, SIGTERM is sent; if it doesn't exit within 5 seconds, SIGKILL follows. There is no POSIX double-fork daemonization; the child is a direct subprocess of the shell.

**Transport:** A `UnixListener` binds `/tmp/veyron.sock` (configurable). For each accepted connection, `ConnectionHandler` splits the stream into a read half (polling for frames) and a write half (draining an mpsc channel). The read loop forwards `IncomingMessage { conn_id, frame, write_tx }` to the router via a channel of capacity 1024.

**Routing:** `MessageRouter::run()` processes messages sequentially. Target is extracted from the 32-byte target field of each frame:
- `"kernel"` → handled internally (register, ping, subscribe, action_request)
- `"*"` → broadcast to all registered plugins except sender
- `<plugin_id>` → unicast forward

**Control:** `ApiServer` (Axum, `0.0.0.0:8000` or configured port) exposes REST endpoints for inspection and process management. `EventBus` delivers lifecycle events to subscribed plugins.

### ASCII Diagram

```
  vyn start
      │ re-exec --foreground
      ▼
┌─────────────────────────────────────────────────┐
│  Veyron Kernel (Tokio async runtime)            │
│                                                 │
│  UdsServer ──accept──► ConnectionHandler        │
│                              │ read loop        │
│                              ▼                  │
│                         MessageRouter ──────────┤
│                              │                  │
│                   ┌──────────┼──────────┐       │
│                   ▼          ▼          ▼       │
│             kernel msg   broadcast    unicast   │
│             handler      (all reg'd)  forward   │
│                   │                            │
│           PluginRegistry ◄──────────────────── │
│           (DashMap)                            │
│                   │                            │
│           EventBus ◄── publish on join/leave   │
│           (DashMap)                            │
│                   │                            │
│           PluginSupervisor (monitor_loop)      │
│                   │                            │
│           ApiServer (Axum :8000)               │
└─────────────────────────────────────────────────┘
     ▲ UDS 44-byte frames          ▲ UDS 44-byte frames
     │                             │
┌──────────────┐         ┌──────────────┐
│  Plugin A    │         │  Plugin B    │
│  (process)   │         │  (process)   │
│  (Rust SDK)  │         │  (C++ SDK)   │
└──────────────┘         └──────────────┘
```

### Message Flow: Plugin A → Plugin B

```
1. Plugin A: pack Envelope { target: "plugin-b", payload: ActionRequest{...} }
2. Plugin A: protobuf-encode → veyron_crc32 → write 44-byte header + payload
3. Kernel: read_frame() → validate magic + CRC32
4. Kernel: parse target from header bytes [8..40]
5. Kernel: registry.get("plugin-b") → get write_tx
6. Kernel: forward frame to plugin-b's write channel
7. Plugin B: read_frame() → validate CRC32 → Envelope::decode()
8. Plugin B: dispatch to on_message()
```

---

## 3. Capability Inventory

### A. Daemon & Lifecycle

**Start:** `vyn start` forks a subprocess running the same binary with `--foreground`, redirecting its stdout+stderr to `cfg.log_file`. The PID is written to `cfg.pid_file`. Running while a PID file exists (and the process is alive via SIGCONT probe) is rejected.

**Stop:** `vyn stop` reads the PID, sends SIGTERM, polls every 500ms up to 5 seconds, then SIGKILL. PID file is removed.

**Restart:** Sequential stop + start. Not atomic.

**Status:** `vyn status` prints PID or "not running".

**Logs:** `vyn logs [--lines N]` tails the kernel log file (default: last 20 lines). This is kernel logs only — plugin stdout is separate (see Section 8).

**Foreground mode:** `vyn start --foreground` — skips fork, runs in terminal. Useful for development. Responds to Ctrl+C (SIGINT via `tokio::signal::ctrl_c()`). No explicit SIGTERM handler in foreground mode; SIGTERM will kill the process without graceful shutdown.

**Graceful shutdown:** On Ctrl+C, kernel broadcasts `PluginShutdown { reason: "kernel shutdown", grace_seconds: 5 }` to all registered plugins over their write channels, then waits 200ms before exiting. This is a best-effort notification; the kernel does not wait for plugin acknowledgment.

**Configuration** (`config.yaml`):

| Field | Default | Description |
|-------|---------|-------------|
| `port` | 8000 | HTTP API port |
| `log_level` | `"info"` | Log verbosity |
| `pid_file` | `/tmp/veyron.pid` | PID file path |
| `log_file` | `/tmp/veyron.log` | Kernel + plugin log output |
| `data_dir` | `/var/lib/veyron` | Data directory (unused in Phase 1.1) |
| `socket_path` | `/tmp/veyron.sock` | UDS path (serde default, not in yaml) |

CLI overrides: `--port PORT`, `--debug` (sets log_level=debug), `--config FILE`.  
Environment: `RUST_LOG=<filter>` controls tracing output. `VEYRON_SOCKET_PATH` is read by the Rust SDK.

### B. Plugin System

**Registration handshake:**
1. Plugin connects to UDS
2. Sends `PluginRegister { plugin_id, version, manifest: PluginManifest { permissions, actions, events, needs_ai, needs_gpu, priority } }`
3. Kernel stores in registry, sends `PluginRegisterAck { accepted, reject_reason, granted_permissions }`
4. On success, publishes `system.plugin_joined` event

`granted_permissions` in the ack now mirrors the permissions declared in the manifest (Phase 1.1 fix). The kernel does not currently negotiate or downgrade permissions; it accepts what the plugin declares.

**Registration rejection:** Only if the same `plugin_id` is already registered. No other rejection criteria exist (no capability validation, no allowlist check).

**Lifecycle states:** `Registered` only. States `Connected` and `Shuttingdown` were removed as dead code; lifecycle transitions are implicit in registry presence.

**Supervisor (partially integrated):** `PluginSupervisor::new(socket_path)` is instantiated at kernel boot and its `monitor_loop` runs as a background task. The supervisor manages plugins it spawned via `spawn_plugin(config: PluginConfig)`. Plugins that self-connect (not spawned by the kernel) are not in the supervisor's process table and cannot be restarted via the HTTP API. No plugins are auto-spawned by the kernel in Phase 1.1 — the supervisor is wired but idle until callers use `spawn_plugin`.

**Restart policies:** `Always`, `OnFailure`, `Never`. Max restart count enforced per plugin. No exponential backoff — restart is immediate on process exit.

**Resource limits:** None. No CPU, memory, or open-file limits are applied to supervised plugin processes.

### C. IPC & Messaging

**Transport:** Unix domain socket. File path configurable, default `/tmp/veyron.sock`. Stale socket files are removed on startup. Socket file permissions are whatever `umask` produces — no explicit mode is set.

**Frame format** (44 bytes, all multi-byte fields big-endian):

```
Bytes  0– 1:  magic    u16 BE  = 0x5652
Bytes  2– 3:  flags    u16 BE  = 0x0000 (reserved)
Bytes  4– 7:  length   u32 BE  = payload byte count
Bytes  8–39:  target   [u8;32] = null-padded destination id
Bytes 40–43:  crc32    u32 BE  = CRC-32/ISO-HDLC of payload
Bytes 44+:    payload          = protobuf-encoded Envelope
```

**Size limit:** 1 MiB (`MAX_PAYLOAD_SIZE = 1_048_576`). Enforced on both read and write. Frames exceeding the limit are rejected before allocation.

**Integrity:** CRC-32/ISO-HDLC (IEEE 802.3 polynomial, same as zlib/crc32fast) is computed on the payload and validated on every received frame. Mismatches return `VeyronError::FrameCrcMismatch` and drop the frame; the connection is not closed.

**Message types** (from `veyron_protocol.proto`):

| Type | Direction | Handled in Phase 1.1 |
|------|-----------|----------------------|
| `PluginRegister` | Plugin → Kernel | ✅ Full |
| `PluginRegisterAck` | Kernel → Plugin | ✅ Full |
| `PluginShutdown` | Kernel → Plugin | ✅ Broadcast on shutdown |
| `ActionRequest` | Plugin → Kernel | ✅ Permission-checked |
| `ActionResponse` | Kernel → Plugin | ✅ Returned |
| `KernelCommand` | Kernel → Plugin | ❌ Not dispatched (falls to `_` arm) |
| `KernelCommandAck` | Plugin → Kernel | ❌ Not handled |
| `Event` | Kernel → Plugin | ✅ Via event bus |
| `EventAck` | Plugin → Kernel | ❌ Not handled |
| `Subscribe` | Plugin → Kernel | ✅ Full |
| `Unsubscribe` | Plugin → Kernel | ✅ Full |
| `AiRequest` | Plugin → Kernel | ❌ Returns ErrUnknown |
| `AiResponse` | Kernel → Plugin | ❌ Not dispatched |
| `AiStreamChunk` | Kernel → Plugin | ❌ Not dispatched |
| `Ping` | Plugin → Kernel | ✅ Real timestamp |
| `Pong` | Kernel → Plugin | ✅ Both timestamps |
| `ErrorMessage` | Kernel → Plugin | ✅ Sent on errors |

**Routing:**
- `target = "kernel"` — handled by `handle_kernel_message()`
- `target = "*"` — forwarded to all registered plugins (sender excluded)
- `target = "<id>"` — forwarded directly to that plugin's write channel; returns error if not found
- Unregistered connections sending non-register messages receive `ErrNotRegistered`

**Channel backpressure:** Router input channel capacity is 1024. Per-plugin write channel capacity is 64. If a plugin's write channel is full, `send().await` blocks the router — a slow plugin blocks the broadcast path for all other plugins.

### D. Permissions & Security

**Model:** Declaration-based RBAC. Plugins declare permissions in their manifest at registration. The kernel stores the declared permissions. When a plugin sends an `ActionRequest`, the kernel checks whether the sender's registered permissions include the required permission for that action.

**Permission types** (`PermissionType` enum):

| Permission | Actions |
|-----------|---------|
| `PERMISSION_NETWORK` | `http_get`, `http_post`, `http_put`, `http_delete`, `http_patch` |
| `PERMISSION_FILES_READ` | `read_file`, `list_dir` |
| `PERMISSION_FILES_WRITE` | `write_file`, `delete_file` |
| `PERMISSION_SYSTEM` | `get_cpu`, `get_memory`, `get_disk` |
| `PERMISSION_AUDIO` | `play_audio`, `record_audio` |
| `PERMISSION_NOTIFY` | `send_notification` |
| `PERMISSION_AI` | `ai_complete`, `ai_embed` |
| `PERMISSION_SCHEDULER` | `set_timer`, `create_alarm` |
| `PERMISSION_BROWSER` | `browser_navigate`, `browser_screenshot` |

**Enforcement scope:** Only `ActionRequest` messages are permission-checked. Unicast and broadcast messages between plugins are not permission-checked. A plugin with no permissions can still send arbitrary `Envelope` payloads to other plugins.

**Authentication:** None. Any process that can connect to the socket can claim any `plugin_id`. The kernel does not verify process identity. `src/auth/jwt.rs` is an empty file — JWT authentication is deferred to Phase 1.2.

### E. Event Bus

**Events emitted by kernel:**
- `system.plugin_joined` — on successful registration, payload: `{"plugin_id": "<id>"}`
- `system.plugin_left` — on disconnect, payload: `{"plugin_id": "<id>"}`

**No other kernel-originated events.** No `plugin_crashed`, `plugin_restarted`, `kernel_shutdown_warning` events are emitted. Supervisor crash detection does not publish to the event bus.

**Subscriptions:** Plugins subscribe via `Subscribe { event_types: [...] }`. Use `"*"` for all events. Multiple event types per subscribe call. `Unsubscribe` removes specific types. On plugin disconnect, all subscriptions are removed.

**Delivery:** Fire-and-forget. If a subscriber's write channel is full, the event is dropped silently. No retry, no queue, no `EventAck` processing.

### F. HTTP Control Plane

**Binding:** `0.0.0.0:<port>` (port from config, default 8000). Not restricted to localhost — exposed on all interfaces.

**Endpoints:**

| Method | Path | Response | Notes |
|--------|------|----------|-------|
| `GET` | `/health` | `{"status":"ok"}` 200 | Always 200 while process is alive |
| `GET` | `/plugins` | JSON array | All registered plugins |
| `GET` | `/plugins/:id` | JSON object | 404 if not found |
| `POST` | `/plugins/:id/stop` | 200 / 404 | Removes from registry only; no SIGTERM sent |
| `POST` | `/plugins/:id/restart` | 202 / 404 / 422 | 422 if plugin not in supervisor's process table |

**Plugin JSON fields:** `plugin_id`, `state` (always `"Registered"`), `registered_at` (Unix timestamp seconds), `permissions` (string array).

**Missing endpoints:** `GET /plugin/{id}/logs` is not implemented. Plugin stdout/stderr is not captured by the kernel.

**No authentication** on the HTTP API. No CORS headers. No TLS.

**Caveat on `/stop`:** The stop endpoint removes the plugin from the registry but does not send SIGTERM to the process. The plugin process continues running; it will receive a connection error when it next tries to read/write, which will cause it to exit (if the SDK error path calls shutdown). This is not a clean stop.

### G. SDKs

**Rust SDK** (`sdk/rust/`): Complete. Exports `VeyronClient` and `Plugin` trait.

```rust
// Minimal plugin in ~20 lines
struct MyPlugin;
impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }
    fn manifest(&self) -> PluginManifest { PluginManifest::default() }
    async fn on_init(&mut self, _c: &mut VeyronClient) -> Result<(), VeyronError> { Ok(()) }
    async fn on_message(&mut self, _e: Envelope) -> Result<Option<Envelope>, VeyronError> { Ok(None) }
    async fn on_shutdown(&mut self) -> Result<(), VeyronError> { Ok(()) }
}
// MyPlugin.run().await — connects, registers, message loop, shutdown
```

**C++ SDK** (`sdk/cpp/`): Framing complete. `pack_frame(target, payload)`, `read_frame(fd)`, `veyron_crc32()` implemented. Wire format matches Rust exactly (44-byte header, big-endian, CRC-32/ISO-HDLC). `client.hpp` stub exists; connection logic and plugin lifecycle are not implemented.

**Python SDK** (`sdk/python/`): Empty. All three files (`framing.py`, `client.py`, `plugin.py`) contain no code.

### H. Logging & Monitoring

**Framework:** `tracing` crate with `tracing-subscriber`. Structured output with target, file, and line number. Respects `RUST_LOG` environment variable.

**Log output:** In daemon mode, kernel stdout+stderr redirect to `cfg.log_file`. Plugin stdout+stderr inherit from parent (kernel), so they also write to the same log file, unstructured and intermixed with kernel logs.

**Logged events:**
- UDS server start (info)
- Per-connection accept (not logged — gap)
- Plugin registration success/failure (not logged — gap)
- Disconnect handling (not logged — gap)
- HTTP API start (info)
- Kernel shutdown start/end (info)
- HTTP API errors (error)

**Not logged:** Message routing decisions, permission denials, CRC errors, oversized frames, individual message sends. The IPC layer, router, registry, event bus, and auth modules have zero log statements.

**Metrics:** None. No Prometheus, no counters, no histograms.

---

## 4. Wire Protocol Reference

### Frame Layout

```
 0               1               2               3
 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7 0 1 2 3 4 5 6 7
├───────────────────────────────┼───────────────────────────────┤
│         magic (0x5652)        │      flags (0x0000)           │ bytes 0-3
├───────────────────────────────────────────────────────────────┤
│                payload length (u32 BE)                        │ bytes 4-7
├───────────────────────────────────────────────────────────────┤
│                                                               │
│                target (32 bytes, null-padded)                 │ bytes 8-39
│                                                               │
├───────────────────────────────────────────────────────────────┤
│                CRC-32/ISO-HDLC of payload (u32 BE)            │ bytes 40-43
├───────────────────────────────────────────────────────────────┤
│                payload (protobuf Envelope, variable)          │ bytes 44+
└───────────────────────────────────────────────────────────────┘
```

**Note on magic:** Magic is `0x5652` ("VR"), not `0x5659` ("VY") as some documentation suggests. The proto and routing code are in agreement; the bytes spell "VR" on the wire.

### Registration Sequence

```
Plugin                              Kernel
  │                                    │
  │── connect() ──────────────────────►│
  │                                    │ ConnectionHandler spawned
  │── PluginRegister ─────────────────►│
  │   { plugin_id: "weather"           │ registry.register(...)
  │     manifest: { permissions:       │ event_bus.publish(system.plugin_joined)
  │       ["PERMISSION_NETWORK"] } }   │
  │                                    │
  │◄── PluginRegisterAck ──────────────│
  │    { accepted: true                │
  │      granted_permissions:          │
  │        ["PERMISSION_NETWORK"] }    │
  │                                    │
  │── Subscribe ──────────────────────►│
  │   { event_types: ["*"] }           │
  │                                    │
  │ [registered and active]            │
```

### Unicast Message Flow

```
Plugin A                  Kernel                    Plugin B
   │                         │                         │
   │── Envelope ────────────►│                         │
   │   target: "plugin-b"    │ registry.get("plugin-b")│
   │   payload: ActionReq{}  │ ──forward──────────────►│
   │                         │                         │
```

### Error Codes

| Code | Value | Meaning |
|------|-------|---------|
| `ERR_UNKNOWN` | 0 | Unhandled message type |
| `ERR_PROTOCOL_MISMATCH` | 1 | Protocol version mismatch |
| `ERR_NOT_REGISTERED` | 2 | Non-register message from unregistered connection |
| `ERR_RATE_LIMITED` | 3 | Not implemented |
| `ERR_INTERNAL` | 4 | Kernel internal error |
| `ERR_DESERIALIZATION` | 5 | Protobuf decode failure |

---

## 5. Security Assessment

### Threat Model

| Threat | Mitigation | Gaps |
|--------|-----------|------|
| Rogue process connects to socket | None — any process can connect | No auth |
| Plugin claims another plugin's ID | Rejected (duplicate ID check in registry) | But before the legit plugin connects, attacker can register first |
| Message tampering in transit | CRC-32 detects corruption | Not cryptographic; no MAC |
| Plugin sends to another without permission | Only checked for `ActionRequest`; peer-to-peer messages unchecked | Medium risk |
| HTTP API misuse | No auth | Accessible on all interfaces |
| Plugin exhausts resources | None | No CPU/memory limits |
| Malformed protobuf | `Envelope::decode` returns error; `ErrDeserialization` sent | Connection not closed; plugin can spam errors |
| Oversized frame | Rejected before allocation (>1 MiB) | Correct |

### Authentication

None. `src/auth/jwt.rs` is empty. Plugin identity is the `plugin_id` string they provide at registration. A process that connects first with ID `"admin"` will succeed; a legitimate admin plugin arriving later will be rejected with `PluginAlreadyRegistered`.

**Risk:** In a trusted local environment (single machine, controlled processes), this is acceptable for Phase 1.1. For any multi-tenant or production deployment, authentication is a prerequisite.

### Authorization

RBAC is enforced for `ActionRequest` messages only. The 18 mapped actions across 9 permission types cover the expected Phase 1 action surface. Gaps:

- Peer-to-peer messages (`target = "<plugin_id>"`) are not permission-checked. Any registered plugin can send any message to any other plugin.
- Broadcast messages (`target = "*"`) are not permission-checked.
- `KernelCommand`, `AiRequest` fall to unhandled arms and return `ErrUnknown`; they are not dispatched but also not explicitly denied.

### Socket Security

The UDS socket is created with default umask permissions. On Linux this typically produces `srwxrwxrwx` — readable and writable by all users. No `chmod` is applied after binding. Any local user can connect. For restricted environments, set `umask 0077` before starting the kernel or `chmod 0600 /tmp/veyron.sock` after startup.

### Integrity

CRC-32/ISO-HDLC detects accidental corruption. It provides no protection against deliberate tampering: CRC is not a cryptographic MAC, and an attacker who can intercept the socket can compute valid CRC32 for any payload.

### Audit Logging

Permission denials are not logged. CRC errors are not logged. Message routing decisions are not logged. The only evidence of security-relevant events is in `tracing` output at `info` level for kernel start/stop. This is insufficient for any security-sensitive deployment.

### HTTP API Exposure

The API binds `0.0.0.0` — it is accessible from any network interface, not just localhost. No authentication, no rate limiting, no CORS headers. On a multi-homed machine this is a real exposure.

---

## 6. Performance Characteristics

### What We Know

- **Frame parsing:** `crc32fast` uses hardware CRC32 instructions (SSE4.2/ARM CRC32). Per-frame overhead is in the nanosecond range.
- **Router channel capacity:** 1024 pending messages. At queue saturation, back-pressure propagates to the UDS read loop (TCP-style flow control via tokio channel back-pressure).
- **Per-plugin write channel capacity:** 64 frames. A slow plugin blocks broadcasts that include it.
- **Broadcast cost:** O(n) per registered plugin — one `DashMap::get` + one channel send per recipient. Under high plugin counts, broadcasts are the bottleneck.
- **Ping RTT:** Integration tests assert RTT < 1 second; actual observed RTT is sub-millisecond (UDS + in-process routing with no syscall besides socket read/write).

### What We Don't Know

No benchmarks exist. The following are unknown:
- Maximum frames/second through the router under sustained load
- Latency percentiles (P50, P99, P999) for unicast and broadcast
- Memory footprint scaling with plugin count
- Throughput degradation with slow subscribers (backpressure behavior)
- CPU usage at idle vs. under load

### Known Performance Risks

1. **Single-threaded router:** `MessageRouter::run()` processes one message at a time. All routing is sequential. CPU-bound routing (e.g., 10K messages/sec from 100 plugins) will queue.
2. **Broadcast blocks router:** If any plugin's write channel is full (capacity 64), the broadcast call to `send().await` blocks the entire router until that channel drains or the send times out (it doesn't — `let _ = tx.send(frame).await` is `.await` without a timeout).
3. **No message timeout:** There is no deadline on message delivery. A plugin that never reads its write channel will fill the channel (capacity 64) and then block the router for all future broadcasts involving it.

---

## 7. Test Coverage

### Summary

| Suite | Count | Coverage Area |
|-------|-------|--------------|
| Unit: framing | 7 | Round-trip, CRC mismatch, magic mismatch, oversized, target padding |
| Unit: router | 7 | Unicast, broadcast, kernel register, ping, permission deny, unknown target, unregistered sender |
| Unit: registry | 8 | CRUD, thread safety, dual-index, timestamps |
| Unit: supervisor | 6 | Spawn, stop, restart policies (Always/OnFailure/Never), max retries, env inheritance |
| Unit: event bus | 6 | Subscribe, unsubscribe, wildcard, multi-subscriber, empty subscriber |
| Unit: API | 8 | All HTTP endpoints including new restart (404) and restart-422 |
| Unit: permissions | 6 | Permission grant/deny, action mapping, unknown action, unknown plugin |
| Unit: kernel | 3 | Start+register, ping-pong, graceful shutdown |
| Unit: SDK | 5 | Connect, register, ping, send-recv, subscribe |
| Unit: server | 5 | Accept, unique conn IDs, stale socket cleanup, disconnect detection, bidirectional frame |
| Unit: errors/proto | 7 | Error variants, Display, conversions, proto encode/decode |
| Integration | 8 | Registration, ping, routing (2 plugins), events, wildcard sub, disconnect (2 scenarios) |
| **Total** | **80** | |

### What Is NOT Tested

| Gap | Risk |
|-----|------|
| Concurrent registration races (two plugins, same ID, simultaneous) | Medium — DashMap `contains_key` + `insert` is not atomic |
| Router under sustained load (1K+ messages/sec) | Unknown throughput ceiling |
| 100+ simultaneous plugin connections | Unknown scaling behavior |
| Malformed protobuf (valid frame, invalid proto payload) | Low — prost handles gracefully, but error path not tested |
| Oversized frame mid-stream (partial header, then disconnect) | Low — `read_exact` will return Io error |
| Broadcast with one slow subscriber (channel-full back-pressure) | Medium — router may stall |
| Signal handling (SIGTERM in foreground mode, SIGHUP) | No signal handling beyond Ctrl+C |
| Plugin crash during registration (after connect, before `PluginRegister`) | Registry and disconnect_loop interaction untested |
| Long-running stability (24-hour soak) | Unknown — no soak tests |
| Fuzz (arbitrary bytes as frame payload) | No fuzz harness |
| HTTP API under concurrent requests | Unknown — Axum handles concurrency, but shared state not stress tested |

---

## 8. Known Limitations

### By Design

- **Process isolation is the only isolation model.** Plugins are OS processes. There is no in-process plugin model. This is intentional but means each plugin costs a process spawn.
- **No shared memory.** All communication is via UDS. No mmap, no shared ring buffers.
- **No message guarantees.** The event bus is fire-and-forget. Messages to a full write channel are dropped.
- **Single router thread.** All message routing is sequential through one async task.

### Not Implemented (Phase 1.1 scope)

| Feature | File | Status |
|---------|------|--------|
| JWT authentication | `src/auth/jwt.rs` | Empty file |
| WebSocket gateway | `src/api/websocket.rs` | Empty file |
| HTTP middleware | `src/api/middleware.rs` | Empty file |
| Plugin loader (discovery) | `src/plugins/loader.rs` | Empty file |
| Plugin manager | `src/plugins/manager.rs` | Empty file |
| Plugin runner | `src/plugins/runner.rs` | Empty file |
| Python SDK | `sdk/python/` | Empty files |
| Plugin log capture | — | Not architected |
| `GET /plugin/{id}/logs` | — | Route not registered |
| Resource limits (CPU/mem) | `PluginSupervisor` | No limit fields in `PluginConfig` |
| Exponential backoff on restart | `monitor_loop` | Immediate restart |
| SIGTERM handler (foreground) | `orchestrator.rs` | Only Ctrl+C handled |
| POSIX daemonization (setsid) | `main.rs` | Simple subprocess fork |

### Operational Constraints

- **Socket path** defaults to `/tmp/veyron.sock`. Overridable in config; not via CLI flag.
- **HTTP port** defaults to 8000 but config.yaml example uses 8080 — inconsistency between binary default and example config.
- **HTTP binding** is `0.0.0.0` — not restricted to loopback.
- **PID file** is not locked (no `flock`). Concurrent `vyn start` calls can race.
- **Log rotation** not supported. `cfg.log_file` is opened with `File::create` on each start (truncates on restart).
- **`data_dir`** config field is parsed but never used.
- **Envelope fields** `message_id`, `timestamp`, `sender_id`, `version` are always `Default::default()` in kernel-generated messages (all zero/empty). Plugins cannot correlate kernel responses by `message_id`.

---

## 9. Operational Runbook

### Starting

```bash
# Daemon mode (default)
vyn start

# Foreground mode (development)
vyn start --foreground

# Custom port and debug logging
vyn start --port 9090 --debug

# Custom config file
vyn start --config /etc/veyron/config.yaml

# Check status
vyn status
# → veyron is running (PID: 12345)

# Tail logs
vyn logs --lines 50
```

### Health Monitoring

```bash
# Basic health check
curl http://localhost:8000/health
# → {"status":"ok"}

# List registered plugins
curl http://localhost:8000/plugins | jq .
# → [{"plugin_id":"weather","state":"Registered","registered_at":1750000000,"permissions":["PERMISSION_NETWORK"]}]

# Get specific plugin
curl http://localhost:8000/plugins/weather | jq .

# Restart a supervised plugin
curl -X POST http://localhost:8000/plugins/weather/restart
# → 202 Accepted (if spawned by kernel via supervisor)
# → 422 Unprocessable Entity (if self-connected, not supervisor-managed)

# Remove plugin from registry (does NOT stop process)
curl -X POST http://localhost:8000/plugins/weather/stop
# → 200 OK
```

### Stopping

```bash
vyn stop
# Sends SIGTERM, waits up to 5s, then SIGKILL

# Manual stop
kill -TERM $(cat /tmp/veyron.pid)
```

### Troubleshooting

| Symptom | Check |
|---------|-------|
| Plugin won't register | Socket path correct? Kernel running? `vyn status`. Duplicate ID? `GET /plugins`. |
| Messages not delivered | Target ID spelled correctly? Plugin registered? `GET /plugins`. |
| `POST /restart` returns 422 | Plugin self-connected; not supervisor-managed. Must be spawned via `spawn_plugin()`. |
| Kernel exits immediately | Check log file. Config parse error? Port in use? |
| Broadcast stops working | Slow plugin may have filled its write channel (cap 64). `POST /plugins/<slow-id>/stop`. |
| High kernel CPU | Check plugin count and message rate. Single router thread is the ceiling. |
| Stale socket warning on start | Previous kernel died without cleanup. Kernel auto-removes stale socket on start. |

### Plugin Development Quickstart (Rust)

```rust
// Cargo.toml: veyron-sdk = { path = "sdk/rust" }

struct EchoPlugin;
impl Plugin for EchoPlugin {
    fn id(&self) -> &str { "echo" }
    fn manifest(&self) -> PluginManifest {
        PluginManifest { permissions: vec!["PERMISSION_NETWORK".into()], ..Default::default() }
    }
    async fn on_init(&mut self, _c: &mut VeyronClient) -> Result<(), VeyronError> { Ok(()) }
    async fn on_message(&mut self, e: Envelope) -> Result<Option<Envelope>, VeyronError> {
        // echo back to sender
        Ok(Some(e))
    }
    async fn on_shutdown(&mut self) -> Result<(), VeyronError> { Ok(()) }
}

#[tokio::main]
async fn main() { EchoPlugin.run().await.unwrap(); }
```

```bash
VEYRON_SOCKET_PATH=/tmp/veyron.sock cargo run
```

---

## 10. Recommendations for Phase 1.2+

### Critical (before any production workload)

**1. JWT authentication.**  
Every plugin must present a signed token at registration. The kernel verifies the signature and extracts the plugin's allowed permissions from the token claims rather than trusting what the plugin declares. File `src/auth/jwt.rs` is empty — this is the highest-priority gap.

**2. HTTP API restricted to loopback.**  
Change `0.0.0.0` to `127.0.0.1` in `ApiServer::run()`. The control plane should not be network-accessible by default.

**3. Add router timeout for slow subscribers.**  
Change `let _ = entry.write_tx.send(frame).await` in the broadcast path to `tokio::time::timeout(Duration::from_millis(50), ...)`. Slow plugins should not stall the router.

**4. Socket file permissions.**  
After `UnixListener::bind()`, call `fs::set_permissions(socket_path, Permissions::from_mode(0o600))`. Without this, any local user can connect.

### High Priority (Phase 1.2)

**5. Python SDK.**  
`sdk/python/` is entirely empty. Python is the most common plugin language. Complete `framing.py` (struct.pack with big-endian layout), `client.py` (connect/register/send/recv/subscribe), and `plugin.py` (Plugin base class mirroring the Rust SDK).

**6. Plugin log capture.**  
Modify `PluginSupervisor::spawn_internal()` to set `.stdout(Stdio::piped()).stderr(Stdio::piped())` on the `Command`. Store captured output in a ring buffer (e.g., last 1000 lines per plugin). Implement `GET /plugins/:id/logs` endpoint.

**7. Exponential backoff in supervisor.**  
`monitor_loop` currently restarts immediately. Add backoff: `min(2^restart_count * 100ms, 30s)`. Prevents a crashing plugin from spam-restarting and generating noise.

**8. Populate Envelope metadata.**  
`message_id`, `timestamp`, `sender_id`, `version` are always empty in kernel-generated messages. Plugins need `message_id` for RPC correlation. Generate a UUID for `message_id` and fill `timestamp` with `SystemTime::now()` in `send_envelope()`.

### Medium Priority

**9. SIGTERM handler in foreground mode.**  
`tokio::signal::unix::signal(SignalKind::terminate())` alongside `ctrl_c()` in `run_with_shutdown`. Currently, `kill -TERM <pid>` in foreground mode kills the process without graceful shutdown.

**10. Performance benchmarks.**  
Add a `benches/` directory with Criterion benchmarks: frame encode/decode throughput, unicast latency P50/P99, broadcast latency vs. plugin count. Required before claiming any throughput guarantees.

**11. Peer-to-peer permission model.**  
Currently any plugin can send any message to any other plugin. Add a permission type `PERMISSION_IPC_SEND` and check it in the `forward()` path. Or define per-plugin allowlists in the manifest.

**12. PID file locking.**  
Use `flock(LOCK_EX | LOCK_NB)` on the PID file to prevent concurrent starts.

### Low Priority / Phase 2+

**13. WebSocket gateway** — real-time event streaming to browser/dashboard clients.  
**14. Plugin resource limits** — `ulimit` or cgroup-based CPU/memory constraints per supervised plugin.  
**15. KernelCommand dispatch** — implement handlers for `reload_config`, `health_check` so plugins can receive commands from the kernel.  
**16. EventAck + at-least-once delivery** — track unacknowledged events, retry up to N times, publish to dead-letter topic.  
**17. Prometheus metrics endpoint** — `/metrics` with counters for messages routed, plugins registered, errors by type.  
**18. `data_dir` usage** — currently parsed from config but unused; Phase 2 event store (SQLite) should use it.  

---

## Appendix: File Inventory

| File | Status | Notes |
|------|--------|-------|
| `src/main.rs` | ✅ Complete | CLI, daemon fork, PID management |
| `src/kernel/orchestrator.rs` | ✅ Complete | Kernel bootstrap, supervisor wiring |
| `src/kernel/config.rs` | ✅ Complete | YAML config, defaults |
| `src/kernel/signals.rs` | ❌ Empty | Signal handling stub |
| `src/ipc/server.rs` | ✅ Complete | UDS listener, connection accept |
| `src/ipc/connection.rs` | ✅ Complete | Per-connection read/write loops |
| `src/ipc/framing.rs` | ✅ Complete | 44-byte frame, CRC32, 1MiB limit |
| `src/ipc/protocol.rs` | ✅ Complete | Message router, dispatch |
| `src/ipc/messages.rs` | ✅ Complete | IncomingMessage type |
| `src/plugins/registry.rs` | ✅ Complete | DashMap dual-index registry |
| `src/plugins/supervisor.rs` | ✅ Complete | spawn/stop/restart/monitor_loop |
| `src/plugins/loader.rs` | ❌ Empty | Plugin discovery stub |
| `src/plugins/manager.rs` | ❌ Empty | Plugin command handler stub |
| `src/plugins/runner.rs` | ❌ Empty | Plugin execution context stub |
| `src/auth/permissions.rs` | ✅ Complete | RBAC check, action→permission map |
| `src/auth/jwt.rs` | ❌ Empty | JWT auth stub |
| `src/events/bus.rs` | ✅ Complete | Pub/sub event delivery |
| `src/api/server.rs` | ✅ Complete | Axum setup, AppState |
| `src/api/routes.rs` | ✅ Complete | All 5 REST endpoints |
| `src/api/middleware.rs` | ❌ Empty | Middleware stub |
| `src/api/websocket.rs` | ❌ Empty | WebSocket stub |
| `src/utils/errors.rs` | ✅ Complete | VeyronError enum |
| `src/utils/config.rs` | ✅ Complete | Config struct, YAML loader |
| `src/utils/logging.rs` | ✅ Complete | tracing-subscriber init |
| `sdk/rust/src/client.rs` | ✅ Complete | VeyronClient |
| `sdk/rust/src/plugin.rs` | ✅ Complete | Plugin trait |
| `sdk/cpp/include/veyron/framing.hpp` | ✅ Complete | Wire format declarations |
| `sdk/cpp/src/framing.cpp` | ✅ Complete | pack_frame, read_frame, veyron_crc32 |
| `sdk/cpp/include/veyron/client.hpp` | ⚠️ Stub | Uses framing; no connection logic |
| `sdk/cpp/include/veyron/plugin.hpp` | ❌ Empty | C++ plugin base class stub |
| `sdk/python/` | ❌ Empty | All files empty |
| `proto/veyron_protocol.proto` | ✅ Complete | All message types defined |
