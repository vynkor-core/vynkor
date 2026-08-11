# Veyron

**A Unix-native plugin kernel written in Rust.** Routes bytes between isolated processes over Unix Domain Sockets. Knows nothing about your business logic. Does not care about AI.

> **Two projects. Clear boundaries.**
>
> - **Veyron** — the "dumb" core. Routes frames, supervises processes, enforces permissions.
> - **Kairo** — a smart plugin built *on top of* Veyron. AI agent, memory, voice. Uses the kernel as infrastructure.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    External Clients                         │
│           (browser, mobile app, remote sensor)              │
└──────────────────────────┬──────────────────────────────────┘
                           │ WebSocket / HTTP  (JWT required)
                           │
┌──────────────────────────▼──────────────────────────────────┐
│                   Veyron Core (Rust)                        │
│                                                             │
│  ┌─────────────┐  ┌──────────────┐  ┌────────────────────┐  │
│  │ API Gateway │  │ Message      │  │ Plugin Supervisor  │  │
│  │ (Axum WS/   │  │ Router       │  │ (spawn, restart,   │  │
│  │  HTTP)      │  │ (target-key  │  │  SIGTERM, watchdog)│  │
│  └──────┬──────┘  │  routing,    │  └────────────────────┘  │
│         │         │  zero-parse) │                          │
│         └────────►│              │  ┌────────────────────┐  │
│                   └──────┬───────┘  │ Plugin Registry    │  │
│                          │          │ (DashMap, pong     │  │
│                          │          │  timestamps)       │  │
│                          │          └────────────────────┘  │
└──────────────────────────┼──────────────────────────────────┘
                           │ Unix Domain Sockets (UDS)
         ┌─────────────────┼────────────────────┐
         │                 │                    │
┌────────▼───────┐ ┌───────▼───────┐ ┌──────────▼─────────┐
│  Plugin: Kairo │ │ Plugin: STT   │ │  Plugin: Weather   │
│  (Rust)        │ │ (Python)      │ │  (any language)    │
│  AI agent      │ │ Speech-to-text│ │                    │
└────────────────┘ └───────────────┘ └────────────────────┘
```

**Data flow:** `Client → WebSocket (JWT validated) → Veyron Core → UDS frame → Plugin → UDS frame → Core → Client`

The core never inspects the payload. It reads 44 bytes, extracts the target field, and routes. That is all.

---

## The Veyron Manifesto

These rules are non-negotiable. PRs that violate them are rejected.

### 1. Dumb Core

The kernel contains **no** business logic, **no** AI models, **no** databases for application state. It is a high-speed byte router and process supervisor. All intelligence lives in plugins.

### 2. UDS-Only Intra-Host IPC

Plugin↔kernel communication uses Unix Domain Sockets exclusively. No TCP, no Redis, no RabbitMQ, no message queues. The kernel's UDS socket bypasses the TCP/IP stack entirely — lower latency, stricter access control (file permissions `0o600`).

### 3. Binary Frame Protocol

Every message on UDS is wrapped in a strict 44-byte header:

```
 0        2        4        8                            40       44
 ├────────┼────────┼────────┼────────────────────────────┼────────┤
 │ Magic  │ Flags  │ Length │ Target (32 bytes, UTF-8)   │ CRC32  │ Payload…
 │ 0x5652 │ 2 B    │ 4 B BE │ plugin_id or "kernel"      │ 4 B    │
 └────────┴────────┴────────┴────────────────────────────┴────────┘
```

- **Magic `0x5652`** ("VR") — bad magic closes connection instantly, no further read.
- **Zero-parse routing** — the core routes by the 32-byte target field *without deserializing the Protobuf payload*. A frame destined for `"weather-plugin"` is forwarded with zero copies and zero JSON parsing.
- **CRC32** — computed over payload only. Corrupt frame → `FrameCrcMismatch` → the kernel **drops the connection** (corruption on a local UDS is never line noise; a supervised plugin is restarted per its restart policy).
- **Flag Bit 0 (`0x0001`)** — `FLAG_MAC_PRESENT`: a 32-byte HMAC-SHA256 tag is appended after the payload. Active on all authenticated connections.
- **Flag Bit 1 (`0x0002`)** — `FLAG_COMPRESSED`: payloads ≥ 64 KiB are transparently zstd-compressed on the wire. See `docs/FRAMING.md` for the full flag table and MAC interaction.
- **Payload** — Protobuf `Envelope` (see `proto/veyron_protocol.proto`). The kernel only decodes it when `target == "kernel"`.

### 4. Security by Default

- JWT validation is mandatory. The kernel refuses to start without `jwt_secret` in config unless the operator explicitly sets `allow_no_auth: true`.
- WebSocket clients present their JWT in the `Sec-WebSocket-Protocol` header (`veyron, <token>`) — validated before the upgrade handshake completes.
- Per-session HMAC-SHA256 frame authentication activates after registration. A single tampered byte kills the connection.
- Permissions are default-deny. A plugin that wants to send frames to another plugin must declare `PERMISSION_IPC_SEND` *and* name the target in its `ipc_targets` allowlist.

### 5. Process Isolation

Every plugin runs in a separate OS process. The kernel spawns it, injects `VEYRON_SOCKET_PATH`, and supervises it. On Linux with `sandbox: true`, the plugin runs in private **user + network namespaces** (for non-root kernels the sandbox first switches into a private user namespace, so it works without CAP_SYS_ADMIN) with `RLIMIT_NPROC=1024` and `RLIMIT_AS=512MiB` by default. PID-namespace isolation is **not** included: the kernel cannot move the exec'd plugin into a new PID namespace from the spawn path — a process with a pending `pid_for_children` namespace (set by `unshare(CLONE_NEWPID)`) cannot create threads, which kills every multithreaded plugin with EINVAL. Doing it correctly requires a shim-process supervisor (tracked in `AUDIT.md`). A plugin crash does not affect other plugins or the kernel.

---

## Getting Started

### Prerequisites

- Rust 1.85+ (`rustup update stable`) — matches `veyron-wire`'s MSRV, which the kernel depends on
- Linux or macOS (Linux required for full sandbox isolation)

### Build

```bash
git clone https://github.com/veyron-core/veyron
cd veyron
cargo build --release
```

The binary is at `target/release/vyn`.

### Configure

```yaml
# config.yaml
port: 8080
# socket_path defaults to $XDG_RUNTIME_DIR/veyron.sock (never shared /tmp);
# set explicitly only if you need a custom location.
jwt_secret: "change-me-in-production"
log_level: info

plugins:
  - id: my-plugin
    binary: ./target/release/my_plugin
    restart: on-failure   # always | on-failure | never
    max_restarts: 5
    sandbox: true        # Linux only
```

### Run

```bash
# Start in background (daemonizes)
vyn start --config config.yaml

# Start in foreground (dev mode)
vyn start --foreground --debug

# Check status
vyn status

# Tail logs
vyn logs --lines 50

# Stop
vyn stop
```

### Write a Plugin (Rust)

```rust
use veyron_sdk::{Plugin, VeyronClient, VeyronError};
use veyron::proto::veyron::{Envelope, PluginManifest};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }
    fn manifest(&self) -> PluginManifest { PluginManifest::default() }

    async fn on_init(&mut self, _client: &mut VeyronClient) -> Result<(), VeyronError> {
        Ok(())
    }
    async fn on_message(&mut self, _env: Envelope) -> Result<Option<Envelope>, VeyronError> {
        Ok(None) // return Some(envelope) to reply to the kernel
    }
    async fn on_shutdown(&mut self) -> Result<(), VeyronError> { Ok(()) }
}

// MyPlugin.run().await connects via $VEYRON_SOCKET_PATH and registers.
```

SDKs available: [`veyron-sdk-rust`](https://github.com/veyron-core/veyron-sdk-rust),
[`veyron-sdk-cpp`](https://github.com/veyron-core/veyron-sdk-cpp), and
[`veyron-sdk-python`](https://github.com/veyron-core/veyron-sdk-python) — all
three support `send_action`, streaming actions (`send_action_streaming` +
request/response chunks), `close_session`, and `publish_event`.

Published packages:

| Package | Registry |
|---|---|
| [`veyron-sdk`](https://crates.io/crates/veyron-sdk) | crates.io (Rust) |
| [`veyron-wire`](https://crates.io/crates/veyron-wire) | crates.io (wire protocol types) |
| [`veyron-sdk`](https://pypi.org/project/veyron-sdk/) | PyPI (Python) |

```bash
cargo add veyron-sdk    # Rust plugins
pip install veyron-sdk  # Python plugins
```

---

## Project Structure

```
src/
├── kernel/         # Orchestrator: component wiring, shutdown sequencing
├── api/            # Axum HTTP + WebSocket gateway
├── auth/           # JWT validation, HMAC frame MAC, permission checks
├── ipc/            # UDS server, framing, routing, connection handler
├── plugins/        # Loader, manager, registry, supervisor, sandbox runner
├── events/         # Event bus, at-least-once delivery store
├── cli/            # `vyn` CLI (clap)
└── utils/          # Config, logging, errors

proto/
└── veyron_protocol.proto   # Single source of truth for all IPC message types
```

---

## Security

Report vulnerabilities via GitHub Security Advisories (not public issues). See `AUDIT.md` for the current audit findings and score.

Current posture: **pre-production**. Kernel core (framing, MAC, fragmentation, supervision) is solid and regression-tested; compressed-frame support works across all three SDKs (R5-01), and Rust/C++/Python SDKs now have full parity on `publish_event`, streaming actions, and session close (Phase 7). Remaining open items are tracked in `AUDIT.md` and `ROADMAP.md`.

---

## Protocol Reference

- **Frame format & flag bits:** `docs/FRAMING.md`
- **Message schema:** `proto/veyron_protocol.proto` (single source of truth)
- **Plugin registry schema:** `docs/PLUGIN_REGISTRY_SCHEMA.md`
- **Audit:** `AUDIT.md`
- **Roadmap:** `ROADMAP.md`

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
