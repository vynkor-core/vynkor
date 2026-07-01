# Veyron Core — Implementation Roadmap

**Last updated:** 2026-06-21  
**Branch:** `develop` — commit `1c2a824`

This document tracks what is built, what is next, and what is later.
Effort estimates assume one developer familiar with the codebase.

---

## Current State (Phases 1–5 complete — roadmap done ✅)

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
| C++ SDK | Full client: connect, register, send/recv, ping, plugin base class |
| Python SDK | Full client: framing, VeyronClient, Plugin ABC, proto codegen |
| Tests | 89 tests (80 unit + 9 integration), all passing |
| Watchdog | Ping/Pong health monitor, SIGKILL + restart on timeout |
| Log capture | Per-plugin ring buffer (1000 lines), GET /plugins/:id/logs |
| Backoff | Exponential restart delay (100ms → 30s cap) |
| PID lock | flock LOCK_EX prevents concurrent daemon starts |
| HTTP auth | JWT middleware on POST routes, read-only routes open |
| plugin_died | system.plugin_died event on supervisor crash |
| KernelCommand | health_check + reload_config dispatched |
| Logging | Structured tracing spans across IPC, supervisor, events |
| Prometheus | GET /metrics — messages, plugins, restarts, latency histograms |
| EventStore | SQLite at-least-once delivery, EventAck, retry worker |
| WebSocket | /ws gateway — JWT via Sec-WebSocket-Protocol header (not URL), binary framing |
| Sandbox | PluginConfig.sandbox: bool — CLONE_NEWPID + CLONE_NEWNET via pre_exec (Linux) |
| Runner | sandbox_pre_exec + apply_resource_limits (RLIMIT_NPROC=64, RLIMIT_AS=512 MiB) in runner.rs |
| PluginManager | High-level lifecycle API (start/stop/restart/is_supervised/is_connected/logs) |
| AiRequest | Kernel proxies AI requests to Anthropic API via reqwest (claude-opus-4-8 default) |
| Slowloris | WS route wrapped with TimeoutLayer(5s, 408) — blocks slow HTTP upgrade attacks |

### Known gaps

- Namespace sandbox requires CAP_SYS_ADMIN or user-namespace support on hardened kernels

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

## Phase 5 — External Access ✅

### 5.1 WebSocket gateway ✅

Per architecture: WebSocket is for external clients (browser, mobile) only. Internal IPC stays on UDS.

- `src/api/websocket.rs` — Axum WebSocket handler
- JWT via `Sec-WebSocket-Protocol: veyron, <jwt>` header (not URL — avoids log leakage)
- Same 44-byte binary framing as UDS — transparent frame translation
- WS clients participate in the same routing graph with synthetic conn_id (base 1,000,000,000)

**Note:** `ws.protocols(["veyron"])` responds with `Sec-WebSocket-Protocol: veyron`. Client JS: `new WebSocket(url, ["veyron", jwtToken])`.

---

### 5.2 Linux namespace sandboxing for untrusted plugins ✅

- `PluginConfig` and `PluginDef` gain `sandbox: bool`
- `spawn_internal` with `sandbox: true`: calls `unshare(CLONE_NEWPID | CLONE_NEWNET)` via `pre_exec` — plugin can't signal other processes, can't open network sockets
- `#[cfg(target_os = "linux")]` — no-op on other platforms

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

## Sprint History (all complete ✅)

**Sprint 1 — reliability:** 2.1 watchdog, 2.2 log capture, 2.3 backoff, 2.4 PID lock
**Sprint 2 — security + events:** 2.5 HTTP auth, 2.6 plugin_died, 2.7 KernelCommand
**Sprint 3 — SDK completion:** 3.1 Python SDK, 3.2 C++ SDK client
**Sprint 4 — observability:** 4.1 logging, 4.2 Prometheus metrics, 4.3 EventStore
**Sprint 5 — external access:** 5.1 WebSocket gateway, 5.2 namespace sandboxing

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
- Browser WebSocket client connects with `Sec-WebSocket-Protocol: veyron, <jwt>` and receives `system.plugin_joined` events ✅
- Plugin spawned with `sandbox: true` cannot `kill -9` any other process or open TCP sockets ✅

---

## Stub Files to Delete or Implement

| File | Action |
|------|--------|
| `src/kernel/signals.rs` | Deleted ✅ (signal handling in orchestrator.rs) |
| `src/plugins/manager.rs` | Implemented ✅ (PluginManager: start/stop/restart/is_supervised/is_connected/logs) |
| `src/plugins/runner.rs` | Implemented ✅ (sandbox_pre_exec + apply_resource_limits via setrlimit) |
| `src/api/middleware.rs` | Implemented ✅ (JWT auth for HTTP) |
| `src/api/websocket.rs` | Implemented ✅ (Sprint 5) |
