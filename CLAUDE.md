# Veyron - Plugin Kernel System

A high-performance plugin kernel written in Rust with C++ interop and multi-SDK support (Python, C++, Rust). Implements a message-passing architecture for secure plugin sandboxing and IPC.

## WHY

- **Kernel-first architecture:** Plugin isolation via IPC, not direct linking
- **Multi-language SDK:** Rust/C++/Python developers can write plugins uniformly
- **Protocol-driven:** Protobuf contracts ensure versioning, forward/backward compat
- **Large distributed system:** Needs semantic navigation across SDK boundaries

## WHAT

### Core Architecture

```
┌─ Veyron Main Process (Rust)
│  ├─ Kernel: Plugin lifecycle, config, signals
│  ├─ API Server: REST + WebSocket for plugin control
│  ├─ IPC: Unix domain sockets to plugins (UDS-only)
│  ├─ Auth: JWT + permission model
│  ├─ Events: Global event bus
│  └─ Plugin Manager: Loader, registry, supervisor
│
└─ Plugins (Separate Processes)
   ├─ Written in Rust, C++, or Python
   ├─ Communicate via Veyron Protocol (protobuf)
   └─ Supervised (restart on crash, resource limits)
```

### Project Structure

**Core Components:**
- `src/kernel/` — Orchestrator (component wiring, shutdown), kernel commands
- `src/api/` — REST server, WebSocket, routes
- `src/auth/` — JWT, permissions system
- `src/ipc/` — Framing, connection handler, router, UDS server
- `src/plugins/` — Plugin loader, manager, supervisor
- `src/events/` — Event bus
- `src/cli/` — CLI interface
- `src/utils/` — Logging, errors, config parsing

**SDKs (for plugin developers):**
- `sdk/rust/` — Rust SDK
- `sdk/cpp/` — C++ SDK
- `sdk/python/` — Python SDK

**Protocol & Docs:**
- `proto/veyron_protocol.proto` — IPC message schema (single source of truth)
- `docs/FRAMING.md` — Frame format, flag bits (single source of truth for flags)
- `docs/PLUGIN_REGISTRY_SCHEMA.md` — Marketplace registry schema
- `docs/archive/` — Historical architecture docs and completed roadmap phases

### Code Conventions

**Rust:**
- Error handling: `Result<T, VeyronError>` (see `utils/errors.rs`)
- Async: Tokio runtime
- Serialization: Protobuf via `prost`
- Plugin spawning: `std::process::Command` (separate OS process)
- Logging: `tracing` crate

**Protobuf:**
- `proto/veyron_protocol.proto` is **single source of truth** for plugin<->kernel IPC
- Changes auto-generate Rust code via the root `build.rs` (prost-build)
- **Always use `reserved` fields for forward compatibility**

**SDK Pattern:**
- All SDKs implement same `Plugin` trait
- Methods: `on_init()`, `on_shutdown()`, `on_message()`
- All use protobuf for serialization

### Critical Files (Edit Carefully)

- **`proto/veyron_protocol.proto`** ← Changes affect ALL plugins
- **`src/kernel/orchestrator.rs`** ← Component wiring & shutdown sequencing
- **`src/plugins/supervisor.rs`** ← Process supervision & resource limits
- **`src/ipc/protocol.rs`** ← Message routing
- **`Cargo.toml`** ← Dependencies & test targets

## HOW

### Build & Test

```bash
# Build everything
cargo build --release

# Run tests
cargo test --all --all-features

# Check code
cargo clippy -- -D warnings
cargo fmt --check
```

### Feature Workflow

1. **Define contract first:** Edit `proto/veyron_protocol.proto`
2. **Run build:** `cargo build` (auto-generates proto bindings)
3. **Implement logic:** Add handler in appropriate `src/` module
4. **Update SDKs:** If message interface changed, update `sdk/rust/`, `sdk/cpp/`, `sdk/python/`
5. **Test:** `cargo test --all-features`

### Bug Fix Workflow

1. Find broken component (e.g., plugin crashes)
2. Check `src/plugins/supervisor.rs` (restart logic?)
3. Check `src/ipc/protocol.rs` (message handling?)
4. Check `proto/veyron_protocol.proto` (schema match?)
5. Write test in `tests/unit/` or `tests/integration/`
6. Fix and verify

### Common Issues

| Issue | Check |
|-------|-------|
| Plugin doesn't receive messages | `src/ipc/server.rs` — is route registered? Proto version match? |
| Proto changes break plugins | Use `reserved` fields. Bump proto version. |
| Plugin leaks memory | Resource limits apply only with `sandbox: true` (`src/plugins/runner.rs`) |
| IPC hangs | `src/ipc/protocol.rs` — timeout handling? Message framing? |
| Auth fails | `src/auth/jwt.rs` — token expiry? Permissions in `permissions.rs`? |

### When to Raise Questions

- Modifying IPC protocol (`proto/veyron_protocol.proto`)
- Changes to plugin lifecycle (`orchestrator.rs`, `supervisor.rs`)
- Cross-SDK compatibility (affects all three SDKs)
- Performance-critical paths (IPC, event bus, kernel loop)

## Reference

- **Protocol:** `proto/veyron_protocol.proto`
- **Architecture:** `docs/archive/VEYRON_ARCHITECTURE.md` (historical) · `README.md` (current)
- **Roadmap:** `ROADMAP.md`
- **Config:** `config.yaml`
