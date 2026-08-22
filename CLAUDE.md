# Vynkor - Plugin Kernel System

A high-performance plugin kernel written in Rust with C++ interop and multi-SDK support (Python, C++, Rust). Implements a message-passing architecture for secure plugin sandboxing and IPC.

> **Rename COMPLETE (2026-08-22):** the project IS **vynkor** — org
> `vynkor-core`, repos `vynkor*`, crates `vynkor-wire` / `vynkor-sdk`
> (published), binary **`vyn`**, manager **`vynm`**. Old `veyron*` names
> exist only in history; do not reintroduce them. Authoritative layout:
> `docs/VYN_PRODUCT_LAYOUT.md`.

## WHY

- **Kernel-first architecture:** Plugin isolation via IPC, not direct linking
- **Multi-language SDK:** Rust/C++/Python developers can write plugins uniformly
- **Protocol-driven:** Protobuf contracts ensure versioning, forward/backward compat
- **Large distributed system:** Needs semantic navigation across SDK boundaries

## WHAT

### Core Architecture

```
┌─ Vynkor main process (Rust)
│  ├─ Kernel: Plugin lifecycle, config, signals
│  ├─ API Server: REST + WebSocket for plugin control
│  ├─ IPC: Unix domain sockets to plugins (UDS-only)
│  ├─ Auth: JWT + permission model
│  ├─ Events: Global event bus
│  └─ Plugin Manager: Loader, registry, supervisor
│
└─ Plugins (Separate Processes)
   ├─ Written in Rust, C++, or Python
   ├─ Communicate via Vynkor protocol (protobuf)
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

**SDKs (for plugin developers):** sibling repos — `vynkor-sdk` (Rust), `vynkor-sdk-cpp`, `vynkor-sdk-python` (checked out at `../vynkor-sdk-*` for tests/CI)

**Protocol & Docs:**
- `../vynkor-wire/proto/vynkor_protocol.proto` — IPC message schema (single source of truth)
- `docs/FRAMING.md` — Frame format, flag bits (single source of truth for flags)
- `docs/PLUGIN_REGISTRY_SCHEMA.md` — Marketplace registry schema
- `docs/archive/` — Historical architecture docs and completed roadmap phases

### Code Conventions

**Rust:**
- Error handling: `Result<T, VynkorError>` (see `utils/errors.rs`)
- Async: Tokio runtime
- Serialization: Protobuf via `prost`
- Plugin spawning: `std::process::Command` (separate OS process)
- Logging: `tracing` crate

**Protobuf:**
- `../vynkor-wire/proto/vynkor_protocol.proto` is **single source of truth** for plugin<->kernel IPC
- Codegen lives in the `vynkor-wire` crate (prost-build); this repo only re-exports via `src/proto.rs`
- **Always use `reserved` fields for forward compatibility**

**SDK Pattern:**
- All SDKs implement same `Plugin` trait
- Methods: `on_init()`, `on_shutdown()`, `on_message()`
- All use protobuf for serialization

**Comment style:**
- lowercase, terse, commit-message tone — not a docstring
- explain *why*, not what (code already says what)
- e.g. `// kernel-assigned id, not the requester's original action_id` not `// This variable stores the ID.`

### Critical Files (Edit Carefully)

- **`../vynkor-wire/proto/vynkor_protocol.proto`** ← Changes affect ALL plugins and SDKs
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

1. **Define contract first:** Edit `../vynkor-wire/proto/vynkor_protocol.proto`
2. **Run build:** `cargo build` (wire crate regenerates bindings)
3. **Implement logic:** Add handler in appropriate `src/` module
4. **Update SDKs:** If message interface changed, bump the `vynkor-wire` crate version and update sibling SDK repos; mirror the proto to `../vynkor-sdk-cpp/proto/` and `../vynkor-sdk-python/proto/` (CI's T-17 drift check enforces byte-identical copies)
5. **Test:** `cargo test --all-features`

### Bug Fix Workflow

1. Find broken component (e.g., plugin crashes)
2. Check `src/plugins/supervisor.rs` (restart logic?)
3. Check `src/ipc/protocol.rs` (message handling?)
4. Check `../vynkor-wire/proto/vynkor_protocol.proto` (schema match?)
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

- Modifying IPC protocol (`../vynkor-wire/proto/vynkor_protocol.proto`)
- Changes to plugin lifecycle (`orchestrator.rs`, `supervisor.rs`)
- Cross-SDK compatibility (affects all three SDKs)
- Performance-critical paths (IPC, event bus, kernel loop)

## Reference

- **Protocol:** `../vynkor-wire/proto/vynkor_protocol.proto`
- **Architecture:** `docs/archive/VEYRON_ARCHITECTURE.md` (historical) · `README.md` (current)
- **Roadmap:** `ROADMAP.md`
- **Config:** `config.yaml`
