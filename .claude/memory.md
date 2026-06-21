# Claude Code Memory - Veyron

## Project State
- Status: Early development
- Key areas: Kernel lifecycle, IPC, multi-SDK support

## Architecture
- **Kernel:** Plugin lifecycle machine
- **IPC:** Protobuf-based message passing
- **Plugins:** Separate OS processes with supervision
- **SDKs:** Rust, C++, Python interfaces

## Build Commands
```bash
cargo build --release
cargo test --all --all-features
cargo clippy -- -D warnings
```

## Proto Workflow
1. Edit `proto/veyron_protocol.proto`
2. Run `cargo build` (auto-generates Rust bindings)
3. Update SDKs if interface changed

## Current Focus
[Describe what you're working on]
