# Veyron Core — Implementation Roadmap

**Last updated:** 2026-06-20  
**Branch:** `develop` — commit `7cfdbbe`

This document tracks what is built, what is next, and what is later.
Effort estimates assume one developer familiar with the codebase.

---

## Current State (Phase 1 complete)

### Done ✅

| Area | What exists |
|------|-------------|
| Transport | UDS server, 44-byte binary framing, CRC-32/ISO-HDLC, 1 MiB limit |
| Router | Unicast, broadcast (`*`), kernel dispatch, 50ms broadcast timeout |
| Registry | DashMap dual-index (plugin_id + conn_id), registered_at timestamp |
| Supervisor | spawn/stop/restart, RestartPolicy (Always/OnFailure/Never), monitor_loop, env passthrough, plugin loader from config |
| Event bus | pub/sub, wildcard `"*"`, unsubscribe_all on disconnect |
| Permissions | RBAC for ActionRequest, 9 permission types, 18 mapped actions |
| Auth | JWT HS256 validation on registration (`jwt_secret` in config) |
| HTTP API | GET /health, GET /plugins, GET /plugins/:id, POST /plugins/:id/stop, POST /plugins/:id/restart |
| CLI | `vyn start/stop/restart/status/logs`, daemon fork, PID file, log file |
| Signals | SIGTERM + Ctrl-C both trigger graceful shutdown in foreground mode |
| Envelope | message_id, timestamp, sender_id stamped on all kernel responses |
| Security | Socket chmod 0o600, HTTP binds 127.0.0.1 only |
| Proto | Single-source `veyron_protocol.proto`, prost codegen, jwt_token field |
| Rust SDK | VeyronClient (connect/register/send/recv/subscribe/ping), Plugin trait |
| C++ SDK | Framing only (pack_frame, read_frame, CRC32) — client stub empty |
| Python SDK | **Empty** |
| Tests | 86 tests (78 unit + 8 integration), all passing |

### Known gaps

- No watchdog — hung plugins not detected
- No plugin log capture — plugin stdout/stderr lost
- Supervisor restart is immediate — no backoff
- HTTP API has no auth — loopback-only is not enough
- `KernelCommand` proto message unhandled (falls to `ErrUnknown`)
- `AiRequest` proto message unhandled
- C++ SDK has no connection or registration logic
- Python SDK is completely empty
- No metrics / observability
- No at-least-once event delivery (EventAck unhandled)
- PID file not locked (flock) — concurrent starts can race

---

## Phase 2 — Reliability (Priority: ship now)

Kernel must be able to run unattended. Plugged gaps that block production use.

---

### 2.1 Watchdog / Health Monitor

**Problem:** Hung plugin (process alive, not reading socket) goes undetected forever.  
**Solution:** Supervisor sends `Ping` to each plugin every N seconds. No `Pong` in M seconds → SIGKILL → restart.

**Files:**
- `src/plugins/supervisor.rs` — add `watchdog_loop()`: iterate entries, send Ping via write_tx, track last_pong timestamp
- `src/utils/config.rs` — add `watchdog_interval_secs: u64` (default 30), `watchdog_timeout_secs: u64` (default 10)
- `src/kernel/orchestrator.rs` — spawn `supervisor.watchdog_loop()` alongside `monitor_loop()`

**Proto:** use existing `Ping`/`Pong` messages. No proto changes.

**Acceptance:** spawn a plugin that reads but never responds to Ping → watchdog kills it after timeout → supervisor restarts it per policy.

**Effort:** 2–3 h

---

### 2.2 Plugin log capture + GET /plugins/:id/logs

**Problem:** Plugin stdout/stderr goes to kernel's log file unstructured; can't query per-plugin logs.  
**Solution:** Supervisor pipes each plugin's stdout+stderr into a per-plugin ring buffer (last 1000 lines). HTTP endpoint exposes it.

**Files:**
- `src/plugins/supervisor.rs` — `spawn_internal`: set `.stdout(Stdio::piped()).stderr(Stdio::piped())`, spawn async reader task, append to `Arc<Mutex<VecDeque<String>>>` per plugin_id
- `src/plugins/supervisor.rs` — add `get_logs(plugin_id, n) -> Vec<String>`
- `src/api/routes.rs` — add `GET /plugins/:id/logs?lines=N` handler
- `src/api/server.rs` — register the route

**Config:** `log_buffer_lines: usize` (default 1000)

**Acceptance:** plugin writes to stdout → `GET /plugins/myplugin/logs?lines=20` returns last 20 lines.

**Effort:** 2–3 h

---

### 2.3 Exponential backoff on supervisor restart

**Problem:** Plugin that crashes immediately gets restarted in a tight loop, hammering disk/CPU.  
**Solution:** `monitor_loop` waits `min(2^restart_count * 100ms, 30s)` before each restart.

**Files:**
- `src/plugins/supervisor.rs` — in the `Some(config)` arm of `monitor_loop`, add `tokio::time::sleep(backoff_delay(new_count)).await` before `spawn_internal`

```rust
fn backoff_delay(restart_count: u32) -> Duration {
    let ms = 100u64.saturating_mul(1u64 << restart_count.min(8));
    Duration::from_millis(ms.min(30_000))
}
```

**Acceptance:** plugin with `RestartPolicy::Always` that crashes immediately → restarts at 100ms, 200ms, 400ms … 30s cap.

**Effort:** 30 min

---

### 2.4 PID file locking (flock)

**Problem:** `vyn start` called twice in quick succession creates two daemon processes that fight over the socket.  
**Solution:** Use `fcntl LOCK_EX | LOCK_NB` on the PID file before writing. If lock fails, another instance is running — abort.

**Files:**
- `src/main.rs` — in `start_daemon` (foreground path): open PID file with `OpenOptions::write(true).create(true)`, call `nix::fcntl::flock(fd, FlockArg::LockExclusiveNonblock)` — error → print "already running" and exit 1

**Effort:** 30 min

---

### 2.5 HTTP API authentication middleware

**Problem:** control plane (`/plugins/:id/stop`, `/plugins/:id/restart`) is callable by any local process without auth.  
**Solution:** `Authorization: Bearer <jwt>` required on state-mutating endpoints. Read-only endpoints (`/health`, `GET /plugins`) remain open.

**Files:**
- `src/api/middleware.rs` — implement `auth_middleware`: extract `Authorization` header, validate with `JwtValidator`, reject with 401 if absent or invalid
- `src/api/server.rs` — apply middleware to POST routes only via Axum layer
- `src/utils/config.rs` — reuse existing `jwt_secret` field

**Effort:** 1–2 h

---

### 2.6 plugin_died event from supervisor

**Problem:** when a supervised plugin crashes, the event bus doesn't notify other plugins.  
**Solution:** `monitor_loop` publishes `Event { event_type: "system.plugin_died", payload_json: { plugin_id, restart_count, will_restart } }` after each exit.

**Files:**
- `src/plugins/supervisor.rs` — `PluginSupervisor::new` needs `Arc<EventBus>` + `Arc<PluginRegistry>` so `monitor_loop` can publish. Pass them in from orchestrator.
- `src/kernel/orchestrator.rs` — pass `Arc::clone(&event_bus)` and `Arc::clone(&registry)` to `PluginSupervisor::new`

**Note:** This is an API change to `PluginSupervisor::new`. Affects test_supervisor.rs (add bus/registry args to `PluginSupervisor::new` calls) and test_api.rs (`make_supervisor()`).

**Effort:** 1–2 h

---

### 2.7 KernelCommand dispatch

**Problem:** `KernelCommand` proto message falls to `ErrUnknown`. Plugins can't send commands to the kernel.  
**Solution:** dispatch two initial commands: `reload_config` (re-read config.yaml, apply log_level change) and `health_check` (return kernel uptime + plugin count).

**Files:**
- `src/ipc/protocol.rs` — add `Some(envelope::Payload::KernelCommand(cmd))` arm, dispatch by `cmd.command`
- `proto/veyron_protocol.proto` — already has `KernelCommand`/`KernelCommandAck` — no changes needed

**Effort:** 1–2 h

---

## Phase 3 — SDK Completion

### 3.1 Python SDK

**Files to implement from scratch:**
- `sdk/python/veyron/framing.py` — `pack_frame(target, payload) -> bytes`, `read_frame(sock) -> bytes` using `struct.pack(">HHI32sI")` for big-endian header, `crc32` from `binascii`
- `sdk/python/veyron/client.py` — `VeyronClient`: async with `asyncio.open_unix_connection`, `connect`, `register(plugin_id, manifest, jwt_token="")`, `send(target, envelope)`, `recv() -> Envelope`, `subscribe`, `ping`
- `sdk/python/veyron/plugin.py` — `Plugin` abstract base: `plugin_id`, `manifest`, `on_init`, `on_message`, `on_shutdown`, `run()`
- `sdk/python/pyproject.toml` — add `protobuf`, `grpcio-tools` for proto generation, or use hand-written `veyron_pb2.py`

**Proto generation:** add a `Makefile` or `scripts/gen_proto.py` that runs `python -m grpc_tools.protoc` on `proto/veyron_protocol.proto` → `sdk/python/veyron/veyron_pb2.py`.

**Tests:** `tests/python/` — at minimum: connect + register + ping round-trip against a running kernel.

**Effort:** 4–6 h

---

### 3.2 C++ SDK — connection and plugin lifecycle

Framing is done (`sdk/cpp/src/framing.cpp`). Missing: connection, register, receive loop.

**Files:**
- `sdk/cpp/src/client.cpp` — implement `VeyronClient`: connect via `AF_UNIX` socket, `pack_frame` + `write`, `read_frame` + protobuf decode using Protobuf C++ runtime
- `sdk/cpp/include/veyron/plugin.hpp` — abstract `Plugin` base with `on_init`, `on_message`, `on_shutdown`, `run()`
- `sdk/cpp/CMakeLists.txt` — link `protobuf::libprotobuf`, add `proto/veyron_protocol.proto` as generated target

**Effort:** 4–6 h

---

## Phase 4 — Observability

### 4.1 Structured logging coverage

Current state: only two `info!` calls in the whole IPC/routing stack. No logging for registration, disconnect, permission deny, CRC error, message routing, backpressure.

Add `tracing` spans/events to:
- `src/ipc/protocol.rs` — registration (accepted/rejected), permission denied, unknown target
- `src/ipc/connection.rs` — connect, disconnect, CRC error, oversized frame
- `src/plugins/supervisor.rs` — spawn, crash, restart, watchdog kill
- `src/events/bus.rs` — event dropped (slow subscriber)

**Effort:** 2 h

---

### 4.2 Prometheus metrics

**Files:**
- Add `metrics = "0.23"` + `metrics-exporter-prometheus = "0.14"` to `Cargo.toml`
- `src/api/server.rs` — add `GET /metrics` route
- Instrument: messages_routed_total (by type), plugins_registered_total, plugin_restarts_total, broadcast_timeouts_total, ipc_frame_errors_total, action_request_duration_ms histogram

**Effort:** 2–3 h

---

### 4.3 EventStore — at-least-once delivery

Current: events are fire-and-forget. A plugin that is slow or disconnected at the moment of delivery misses the event permanently.

**Solution:**
- `data_dir` (currently unused in Config) holds `events.db` (SQLite via `rusqlite`)
- On `event_bus.publish()`: persist to `events` table with status `pending`
- After delivery: if plugin acks (`EventAck` message), mark `delivered`
- Retry worker: every 5s re-sends `pending` events older than 10s, up to `max_retries` (default 5), then mark `dead` and log

**Files:**
- `src/events/store.rs` — new file, SQLite schema + CRUD
- `src/events/bus.rs` — wrap `publish()` to persist before deliver; handle `EventAck` in protocol.rs
- `src/ipc/protocol.rs` — add `Some(envelope::Payload::EventAck(ack))` arm

**Effort:** 4–6 h

---

## Phase 5 — External Access

### 5.1 WebSocket gateway

Per architecture: WebSocket is for external clients (browser, mobile) only. Internal IPC stays on UDS.

- `src/api/websocket.rs` — Axum WebSocket handler: JWT in `?token=` query param, 5s handshake timeout (Slowloris protection), translate WS frames ↔ UDS binary frames
- A WS client sends `PluginRegister` with `target="kernel"` via JSON or binary, gateway wraps it in a UDS frame and routes it

**Note:** WebSocket clients appear as "plugins" to the kernel with a synthetic `conn_id`. They participate in the same routing graph.

**Effort:** 4–6 h

---

### 5.2 Linux namespace sandboxing for untrusted plugins

For plugins not built in-house:
- `PluginConfig` gains `sandbox: bool`
- `spawn_internal` with sandbox: use `nix::unistd::clone` with `CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWNS` flags — plugin gets isolated PID namespace (can't signal other processes), no network access, read-only view of filesystem except its data dir

**Effort:** 4–8 h (Linux-specific, requires careful testing)

---

## Dependency Map

```
Phase 2.1 (watchdog)         → no deps, start anytime
Phase 2.2 (log capture)      → no deps, start anytime
Phase 2.3 (backoff)          → trivial, 30 min
Phase 2.4 (PID lock)         → trivial, 30 min
Phase 2.5 (HTTP auth)        → needs JWT already done ✅
Phase 2.6 (plugin_died)      → needs supervisor refactor (API change)
Phase 2.7 (KernelCommand)    → no deps

Phase 3.1 (Python SDK)       → no deps, parallelizable
Phase 3.2 (C++ client)       → framing done ✅, no other deps

Phase 4.1 (logging)          → no deps, do alongside any Phase 2 work
Phase 4.2 (Prometheus)       → no deps
Phase 4.3 (EventStore)       → needs data_dir wired + rusqlite

Phase 5.1 (WebSocket)        → needs JWT ✅, needs HTTP auth (2.5)
Phase 5.2 (namespaces)       → needs PluginConfig changes from 2.6
```

---

## Recommended Sprint Order

**Sprint 1 (this week) — make it reliable:**
1. 2.3 Backoff (30 min)
2. 2.4 PID lock (30 min)
3. 2.1 Watchdog (2–3 h)
4. 2.2 Log capture (2–3 h)

**Sprint 2 — security + events:**
5. 2.5 HTTP auth middleware (1–2 h)
6. 2.6 plugin_died event (1–2 h)
7. 2.7 KernelCommand dispatch (1–2 h)

**Sprint 3 — SDK completion:**
8. 3.1 Python SDK (4–6 h)
9. 3.2 C++ SDK client (4–6 h)

**Sprint 4 — observability:**
10. 4.1 Logging coverage (2 h)
11. 4.2 Prometheus metrics (2–3 h)
12. 4.3 EventStore (4–6 h)

**Sprint 5 — external access:**
13. 5.1 WebSocket gateway (4–6 h)
14. 5.2 Namespace sandboxing (4–8 h)

---

## Acceptance Criteria per Sprint

**Sprint 1 done when:**
- `vyn start` kills a previously running instance instead of hanging
- Plugin crash loop restarts at 100ms → 200ms → … not instantly
- `GET /plugins/myplugin/logs?lines=50` returns plugin stdout
- Watchdog test: plugin that ignores pings gets killed after timeout and restarted

**Sprint 2 done when:**
- `POST /plugins/x/stop` without `Authorization: Bearer <token>` → 401
- Other plugins receive `system.plugin_died` event when a supervised plugin crashes
- `KernelCommand { command: "health_check" }` → `KernelCommandAck { data_json: {...} }`

**Sprint 3 done when:**
- Python plugin connects, registers, sends ping, receives pong
- C++ plugin connects, registers, sends `ActionRequest { action: "http_get" }`, receives `ActionResponse`
- Both SDKs have integration tests that run in CI

**Sprint 4 done when:**
- `curl http://localhost:8000/metrics` shows message counters
- Permission denial, CRC error, backpressure drop all produce structured log events
- Event published to offline plugin is re-delivered when plugin reconnects

**Sprint 5 done when:**
- Browser WebSocket client connects with `?token=<jwt>` and receives `system.plugin_joined` events
- Plugin spawned with `sandbox: true` cannot `kill -9` any other process or open TCP sockets

---

## Stub Files to Delete or Implement

| File | Action |
|------|--------|
| `src/kernel/signals.rs` | Delete (signal handling now in orchestrator.rs) |
| `src/plugins/manager.rs` | Implement in Sprint 2 (KernelCommand dispatch + plugin lifecycle commands) |
| `src/plugins/runner.rs` | Implement in Sprint 3 (execution context for sandboxed plugins) |
| `src/api/middleware.rs` | Implement in Sprint 2 (JWT auth for HTTP) |
| `src/api/websocket.rs` | Implement in Sprint 5 |
