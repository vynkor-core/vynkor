# Phase 1.1 Roadmap: MVP Plugin Kernel

> **Goal:** End-to-end message exchange — kernel spawns two plugins, they register, exchange messages through the kernel, all over UDS with binary framing and protobuf.
>
> **Team:** 1 Rust developer (kernel + Rust SDK), 1 C++ developer (C++ SDK + example plugin)
>
> **Timeline:** 3 weeks (15 working days per developer)
>
> **Start date:** Week of 2026-06-23

---

## 1. Executive Summary

Phase 1.1 turns 195 lines of working scaffolding (CLI daemon, HTTP health check, config loading, process management) and 29 empty `.rs` stubs into a functioning plugin kernel. The deliverable: a Veyron daemon that accepts plugin connections over Unix domain sockets, routes protobuf messages between plugins using a 44-byte binary frame protocol, and supervises plugin processes.

The Rust developer builds the kernel internals — framing, UDS transport, registry, router, supervisor, event bus, and the Rust SDK. The C++ developer builds a matching C++ SDK and example plugin that proves cross-language interop. Both workstreams share the protobuf schema and binary frame format as their contract; they can work independently for most of the phase.

**What Phase 1.1 does NOT include:** JWT auth/RBAC (deferred to Phase 1.2), AI orchestration (Phase 3), WebSocket gateway beyond a skeleton, Python SDK, SQLite EventStore persistence, rate limiting.

---

## 2. MVP Acceptance Criteria

All must pass for Phase 1.1 to be considered complete:

- [ ] `cargo build --release` succeeds with zero warnings
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo fmt --check` passes
- [ ] `vyn start --foreground` launches daemon, listens on `/tmp/veyron.sock`
- [ ] Rust SDK plugin connects to kernel via UDS
- [ ] Plugin sends `PluginRegister`, receives `PluginRegisterAck` with `accepted: true`
- [ ] Kernel maintains in-memory registry of connected plugins
- [ ] Plugin A sends `ActionRequest` targeting Plugin B
- [ ] Kernel routes message to Plugin B via binary frame
- [ ] Plugin B receives message, sends `ActionResponse` back through kernel
- [ ] Plugin A receives response
- [ ] Kernel detects plugin disconnect and removes from registry
- [ ] Supervisor restarts crashed plugin (configurable: always/on-failure/never)
- [ ] Event bus delivers `EventBroadcast` to subscribed plugins
- [ ] Ping/Pong keepalive works
- [ ] C++ SDK plugin connects, registers, exchanges messages with Rust kernel
- [ ] Integration test: kernel + 2 plugins full message round-trip
- [ ] All unit tests pass: `cargo test --all`
- [ ] HTTP API returns plugin list at `GET /plugins`

### In Scope

- Binary framing (44-byte header + protobuf payload)
- UDS server/client transport
- Plugin registry (in-memory)
- Message router (by target field in frame header)
- Plugin supervisor (spawn, monitor, restart)
- Event bus (in-memory pub/sub, no persistence)
- Rust SDK (connect, register, send, receive)
- C++ SDK (connect, register, send, receive)
- Basic HTTP API (health, plugin list)
- Ping/Pong health check
- Unit + integration tests
- Permission checking (manifest-declared permissions only, no JWT)

### Out of Scope (Deferred)

| Feature | Deferred To |
|---------|-------------|
| JWT authentication | Phase 1.2 |
| RBAC with token-based identity | Phase 1.2 |
| WebSocket gateway (full) | Phase 1.2 |
| SQLite EventStore (at-least-once) | Phase 2 |
| Rate limiting | Phase 2 |
| Python SDK | Phase 1.2 |
| AI orchestration | Phase 3 |
| Plugin marketplace | Phase 4 |
| Compression (zstd) in frames | Phase 2 |
| Frame fragmentation | Phase 2 |

---

## 3. Architecture & Dependency Graph

```
                    Proto Schema (veyron_protocol.proto)
                    ┌───────────────┐
                    │   FIXED v1.0  │
                    └───────┬───────┘
                            │
              ┌─────────────┼─────────────┐
              ▼             ▼             ▼
        ┌──────────┐  ┌──────────┐  ┌──────────┐
        │ prost    │  │ protoc   │  │ protoc   │
        │ codegen  │  │ C++ gen  │  │ Python   │
        │ (Rust)   │  │          │  │ (defer)  │
        └────┬─────┘  └────┬─────┘  └──────────┘
             │              │
             ▼              ▼
     ┌──────────────┐ ┌──────────────┐
     │ VeyronError  │ │ C++ Framing  │ ← Can start Day 1
     │ (error.rs)   │ │ (framing.cpp)│
     └──────┬───────┘ └──────┬───────┘
             │              │
             ▼              ▼
     ┌──────────────┐ ┌──────────────┐
     │ Rust Framing │ │ C++ Client   │
     │ (framing.rs) │ │ (client.cpp) │
     └──────┬───────┘ └──────┬───────┘
            │               │
            ▼               ▼
     ┌──────────────┐ ┌──────────────┐
     │ UDS Server   │ │ C++ Plugin   │
     │ (server.rs)  │ │ Base Class   │
     │ + Connection │ │ (plugin.hpp) │
     └──────┬───────┘ └──────┬───────┘
            │               │
     ┌──────┼──────┐        │
     ▼      ▼      ▼        ▼
  ┌──────┐┌──────┐┌──────┐┌──────────────┐
  │Regis-││Router││Event ││C++ Echo      │
  │try   ││      ││Bus   ││Plugin Example│
  └──┬───┘└──┬───┘└──┬───┘└──────────────┘
     │       │       │          │
     ▼       ▼       ▼          │
  ┌──────────────────────┐      │
  │ Supervisor           │      │
  │ (spawn + monitor)    │      │
  └──────────┬───────────┘      │
             │                  │
             ▼                  ▼
  ┌────────────────────────────────────┐
  │ INTEGRATION TEST                   │
  │ Kernel + Rust plugin + C++ plugin  │
  │ Full message round-trip            │
  └────────────────────────────────────┘
```

### Critical Path

```
Proto codegen → Error types → Framing → UDS Server → Registry → Router → Supervisor → Integration Tests
```

### Parallel Opportunities

| Rust Developer | C++ Developer | Sync Point |
|---|---|---|
| Proto codegen (Day 1) | Proto C++ gen (Day 1) | Frame format agreed |
| Framing + UDS Server (Days 2-4) | C++ Framing (Days 2-4) | Binary frame spec |
| Registry + Router (Days 5-7) | C++ Client + Plugin (Days 5-8) | Registration handshake |
| Event Bus + Supervisor (Days 8-10) | Echo plugin example (Days 9-10) | — |
| Rust SDK (Days 6-8) | Integration test (Days 11-12) | Kernel must be running |
| HTTP API + Integration (Days 11-13) | Docs + polish (Days 13-15) | — |

---

## 4. Workstream A: Rust Kernel Core

### Task A.1: Error Types & Proto Codegen Verification

**Owner:** Rust Developer
**Effort:** 1 day
**Priority:** Critical (blocks everything)
**Dependencies:** None
**Blocks:** All subsequent tasks

#### Definition

Verify `proto/build.rs` generates Rust types correctly. Create unified `VeyronError` enum for all kernel error handling. Currently `src/utils/errors.rs` is empty and the project uses `anyhow::Result` everywhere — which is fine for the CLI layer but wrong for library code that other modules import.

#### Acceptance Criteria

- [ ] `cargo build` succeeds and generates protobuf types from `veyron_protocol.proto`
- [ ] Generated types accessible via `include!(concat!(env!("OUT_DIR"), "/veyron.rs"))` or equivalent
- [ ] Can construct and serialize an `Envelope` with a `PluginRegister` payload in a test
- [ ] `VeyronError` enum created with variants: `Io`, `Proto`, `FrameMagicMismatch`, `FrameCrcMismatch`, `PayloadTooLarge`, `PluginNotFound`, `PluginAlreadyRegistered`, `PermissionDenied`, `Timeout`, `Internal`
- [ ] `VeyronError` implements `std::error::Error`, `Display`, `From<io::Error>`, `From<prost::DecodeError>`
- [ ] Unit test: serialize/deserialize `Envelope` round-trip

#### Implementation Notes

- Add `prost` and `prost-types` to `Cargo.toml` dependencies
- Add `prost-build` to `[build-dependencies]`
- Move `proto/build.rs` to project root `build.rs` (Cargo convention) or reference it properly
- Create a `src/proto.rs` module that re-exports generated types

#### Files to Create/Modify

- **Create:** `src/utils/errors.rs` (implement `VeyronError`)
- **Modify:** `src/utils/mod.rs` (add `pub mod errors;` — already declared but empty)
- **Create:** `src/proto.rs` (re-export generated protobuf types)
- **Modify:** `src/main.rs` (add `mod proto;`)
- **Modify:** `Cargo.toml` (add `prost`, `prost-types` deps; `prost-build` build-dep)
- **Verify:** `build.rs` or `proto/build.rs` works correctly

---

### Task A.2: Binary Framing Protocol

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** Critical (blocks UDS server, all message passing)
**Dependencies:** A.1
**Blocks:** A.3, A.6, B.3 (C++ must match this exactly)

#### Definition

The binary framing protocol wraps every message on the UDS stream. Without framing, raw byte streams have no message boundaries. The frame header is 44 bytes:

```
Offset  Size   Field       Description
──────  ────   ─────       ───────────
0       2      magic       0x56 0x52 ("VR")
2       2      flags       Bit 0: COMPRESSED, Bit 1: FRAGMENTED,
                           Bit 2: PRIORITY, Bit 3: ACK_REQUIRED
4       4      length      Payload length in bytes (big-endian u32)
8       32     target      UTF-8 plugin_id, null-padded to 32 bytes
                           Special values: "kernel", "*" (broadcast)
40      4      crc32       CRC32 of payload bytes (big-endian u32)
────────────────────────────────────────────────
44+     N      payload     Protobuf-encoded Envelope
```

This enables zero-copy routing: kernel reads 44-byte header, checks target field, forwards frame to destination without deserializing protobuf payload.

#### Acceptance Criteria

- [ ] `src/ipc/framing.rs` created with:
  - `async fn write_frame(stream: &mut UnixStream, target: &str, flags: u16, payload: &[u8]) -> Result<(), VeyronError>`
  - `async fn read_frame(stream: &mut UnixStream) -> Result<Frame, VeyronError>`
  - `struct Frame { pub magic: u16, pub flags: u16, pub length: u32, pub target: [u8; 32], pub crc32: u32, pub payload: Vec<u8> }`
  - `fn target_as_str(frame: &Frame) -> &str` (extract UTF-8 target, trim null padding)
- [ ] `write_frame` encodes 44-byte header, writes header + payload atomically
- [ ] `read_frame` reads header, validates magic bytes, validates CRC32, reads payload
- [ ] Constant `MAX_PAYLOAD_SIZE: usize = 1_048_576` (1 MB)
- [ ] Unit test: frame round-trip — write then read produces identical frame
- [ ] Unit test: magic mismatch → `FrameMagicMismatch` error
- [ ] Unit test: CRC32 mismatch → `FrameCrcMismatch` error
- [ ] Unit test: payload > 1MB → `PayloadTooLarge` error
- [ ] Unit test: target encoding/decoding with padding
- [ ] Code passes clippy + fmt

#### Implementation Notes

- Use `tokio::net::UnixStream` with `AsyncReadExt::read_exact` / `AsyncWriteExt::write_all`
- Use `crc32fast` crate (fast, no-std compatible) for CRC32
- For tests, use `tokio::net::UnixStream::pair()` to create connected pair without actual socket file
- Flags field: implement as constants, not enum (bitfield)
- Keep `Frame` struct simple — no methods beyond construction
- Never panic on malformed input; return typed errors

#### Files to Create/Modify

- **Create:** `src/ipc/framing.rs`
- **Modify:** `src/ipc/mod.rs` (replace `// Placeholder` with `pub mod framing;`)
- **Modify:** `Cargo.toml` (add `crc32fast` dependency)

---

### Task A.3: UDS Server & Connection Handler

**Owner:** Rust Developer
**Effort:** 3 days
**Priority:** Critical (blocks plugin connectivity)
**Dependencies:** A.2 (framing)
**Blocks:** A.4, A.5, A.6

#### Definition

The UDS server listens on `/tmp/veyron.sock` (configurable), accepts plugin connections, and spawns a Tokio task per connection. Each connection task runs a read loop that decodes frames and dispatches them to the kernel's internal message channel. A write half is kept for sending frames back to the plugin.

#### Acceptance Criteria

- [ ] `src/ipc/server.rs` implements `UdsServer`:
  - `async fn start(socket_path: &Path, tx: mpsc::Sender<IncomingMessage>) -> Result<JoinHandle<()>, VeyronError>`
  - Accepts connections in a loop, spawns handler task per connection
  - Cleans up stale socket file on start
  - Returns handle for graceful shutdown
- [ ] `src/ipc/connection.rs` (new file) implements `ConnectionHandler`:
  - Holds `UnixStream` split into `OwnedReadHalf` / `OwnedWriteHalf`
  - Read loop: `read_frame()` → construct `IncomingMessage { source_conn_id, frame }` → send to channel
  - Write method: `async fn send_frame(frame: Frame) -> Result<()>`
  - Detects disconnect (read returns 0 bytes) and notifies kernel
- [ ] `IncomingMessage` struct defined: `{ conn_id: u64, frame: Frame }`
- [ ] UDS socket path configurable via `Config`
- [ ] Socket file removed on server shutdown
- [ ] Unit test: server accepts connection, client sends frame, server receives it
- [ ] Unit test: client disconnect detected
- [ ] Test: multiple simultaneous connections

#### Implementation Notes

- Use `tokio::net::UnixListener::bind()` and `.accept()` loop
- Each connection gets a unique `conn_id` (atomic u64 counter)
- Write half stored in a `HashMap<conn_id, mpsc::Sender<Frame>>` shared with kernel
- Use `tokio::select!` in read loop for cancellation support
- Remove socket file before bind: `let _ = std::fs::remove_file(&path);`
- Add `socket_path: PathBuf` field to `Config` struct

#### Files to Create/Modify

- **Create:** `src/ipc/server.rs` (was empty)
- **Create:** `src/ipc/connection.rs`
- **Modify:** `src/ipc/mod.rs` (add modules)
- **Modify:** `src/ipc/messages.rs` (define `IncomingMessage`)
- **Modify:** `src/utils/config.rs` (add `socket_path` field)
- **Modify:** `src/main.rs` (start UDS server in `run_foreground`)

---

### Task A.4: Plugin Registry

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** Critical (blocks routing)
**Dependencies:** A.3 (UDS server running)
**Blocks:** A.6, A.7

#### Definition

The registry tracks all connected and registered plugins. When a plugin connects, it's "connected but unregistered." After sending `PluginRegister` and receiving `PluginRegisterAck`, it becomes "registered." Only registered plugins can send/receive messages.

#### Acceptance Criteria

- [ ] `src/plugins/registry.rs` implements `PluginRegistry`:
  - `fn register(&self, plugin_id: String, conn_id: u64, manifest: PluginManifest, write_tx: mpsc::Sender<Frame>) -> Result<(), VeyronError>`
  - `fn unregister(&self, plugin_id: &str)`
  - `fn get(&self, plugin_id: &str) -> Option<PluginEntry>`
  - `fn list(&self) -> Vec<PluginEntry>`
  - `fn is_registered(&self, conn_id: u64) -> bool`
  - `fn get_by_conn_id(&self, conn_id: u64) -> Option<PluginEntry>`
- [ ] `PluginEntry` struct: `{ plugin_id, conn_id, manifest, write_tx, registered_at, state: PluginState }`
- [ ] `PluginState` enum: `Connected`, `Registered`, `Shuttingdown`
- [ ] Duplicate `plugin_id` registration rejected with `PluginAlreadyRegistered`
- [ ] Thread-safe: use `DashMap` or `Arc<RwLock<HashMap<...>>>`
- [ ] Unit test: register, lookup, unregister
- [ ] Unit test: duplicate registration rejected
- [ ] Unit test: list returns all registered plugins

#### Implementation Notes

- Use `dashmap` crate for concurrent HashMap (add to Cargo.toml)
- Two indexes needed: `by_plugin_id: DashMap<String, PluginEntry>` and `by_conn_id: DashMap<u64, String>` (for reverse lookup when connection drops)
- Clone `PluginEntry` fields that need sharing; keep `write_tx` as the only owned sender

#### Files to Create/Modify

- **Create:** `src/plugins/registry.rs` (was empty)
- **Modify:** `src/plugins/mod.rs` (add `pub mod registry;`)
- **Modify:** `Cargo.toml` (add `dashmap` dependency)

---

### Task A.5: Plugin Supervisor

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** High (needed for automatic restart)
**Dependencies:** A.3 (UDS server), A.4 (registry)
**Blocks:** Integration tests

#### Definition

The supervisor spawns plugin processes, monitors them, and restarts on crash according to policy. Each plugin binary is a separate OS process that connects back to the kernel's UDS socket.

#### Acceptance Criteria

- [ ] `src/plugins/supervisor.rs` implements `PluginSupervisor`:
  - `async fn spawn_plugin(&self, config: PluginConfig) -> Result<PluginProcess, VeyronError>`
  - `async fn stop_plugin(&self, plugin_id: &str) -> Result<(), VeyronError>`
  - `async fn monitor_loop(&self)` — watches child processes, restarts per policy
- [ ] `PluginConfig` struct: `{ plugin_id, binary_path, args, restart_policy, max_restarts }`
- [ ] `RestartPolicy` enum: `Always`, `OnFailure`, `Never`
- [ ] Spawned process inherits `VEYRON_SOCKET_PATH` env var
- [ ] Process death detected via Tokio `Child::wait()`
- [ ] Restart counter tracked per plugin; after `max_restarts` failures, stop trying and log
- [ ] Graceful shutdown: send `PluginShutdown` frame, wait `grace_seconds`, then SIGKILL
- [ ] Unit test: spawn and stop a simple binary
- [ ] Unit test: restart policy `Always` triggers restart after process exit
- [ ] Unit test: max_restarts honored

#### Implementation Notes

- Use `tokio::process::Command` for async process management
- Store `HashMap<plugin_id, PluginProcess>` where `PluginProcess` holds `Child` handle and restart count
- Monitor loop: `tokio::select!` over all `child.wait()` futures
- For testing, create a trivial binary in `tests/fixtures/` that connects and immediately exits

#### Files to Create/Modify

- **Create:** `src/plugins/supervisor.rs` (was empty)
- **Modify:** `src/plugins/mod.rs`
- **Create:** `tests/fixtures/test_plugin.rs` (minimal plugin binary for testing)

---

### Task A.6: Message Router

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** Critical (core kernel function)
**Dependencies:** A.3 (connection handler), A.4 (registry)
**Blocks:** Integration tests

#### Definition

The router receives `IncomingMessage` from the UDS server's channel and dispatches it based on the frame's `target` field:

- `"kernel"` → handle internally (registration, subscribe, ping, action requests aimed at kernel)
- `"*"` → broadcast to all registered plugins (except sender)
- `"<plugin_id>"` → forward frame to specific plugin's write channel

The router does NOT deserialize protobuf for forwarding — it reads the target from the 44-byte header and forwards the raw frame. It only deserializes when target is `"kernel"` and it needs to process the message.

#### Acceptance Criteria

- [ ] `src/ipc/protocol.rs` implements `MessageRouter`:
  - `async fn run(rx: mpsc::Receiver<IncomingMessage>, registry: Arc<PluginRegistry>, event_bus: Arc<EventBus>)` — main dispatch loop
  - Routes by target field in frame header (zero-copy for plugin-to-plugin)
  - Handles `"kernel"` target: deserialize Envelope, match payload variant, call handler
  - Handles `"*"` target: broadcast to all registered plugins
  - Handles `"<plugin_id>"` target: lookup in registry, forward frame
- [ ] Kernel message handlers:
  - `PluginRegister` → validate manifest → register in registry → send `PluginRegisterAck`
  - `Subscribe` → add to event bus subscriptions
  - `Unsubscribe` → remove from event bus subscriptions
  - `Ping` → respond with `Pong`
  - `ActionRequest` → check permissions → dispatch to target plugin or handle internally
- [ ] Unknown target → send `ErrorMessage` back to sender with `ERR_NOT_REGISTERED`
- [ ] Unregistered connection sending non-`PluginRegister` message → reject with `ERR_NOT_REGISTERED`
- [ ] Unit test: route to specific plugin
- [ ] Unit test: broadcast to all plugins
- [ ] Unit test: kernel handles PluginRegister
- [ ] Unit test: reject unregistered sender

#### Implementation Notes

- Router runs as a dedicated Tokio task, reading from `mpsc::Receiver<IncomingMessage>`
- For `"kernel"` messages: `prost::Message::decode(frame.payload)` → match `envelope.payload`
- For forwarded messages: just send `Frame` to target's `write_tx` channel — no decode needed
- Keep handler functions separate for testability

#### Files to Create/Modify

- **Create:** `src/ipc/protocol.rs` (was empty, now the router)
- **Modify:** `src/ipc/mod.rs`

---

### Task A.7: Permission Checker

**Owner:** Rust Developer
**Effort:** 1 day
**Priority:** High
**Dependencies:** A.4 (registry stores manifest)
**Blocks:** None (can integrate into router after)

#### Definition

Before executing an `ActionRequest`, the kernel checks whether the sending plugin declared the required permission in its manifest. This is a simple in-memory check — no JWT, no tokens, just manifest-declared permissions.

#### Acceptance Criteria

- [ ] `src/auth/permissions.rs` implements:
  - `fn check_permission(registry: &PluginRegistry, plugin_id: &str, required: PermissionType) -> Result<(), VeyronError>`
  - `fn action_to_permission(action: &str) -> Option<PermissionType>` — maps action names to required permissions
- [ ] Permission check integrated into router's `ActionRequest` handler
- [ ] Denied action returns `ActionResponse` with `ACTION_PERMISSION_DENY` status
- [ ] Unit test: plugin with `PERMISSION_NETWORK` can call `http_get`
- [ ] Unit test: plugin without `PERMISSION_NETWORK` gets denied

#### Files to Create/Modify

- **Create:** `src/auth/permissions.rs` (was empty)
- **Modify:** `src/auth/mod.rs`

---

### Task A.8: Event Bus

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** High
**Dependencies:** A.4 (registry)
**Blocks:** Integration tests (event delivery)

#### Definition

In-memory pub/sub system. Plugins subscribe to event types (e.g., `"system.plugin_joined"`, `"alarm.fired"`). When the kernel publishes an event, it's delivered to all subscribers via their write channels.

#### Acceptance Criteria

- [ ] `src/events/bus.rs` implements `EventBus`:
  - `fn subscribe(&self, plugin_id: &str, event_types: Vec<String>)`
  - `fn unsubscribe(&self, plugin_id: &str, event_types: Vec<String>)`
  - `fn unsubscribe_all(&self, plugin_id: &str)` — called on disconnect
  - `async fn publish(&self, event: Event, registry: &PluginRegistry)` — deliver to all subscribers
  - `fn subscribers(&self, event_type: &str) -> Vec<String>`
- [ ] Wildcard subscription `"*"` receives all events
- [ ] Kernel auto-publishes `system.plugin_joined` and `system.plugin_left` events
- [ ] Unit test: subscribe, publish, verify delivery
- [ ] Unit test: unsubscribe stops delivery
- [ ] Unit test: wildcard subscription
- [ ] Unit test: unsubscribe_all on disconnect

#### Implementation Notes

- Use `DashMap<String, HashSet<String>>` — event_type → set of plugin_ids
- Also maintain reverse index: `DashMap<String, HashSet<String>>` — plugin_id → set of event_types (for `unsubscribe_all`)
- No persistence in Phase 1.1 — events are fire-and-forget; at-least-once delivery is Phase 2

#### Files to Create/Modify

- **Create:** `src/events/bus.rs` (was empty)
- **Modify:** `src/events/mod.rs`

---

### Task A.9: Rust SDK

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** Critical (needed for testing, example plugins)
**Dependencies:** A.2 (framing spec finalized)
**Blocks:** Integration tests

#### Definition

Minimal Rust SDK for plugin developers. Handles UDS connection, binary framing, protobuf serialization, and provides a simple `Plugin` trait to implement.

#### Acceptance Criteria

- [ ] `sdk/rust/Cargo.toml` with dependencies: `tokio`, `prost`, `crc32fast`
- [ ] `sdk/rust/src/framing.rs` — identical framing logic to kernel (or shared crate)
- [ ] `sdk/rust/src/client.rs` implements `VeyronClient`:
  - `async fn connect(socket_path: &str) -> Result<Self, VeyronError>`
  - `async fn register(&mut self, plugin_id: &str, manifest: PluginManifest) -> Result<PluginRegisterAck, VeyronError>`
  - `async fn send(&mut self, target: &str, envelope: Envelope) -> Result<(), VeyronError>`
  - `async fn recv(&mut self) -> Result<Envelope, VeyronError>`
  - `async fn subscribe(&mut self, event_types: Vec<String>) -> Result<(), VeyronError>`
  - `async fn ping(&mut self) -> Result<Duration, VeyronError>` — measure round-trip time
- [ ] `sdk/rust/src/plugin.rs` defines `Plugin` trait:
  - `fn id(&self) -> &str`
  - `fn manifest(&self) -> PluginManifest`
  - `async fn on_init(&mut self, client: &mut VeyronClient) -> Result<(), VeyronError>`
  - `async fn on_message(&mut self, envelope: Envelope) -> Result<Option<Envelope>, VeyronError>`
  - `async fn on_shutdown(&mut self) -> Result<(), VeyronError>`
- [ ] `sdk/rust/src/lib.rs` re-exports public API
- [ ] Example echo plugin in `examples/echo_plugin_rs/` that implements `Plugin`
- [ ] Echo plugin: receives any `ActionRequest`, echoes back `ActionResponse` with same data

#### Implementation Notes

- Consider making framing a shared crate (`veyron-framing`) to avoid duplication between kernel and SDK — or just copy the module for now and extract later
- SDK should read `VEYRON_SOCKET_PATH` env var with fallback to `/tmp/veyron.sock`
- `Plugin` trait provides a `run()` default method that does the connect → register → recv loop

#### Files to Create/Modify

- **Create:** `sdk/rust/Cargo.toml`
- **Create:** `sdk/rust/src/lib.rs`
- **Create:** `sdk/rust/src/framing.rs`
- **Create:** `sdk/rust/src/client.rs`
- **Create:** `sdk/rust/src/plugin.rs`
- **Create:** `examples/echo_plugin_rs/main.rs`
- **Create:** `examples/echo_plugin_rs/Cargo.toml`

---

### Task A.10: HTTP API Routes

**Owner:** Rust Developer
**Effort:** 1 day
**Priority:** Medium
**Dependencies:** A.4 (registry)
**Blocks:** None

#### Definition

Extend existing Axum HTTP server with routes for plugin inspection. No auth in Phase 1.1 — these are local-only diagnostic endpoints.

#### Acceptance Criteria

- [ ] `GET /health` — returns `{"status":"ok"}` (already exists)
- [ ] `GET /plugins` — returns JSON array of registered plugins: `[{"plugin_id":"weather","state":"registered","registered_at":"...","permissions":[...]}]`
- [ ] `GET /plugins/:id` — returns single plugin details or 404
- [ ] `POST /plugins/:id/stop` — sends shutdown command to plugin
- [ ] API server has access to `PluginRegistry` via Axum state
- [ ] Unit test: /plugins returns correct data

#### Files to Create/Modify

- **Modify:** `src/api/server.rs` (add routes, inject registry as state)
- **Create:** `src/api/routes.rs` (handler functions)

---

### Task A.11: Kernel Orchestration

**Owner:** Rust Developer
**Effort:** 1 day
**Priority:** Critical
**Dependencies:** A.3–A.8 (all components exist)
**Blocks:** Integration tests

#### Definition

Wire all components together in `run_foreground()`. The kernel startup sequence:

1. Load config
2. Create `PluginRegistry`
3. Create `EventBus`
4. Start UDS server → get `mpsc::Receiver<IncomingMessage>`
5. Start message router task (reads from channel, uses registry + event bus)
6. Start HTTP API (with registry as shared state)
7. Start supervisor (spawn configured plugins)
8. Wait for shutdown signal

#### Acceptance Criteria

- [ ] `src/kernel/kernel.rs` implements `Kernel` struct with `async fn run(config: Config) -> Result<()>`
- [ ] All components initialized and connected via channels
- [ ] Graceful shutdown: SIGTERM → stop accepting connections → send `PluginShutdown` to all → wait grace period → exit
- [ ] `run_foreground()` in main.rs delegates to `Kernel::run()`
- [ ] Daemon starts successfully with `vyn start --foreground`

#### Files to Create/Modify

- **Create:** `src/kernel/kernel.rs` (was empty)
- **Modify:** `src/kernel/mod.rs`
- **Modify:** `src/main.rs` (use `Kernel::run()`)

---

### Task A.12: Integration Tests

**Owner:** Rust Developer
**Effort:** 2 days
**Priority:** Critical
**Dependencies:** A.9 (Rust SDK), A.11 (kernel wired)
**Blocks:** Phase completion

#### Definition

End-to-end tests proving the full message flow works.

#### Acceptance Criteria

- [ ] Test 1: **Plugin Registration**
  - Start kernel in test
  - Connect with Rust SDK client
  - Send `PluginRegister`
  - Receive `PluginRegisterAck` with `accepted: true`
  - Verify plugin appears in registry
- [ ] Test 2: **Message Routing**
  - Start kernel
  - Register Plugin A and Plugin B
  - Plugin A sends `ActionRequest` targeting Plugin B
  - Plugin B receives the request
  - Plugin B sends `ActionResponse` back
  - Plugin A receives the response
- [ ] Test 3: **Event Broadcasting**
  - Start kernel
  - Register Plugin A with subscription to `"test.event"`
  - Kernel publishes `Event { event_type: "test.event" }`
  - Plugin A receives the event
- [ ] Test 4: **Disconnect Handling**
  - Start kernel
  - Register plugin
  - Drop plugin connection
  - Verify plugin removed from registry
  - Verify `system.plugin_left` event published
- [ ] Test 5: **Ping/Pong**
  - Register plugin
  - Send `Ping`
  - Receive `Pong` with timestamps
- [ ] All tests pass with `cargo test --test integration`

#### Files to Create/Modify

- **Create:** `tests/integration/test_registration.rs`
- **Create:** `tests/integration/test_routing.rs`
- **Create:** `tests/integration/test_events.rs`
- **Create:** `tests/integration/test_disconnect.rs`
- **Create:** `tests/integration/test_ping.rs`
- **Modify:** `tests/integration/mod.rs`

---

## 5. Workstream B: C++ SDK & Examples

### Task B.1: Proto C++ Code Generation

**Owner:** C++ Developer
**Effort:** 1 day
**Priority:** Critical
**Dependencies:** Proto schema finalized (already done)
**Blocks:** B.2, B.3

#### Definition

Set up protoc to generate C++ bindings from `veyron_protocol.proto`. Generated files should be built as a separate static library linked by the SDK.

#### Acceptance Criteria

- [ ] `protoc --cpp_out=... veyron_protocol.proto` generates `veyron_protocol.pb.h` and `veyron_protocol.pb.cc`
- [ ] Generated files compile with C++17
- [ ] CMakeLists.txt updated to build proto library
- [ ] Can construct and serialize an `Envelope` with `PluginRegister` in a C++ test
- [ ] Unit test: serialize in C++, deserialize matches expected fields

#### Implementation Notes

- Use `protoc` directly or via CMake's `protobuf_generate_cpp()` macro
- CMakeLists.txt currently depends on `gRPC::grpc++` — remove gRPC dependency since Veyron uses raw UDS, not gRPC. Replace with `protobuf::libprotobuf` only
- Proto file path: `../../proto/veyron_protocol.proto` (relative to `sdk/cpp/`)

#### Files to Create/Modify

- **Modify:** `sdk/cpp/CMakeLists.txt` (fix proto generation, remove gRPC dep)
- **Create:** `sdk/cpp/proto/` directory (for generated files, gitignored)
- **Create:** `sdk/cpp/tests/test_proto.cpp`

---

### Task B.2: C++ Binary Framing

**Owner:** C++ Developer
**Effort:** 2 days
**Priority:** Critical (must match Rust framing exactly)
**Dependencies:** B.1 (proto gen), Frame spec from A.2
**Blocks:** B.3

#### Definition

C++ implementation of the 44-byte binary frame protocol, byte-identical to the Rust implementation. This is the most critical sync point between workstreams — if frames don't match, nothing works.

#### Acceptance Criteria

- [ ] `sdk/cpp/include/veyron/framing.hpp` declares:
  - `struct Frame { uint16_t magic; uint16_t flags; uint32_t length; char target[32]; uint32_t crc32; std::vector<uint8_t> payload; }`
  - `ssize_t write_frame(int fd, const std::string& target, uint16_t flags, const uint8_t* payload, size_t len)`
  - `std::optional<Frame> read_frame(int fd)`
  - `uint32_t compute_crc32(const uint8_t* data, size_t len)`
- [ ] Byte layout matches Rust exactly (same offsets, same endianness, same magic bytes)
- [ ] CRC32 algorithm matches Rust (standard CRC32, same polynomial)
- [ ] Max payload size enforced: 1 MB
- [ ] Unit test: write frame, read back, verify identical
- [ ] Unit test: CRC mismatch detection
- [ ] **Cross-language test:** frame written by C++ can be read by Rust (and vice versa)
  - Write a frame to a file in C++, read it in Rust, verify fields match
  - Or use a Unix socket pair for the test

#### Implementation Notes

- Use POSIX `read()`/`write()` with `MSG_WAITALL`-style loops (read until N bytes received)
- CRC32: use a header-only implementation or link `zlib` for `crc32()`
- Big-endian encoding: `htonl()`/`ntohl()` for length and CRC32 fields
- The existing `VeyronHeader` in `client.hpp` has wrong sizes (magic is 1 byte, should be 2; crc32 is uint16_t, should be uint32_t) — fix this
- Struct packing: use `#pragma pack(push, 1)` for on-wire header struct

#### Files to Create/Modify

- **Rewrite:** `sdk/cpp/include/veyron/framing.hpp` (fix header struct)
- **Rewrite:** `sdk/cpp/src/framing.cpp` (implement read/write)
- **Create:** `sdk/cpp/tests/test_framing.cpp`

---

### Task B.3: C++ Client (UDS Connection)

**Owner:** C++ Developer
**Effort:** 2 days
**Priority:** Critical
**Dependencies:** B.2 (framing)
**Blocks:** B.4, B.5

#### Definition

C++ client class that connects to the Veyron kernel's UDS socket, handles the registration handshake, and provides send/receive methods.

#### Acceptance Criteria

- [ ] `sdk/cpp/include/veyron/client.hpp` declares `VeyronClient`:
  - `bool connect(const std::string& socket_path)`
  - `bool register_plugin(const std::string& plugin_id, const PluginManifest& manifest)`
  - `bool send(const std::string& target, const Envelope& envelope)`
  - `std::optional<Envelope> recv()`
  - `void disconnect()`
  - `bool is_connected() const`
- [ ] `sdk/cpp/src/client.cpp` implements all methods
- [ ] Uses POSIX `socket()`, `connect()` with `AF_UNIX`
- [ ] Registration handshake: send `PluginRegister` → wait for `PluginRegisterAck` → check `accepted`
- [ ] Reads `VEYRON_SOCKET_PATH` env var, defaults to `/tmp/veyron.sock`
- [ ] Error handling: methods return `false`/`nullopt` on failure, set internal error string
- [ ] Unit test: connect to mock server, send/receive frame

#### Implementation Notes

- The existing `client.hpp` has `send_kernel_message` template — rewrite to use new framing
- Use `struct sockaddr_un` for UDS addressing
- Consider non-blocking I/O or a receive timeout to prevent permanent blocking on `recv()`

#### Files to Create/Modify

- **Rewrite:** `sdk/cpp/include/veyron/client.hpp`
- **Create:** `sdk/cpp/src/client.cpp` (was empty)
- **Modify:** `sdk/cpp/CMakeLists.txt`

---

### Task B.4: C++ Plugin Base Class

**Owner:** C++ Developer
**Effort:** 1 day
**Priority:** High
**Dependencies:** B.3 (client)
**Blocks:** B.5

#### Definition

Abstract base class that plugin developers inherit from. Mirrors the Rust `Plugin` trait.

#### Acceptance Criteria

- [ ] `sdk/cpp/include/veyron/plugin.hpp` declares `VeyronPlugin`:

  ```cpp
  class VeyronPlugin {
  public:
      virtual ~VeyronPlugin() = default;
      virtual std::string id() const = 0;
      virtual PluginManifest manifest() const = 0;
      virtual void on_init(VeyronClient& client) = 0;
      virtual std::optional<Envelope> on_message(const Envelope& msg) = 0;
      virtual void on_shutdown() = 0;

      int run(const std::string& socket_path = "");
  };
  ```

- [ ] `run()` method implements: connect → register → recv loop → dispatch to `on_message` → send response if returned → handle shutdown signal
- [ ] `run()` returns exit code (0 = clean shutdown, 1 = error)
- [ ] Handles SIGTERM for graceful shutdown

#### Files to Create/Modify

- **Rewrite:** `sdk/cpp/include/veyron/plugin.hpp` (was empty)
- **Create:** `sdk/cpp/src/plugin.cpp`

---

### Task B.5: C++ Echo Plugin Example

**Owner:** C++ Developer
**Effort:** 1 day
**Priority:** High
**Dependencies:** B.4 (plugin base class)
**Blocks:** Cross-language integration test

#### Definition

Working example plugin in C++ that echoes back any `ActionRequest` as an `ActionResponse`. Proves the full C++ SDK works end-to-end with the Rust kernel.

#### Acceptance Criteria

- [ ] `examples/echo_plugin_cpp/main.cpp` implements `EchoPlugin : public VeyronPlugin`
- [ ] Plugin registers with `plugin_id: "echo_cpp"`, permissions: none
- [ ] On receiving `ActionRequest`, returns `ActionResponse` with same `action_id` and `data_json` echoed
- [ ] CMakeLists.txt builds the example as a standalone binary
- [ ] Binary connects to running Veyron kernel and exchanges messages
- [ ] Can be tested manually: start kernel → start echo plugin → send message via Rust test client

#### Files to Create/Modify

- **Create:** `examples/echo_plugin_cpp/main.cpp`
- **Create:** `examples/echo_plugin_cpp/CMakeLists.txt`

---

### Task B.6: Cross-Language Integration Test

**Owner:** C++ Developer + Rust Developer (collaboration)
**Effort:** 2 days
**Priority:** Critical (proves the system works)
**Dependencies:** A.11 (kernel running), A.9 (Rust SDK), B.5 (C++ plugin)
**Blocks:** Phase completion

#### Definition

End-to-end test: Rust kernel + Rust plugin + C++ plugin exchanging messages.

#### Acceptance Criteria

- [ ] Test script (shell or Rust test) that:
  1. Starts Veyron kernel (`vyn start --foreground`)
  2. Starts C++ echo plugin (background process)
  3. Starts Rust test client
  4. Rust client sends `ActionRequest` to `"echo_cpp"` target
  5. C++ plugin echoes back `ActionResponse`
  6. Rust client verifies response matches
  7. All processes shut down cleanly
- [ ] Test passes on Linux (primary platform)
- [ ] Test documented in `tests/README.md`

#### Files to Create/Modify

- **Create:** `tests/integration/test_cross_language.rs` or `tests/cross_language.sh`
- **Create:** `tests/README.md`

---

## 6. Timeline & Milestones

### Week 1: Foundation

| Day | Rust Developer | C++ Developer |
|-----|---------------|---------------|
| 1 | A.1: Error types + proto codegen | B.1: Proto C++ generation |
| 2 | A.2: Binary framing (day 1/2) | B.2: C++ framing (day 1/2) |
| 3 | A.2: Binary framing (day 2/2) + tests | B.2: C++ framing (day 2/2) + tests |
| 4 | A.3: UDS server (day 1/3) | B.3: C++ client (day 1/2) |
| 5 | A.3: UDS server (day 2/3) | B.3: C++ client (day 2/2) |

**Milestone 1 (end of Week 1):**

- [ ] Rust kernel accepts UDS connections and reads/writes frames
- [ ] C++ client connects to UDS and sends/receives frames
- [ ] Frame format verified compatible (cross-language frame read test)

### Week 2: Core Logic

| Day | Rust Developer | C++ Developer |
|-----|---------------|---------------|
| 6 | A.3: UDS server (day 3/3) + A.4: Registry (day 1/2) | B.4: Plugin base class |
| 7 | A.4: Registry (day 2/2) | B.5: Echo plugin example |
| 8 | A.6: Message router (day 1/2) | B.5: Polish + test echo plugin manually |
| 9 | A.6: Message router (day 2/2) | A.7: Permission checker (collaboration) |
| 10 | A.9: Rust SDK (day 1/2) | A.8: Event bus support from C++ side |

**Milestone 2 (end of Week 2):**

- [ ] Plugin registration handshake works (connect → register → ack)
- [ ] Message routing works (Plugin A → kernel → Plugin B)
- [ ] C++ echo plugin works with Rust kernel
- [ ] Event subscription/delivery works

### Week 3: Integration & Polish

| Day | Rust Developer | C++ Developer |
|-----|---------------|---------------|
| 11 | A.9: Rust SDK (day 2/2) + A.5: Supervisor (day 1/2) | B.6: Cross-language integration test (day 1/2) |
| 12 | A.5: Supervisor (day 2/2) | B.6: Cross-language integration test (day 2/2) |
| 13 | A.11: Kernel orchestration + A.10: HTTP API | Documentation, README updates |
| 14 | A.12: Integration tests (day 1/2) | Fix issues found in integration |
| 15 | A.12: Integration tests (day 2/2) + final polish | Final testing, CI setup |

**Milestone 3 (end of Week 3):**

- [ ] All MVP acceptance criteria met
- [ ] All tests pass
- [ ] Code passes clippy/fmt/clang-format
- [ ] Documentation complete

---

## 7. Integration Points

These are moments where both developers must synchronize:

### Sync Point 1: Frame Format (Day 2)

Before either developer writes framing code, agree on exact byte layout:

- Magic bytes: `0x56, 0x52`
- Byte order: big-endian for all multi-byte fields
- CRC32 polynomial: standard (ISO 3309, same as zlib)
- Target encoding: UTF-8 with null-byte padding to 32 bytes
- Write a shared test vector: known input → expected frame bytes

### Sync Point 2: Registration Handshake (Day 7)

Both developers verify their implementations produce compatible `PluginRegister` / `PluginRegisterAck` messages:

- C++ plugin connects to running Rust kernel
- Sends `PluginRegister`, receives `PluginRegisterAck`
- If it fails: debug together, compare protobuf serialization output byte-by-byte

### Sync Point 3: Proto Changes (Any Time)

If either developer needs to change `veyron_protocol.proto`:

1. Discuss change first
2. Make change on a branch
3. Both rebuild proto bindings
4. Verify existing tests still pass
5. Merge

### Sync Point 4: Integration Testing (Day 11-12)

Full end-to-end test with both SDKs. Budget 2 days because cross-language bugs are hard to debug.

---

## 8. Risk & Mitigation

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| Frame format mismatch between Rust and C++ | Blocks all cross-language communication | Medium | Write shared test vectors Day 1. Cross-language frame test before writing anything else |
| Proto codegen issues | Blocks all development | Low | Verify Day 1. Both developers confirm proto builds before proceeding |
| CRC32 algorithm mismatch | Silent data corruption | Medium | Use same polynomial (ISO 3309). Test with identical inputs in both languages |
| UDS permissions on different Linux distros | Plugin can't connect | Low | Use `/tmp/` directory. Document required permissions |
| Tokio version conflicts in workspace | Build failures | Low | Pin Tokio version in workspace Cargo.toml |
| C++ build system complexity (CMake + protobuf) | Slows C++ developer | Medium | Remove gRPC dependency (not needed). Keep CMake simple |
| Supervisor process management edge cases | Zombie processes, leaked fds | Medium | Start simple (spawn + wait). Add edge case handling in Phase 1.2 |
| Scope creep (adding JWT, WebSocket, AI) | Phase 1.1 never finishes | High | Strict scope. If it's not in acceptance criteria, it's Phase 1.2+ |

---

## 9. Success Metrics

After 3 weeks, verify:

- [ ] `cargo build --release` — zero warnings
- [ ] `cargo test --all` — 100% pass
- [ ] `cargo clippy -- -D warnings` — clean
- [ ] `cargo fmt --check` — clean
- [ ] `vyn start --foreground` — daemon launches, listens on UDS
- [ ] Rust SDK plugin connects, registers, sends/receives messages
- [ ] C++ SDK plugin connects, registers, sends/receives messages
- [ ] Two plugins exchange messages through kernel (full round-trip)
- [ ] Plugin crash triggers automatic restart (supervisor)
- [ ] Event bus delivers events to subscribers
- [ ] `GET /plugins` returns correct plugin list
- [ ] Logs show clear message flow with tracing spans
- [ ] Cross-language integration test passes
- [ ] Ping/Pong latency < 1ms on localhost

---

## 10. What Comes Next (Phase 1.2 Preview)

After Phase 1.1, the critical path continues:

| Feature | Why | Effort |
|---------|-----|--------|
| JWT Auth + RBAC | Plugins need identity beyond manifest | 3-4 days |
| WebSocket gateway | External clients need access | 3-4 days |
| Python SDK | ML/AI plugins written in Python | 3-4 days |
| Rate limiting | Prevent plugin resource abuse | 2 days |
| Config-driven plugin loading | Kernel reads plugin list from config, spawns automatically | 2 days |
| Health monitor (watchdog) | Detect hung plugins via ping timeout | 2 days |
| `vyn plugin install/list/remove` CLI | User-facing plugin management | 2 days |

Phase 1.2 is estimated at 3-4 weeks and builds directly on Phase 1.1 infrastructure.
