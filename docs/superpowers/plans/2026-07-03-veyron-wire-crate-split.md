# Veyron Wire Crate Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the wire-protocol surface (`proto`, frame framing, frame MAC, socket-path default, and the subset of `VeyronError` that is protocol-level) out of the `veyron` kernel crate into a new standalone `veyron-wire` crate, so `veyron-sdk` (Rust) can depend on a small protocol crate instead of the whole kernel — and so the standalone `veyron-sdk-rust` repo no longer requires a sibling `veyron` checkout to build.

**Architecture:** New crate `wire/` inside the `veyron` core repo, published independently to crates.io. `veyron` (kernel) depends on it and re-exports/wraps its API so **zero existing kernel call sites or tests change behavior or error types** (`VeyronError` keeps every current variant; a `From<WireError>` impl handles the shared subset). `veyron-sdk` (both the copy embedded in the core repo and the standalone `veyron-sdk-rust` repo) drops its dependency on `veyron` entirely and depends only on `veyron-wire`.

**Tech Stack:** Rust, Cargo path+version dual dependencies, `prost`/`prost-build`, `hmac`/`sha2`/`hkdf`, `tokio` (io traits only), `nix` (uid lookup for socket dir).

## Global Constraints

- Zero behavior change in `veyron` (kernel) binary or its existing tests — `cargo test --all --all-features` in `veyron/` must pass unmodified except for import-path churn explicitly listed below.
- `VeyronError` keeps its exact current variant list and `Display` text (`src/utils/errors.rs`) — no call site outside the moved files may need to change its match arms.
- `veyron-wire`'s `Cargo.toml` must have zero dependency on `veyron`, `axum`, `rusqlite`, `reqwest`, or any other kernel-only dependency — it must build standalone in seconds.
- The standalone `~/projects/veyron-core/veyron-sdk-rust` repo's `Cargo.toml` must end this plan with **no `path = ...` key** on its `veyron-wire` dependency — version-only, so `git clone` + `cargo build` works with no sibling checkout.
- Any local-dev override needed before `veyron-wire` is published to crates.io goes in a **gitignored** `.cargo/config.toml` `[patch]` block, never in the committed `Cargo.toml`.

---

### Task 1: Scaffold the `veyron-wire` crate with its error type

**Files:**
- Create: `wire/Cargo.toml`
- Create: `wire/src/lib.rs`
- Create: `wire/src/error.rs`
- Create: `wire/build.rs`
- Move: `proto/veyron_protocol.proto` → `wire/proto/veyron_protocol.proto` (copy for now, keep original until Task 5 removes it)

**Interfaces:**
- Produces: `veyron_wire::error::WireError` enum with variants `Io(std::io::Error)`, `Proto(prost::DecodeError)`, `FrameMagicMismatch`, `FrameCrcMismatch`, `FrameReadTimeout`, `PayloadTooLarge(usize)`, `Timeout`, `PermissionDenied(String)`, `Internal(String)` — mirrors the protocol-level subset of today's `veyron::utils::errors::VeyronError` (`veyron/src/utils/errors.rs`), leaving kernel-only variants (`PluginNotFound`, `PluginAlreadyRegistered`, `InvalidPluginId`, `Incompatible`, `NetworkError`, `CacheError`) out.
- Produces: `veyron_wire::proto::veyron::*` (generated protobuf types), built via `wire/build.rs` using the copied `.proto` file.

- [ ] **Step 1: Write `wire/Cargo.toml`**

```toml
[package]
name = "veyron-wire"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
description = "Veyron wire protocol: framing, frame MAC, and generated protobuf types shared by the kernel and its SDKs."
license = "MIT"
repository = "https://github.com/veyron-core/veyron"

[dependencies]
prost = "0.13"
tokio = { version = "1", features = ["io-util"] }
hmac = "0.12"
sha2 = "0.10"
hkdf = "0.12"
nix = { version = "0.31", features = ["user"] }

[build-dependencies]
prost-build = "0.13"
```

- [ ] **Step 2: Copy the proto file**

```bash
mkdir -p wire/proto
cp proto/veyron_protocol.proto wire/proto/veyron_protocol.proto
```

- [ ] **Step 3: Write `wire/build.rs`**

```rust
fn main() {
    prost_build::compile_protos(&["proto/veyron_protocol.proto"], &["proto/"])
        .unwrap_or_else(|e| panic!("proto codegen failed: {}", e));
}
```

- [ ] **Step 4: Write `wire/src/error.rs`**

```rust
use std::fmt;
use std::io;

#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    Proto(prost::DecodeError),
    FrameMagicMismatch,
    FrameCrcMismatch,
    FrameReadTimeout,
    PayloadTooLarge(usize),
    Timeout,
    PermissionDenied(String),
    Internal(String),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "io error: {}", e),
            WireError::Proto(e) => write!(f, "proto decode error: {}", e),
            WireError::FrameMagicMismatch => write!(f, "frame magic mismatch"),
            WireError::FrameCrcMismatch => write!(f, "frame crc mismatch"),
            WireError::FrameReadTimeout => write!(f, "timed out reading frame body"),
            WireError::PayloadTooLarge(n) => write!(f, "payload too large: {} bytes", n),
            WireError::Timeout => write!(f, "operation timed out"),
            WireError::PermissionDenied(perm) => write!(f, "permission denied: {}", perm),
            WireError::Internal(msg) => write!(f, "internal error: {}", msg),
        }
    }
}

impl std::error::Error for WireError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WireError::Io(e) => Some(e),
            WireError::Proto(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        WireError::Io(e)
    }
}

impl From<prost::DecodeError> for WireError {
    fn from(e: prost::DecodeError) -> Self {
        WireError::Proto(e)
    }
}
```

- [ ] **Step 5: Write `wire/src/lib.rs`**

```rust
pub mod error;
pub mod proto {
    #![allow(clippy::enum_variant_names)]
    pub mod veyron {
        include!(concat!(env!("OUT_DIR"), "/veyron.rs"));
    }
}

pub use error::WireError;
```

- [ ] **Step 6: Confirm it builds standalone**

Run: `cd wire && cargo build 2>&1 | tail -20`
Expected: `Compiling veyron-wire v0.1.0 ...` then a clean `Finished` line, no errors.

- [ ] **Step 7: Commit**

```bash
git add wire/
git commit -m "feat: scaffold veyron-wire crate with WireError and proto codegen"
```

---

### Task 2: Move frame framing into `veyron-wire`, keep `veyron::ipc::framing` as a compatibility shim

**Files:**
- Create: `wire/src/framing.rs` (adapted copy of `veyron/src/ipc/framing.rs`, `VeyronError` → `WireError`)
- Modify: `veyron/src/ipc/framing.rs:1-273` — replace body with a thin shim delegating to `veyron_wire::framing`
- Modify: `veyron/src/utils/errors.rs` — add `impl From<WireError> for VeyronError`
- Modify: `veyron/Cargo.toml` — add `veyron-wire = { path = "wire", version = "0.1.0" }` dependency

**Interfaces:**
- Consumes: `veyron_wire::WireError` (Task 1).
- Produces: `veyron_wire::framing::{Frame, FragmentHeader, MAX_PAYLOAD_SIZE, COMPRESS_THRESHOLD, FLAG_MAC_PRESENT, FLAG_COMPRESSED, FLAG_RAW_BINARY, parse_frag_header, serialize_header, target_as_str, write_frame, write_frame_raw, read_frame, read_frame_with_timeout}` — same names/signatures as today's `veyron::ipc::framing`, but functions return `Result<_, WireError>`.
- Produces: `veyron::ipc::framing::{...}` — **unchanged public signatures**, still returning `Result<_, VeyronError>`, so `veyron/src/ipc/connection.rs` and `veyron/tests/unit/test_framing.rs` require **zero changes**.

- [ ] **Step 1: Copy framing.rs into wire, drop the `VeyronError` import**

```bash
cp veyron/src/ipc/framing.rs wire/src/framing.rs
```

Then in `wire/src/framing.rs`, replace:

```rust
use crate::utils::errors::VeyronError;
```

with:

```rust
use crate::error::WireError;
```

and replace every `VeyronError` occurrence in the file with `WireError` (the enum shape is identical for the variants this file uses: `Io`, `Proto`, `FrameMagicMismatch`, `FrameCrcMismatch`, `FrameReadTimeout`, `PayloadTooLarge`).

- [ ] **Step 2: Register the module in `wire/src/lib.rs`**

```rust
pub mod framing;
```

- [ ] **Step 3: Build `veyron-wire` standalone to confirm framing compiles**

Run: `cd wire && cargo build 2>&1 | tail -20`
Expected: clean `Finished` line.

- [ ] **Step 4: Add the error conversion in the kernel crate**

In `veyron/src/utils/errors.rs`, append:

```rust
impl From<veyron_wire::WireError> for VeyronError {
    fn from(e: veyron_wire::WireError) -> Self {
        use veyron_wire::WireError as W;
        match e {
            W::Io(e) => VeyronError::Io(e),
            W::Proto(e) => VeyronError::Proto(e),
            W::FrameMagicMismatch => VeyronError::FrameMagicMismatch,
            W::FrameCrcMismatch => VeyronError::FrameCrcMismatch,
            W::FrameReadTimeout => VeyronError::FrameReadTimeout,
            W::PayloadTooLarge(n) => VeyronError::PayloadTooLarge(n),
            W::Timeout => VeyronError::Timeout,
            W::PermissionDenied(p) => VeyronError::PermissionDenied(p),
            W::Internal(m) => VeyronError::Internal(m),
        }
    }
}
```

- [ ] **Step 5: Add the path+version dependency to `veyron/Cargo.toml`**

In the `[dependencies]` section, add:

```toml
veyron-wire = { path = "wire", version = "0.1.0" }
```

- [ ] **Step 6: Replace `veyron/src/ipc/framing.rs` with a shim**

```rust
use crate::utils::errors::VeyronError;
use tokio::io::{AsyncRead, AsyncWrite};

pub use veyron_wire::framing::{
    parse_frag_header, serialize_header, target_as_str, Frame, FragmentHeader,
    COMPRESS_THRESHOLD, FLAG_COMPRESSED, FLAG_MAC_PRESENT, FLAG_RAW_BINARY, MAX_PAYLOAD_SIZE,
};

pub async fn write_frame<W>(stream: &mut W, frame: &Frame) -> Result<(), VeyronError>
where
    W: AsyncWrite + Unpin,
{
    veyron_wire::framing::write_frame(stream, frame)
        .await
        .map_err(Into::into)
}

pub async fn write_frame_raw<W>(stream: &mut W, frame: &Frame) -> Result<(), VeyronError>
where
    W: AsyncWrite + Unpin,
{
    veyron_wire::framing::write_frame_raw(stream, frame)
        .await
        .map_err(Into::into)
}

pub async fn read_frame<R>(stream: &mut R) -> Result<Frame, VeyronError>
where
    R: AsyncRead + Unpin,
{
    veyron_wire::framing::read_frame(stream).await.map_err(Into::into)
}

pub async fn read_frame_with_timeout<R>(
    stream: &mut R,
    timeout: std::time::Duration,
) -> Result<Frame, VeyronError>
where
    R: AsyncRead + Unpin,
{
    veyron_wire::framing::read_frame_with_timeout(stream, timeout)
        .await
        .map_err(Into::into)
}
```

Check the exact generic bounds and any private helper types (e.g. a `COMPRESS_THRESHOLD` const or fragment-specific types) referenced by `veyron/src/ipc/connection.rs` before finalizing this shim — grep first:

Run: `grep -n "framing::" veyron/src/ipc/connection.rs`

Add any missing `pub use` re-exports the grep reveals.

- [ ] **Step 7: Full kernel build + test**

Run: `cd veyron && cargo build --all-features 2>&1 | tail -30 && cargo test --all --all-features 2>&1 | tail -60`
Expected: builds clean; `tests/unit/test_framing.rs` and `tests/unit/test_errors.rs` pass unmodified.

- [ ] **Step 8: Commit**

```bash
git add wire/src/framing.rs wire/src/lib.rs veyron/src/ipc/framing.rs veyron/src/utils/errors.rs veyron/Cargo.toml
git commit -m "refactor: move frame framing into veyron-wire, shim veyron::ipc::framing"
```

---

### Task 3: Move frame MAC into `veyron-wire`

**Files:**
- Create: `wire/src/mac.rs` (copy of `veyron/src/auth/frame_mac.rs` — no error-type changes needed, these are pure functions)
- Modify: `veyron/src/auth/frame_mac.rs` — replace body with `pub use veyron_wire::mac::*;`
- Modify: `veyron/src/auth/mod.rs:1-3` — no change needed (still `pub mod frame_mac;`, now the shim)

**Interfaces:**
- Consumes: nothing new — `derive_session_key`, `compute_tag`, `verify_tag`, `SESSION_NONCE_LEN`, `MAC_TAG_LEN` are pure functions/consts with no `VeyronError` dependency (confirmed: `wire/src/framing.rs` and `mac.rs` are independent).
- Produces: `veyron_wire::mac::{derive_session_key, compute_tag, verify_tag, SESSION_NONCE_LEN, MAC_TAG_LEN}`.
- Produces: `veyron::auth::frame_mac::{...}` — unchanged, so `veyron/src/ipc/connection.rs:1,395`, `veyron/src/ipc/protocol.rs:287,330`, and `veyron/src/api/websocket.rs:17` need **zero changes**.

- [ ] **Step 1: Copy frame_mac.rs into wire as mac.rs**

```bash
cp veyron/src/auth/frame_mac.rs wire/src/mac.rs
```

No content changes needed — verify with `grep -n VeyronError wire/src/mac.rs` (expect no output).

- [ ] **Step 2: Register the module**

In `wire/src/lib.rs`, add:

```rust
pub mod mac;
```

- [ ] **Step 3: Shim the kernel module**

Replace the entire contents of `veyron/src/auth/frame_mac.rs` with:

```rust
pub use veyron_wire::mac::*;
```

- [ ] **Step 4: Build and test**

Run: `cd veyron && cargo build --all-features 2>&1 | tail -20 && cargo test --all --all-features 2>&1 | tail -60`
Expected: clean build; `tests/unit/test_framing.rs` (which imports `veyron::auth::frame_mac::{compute_tag, derive_session_key, verify_tag}`) and `tests/integration/test_websocket.rs` pass unmodified.

- [ ] **Step 5: Commit**

```bash
git add wire/src/mac.rs wire/src/lib.rs veyron/src/auth/frame_mac.rs
git commit -m "refactor: move frame MAC into veyron-wire, shim veyron::auth::frame_mac"
```

---

### Task 4: Move `default_socket_path` into `veyron-wire`

**Files:**
- Create: `wire/src/socket.rs` (contains `default_socket_path` and its private helper `default_private_dir`, copied from `veyron/src/utils/config.rs:104-133`)
- Modify: `veyron/src/utils/config.rs:104-133` — remove `default_private_dir`/`default_socket_path`, replace call sites with `veyron_wire::socket::default_socket_path`
- Modify: `veyron/src/utils/config.rs` (wherever `default_pid_path`/`default_log_path` call `default_private_dir()`) — update those two callers to call `veyron_wire::socket::default_private_dir` (make it `pub` in wire, since the kernel still needs it for pid/log paths)

**Interfaces:**
- Produces: `veyron_wire::socket::{default_socket_path() -> String, default_private_dir() -> Option<PathBuf>}`.
- Consumes (by kernel): `veyron/src/utils/config.rs`'s `default_pid_path()` and `default_log_path()` functions call `veyron_wire::socket::default_private_dir()` instead of the local private fn.

- [ ] **Step 1: Write `wire/src/socket.rs`**

```rust
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Resolves the same private runtime directory the kernel uses, so SDKs
/// pick the identical default socket location when `VEYRON_SOCKET_PATH`
/// is unset. Order: `$XDG_RUNTIME_DIR`, `/run/user/<uid>`, `~/.veyron/run`.
pub fn default_private_dir() -> Option<PathBuf> {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime_dir));
    }

    let uid = nix::unistd::Uid::current().as_raw();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if run_user_dir.is_dir() {
        return Some(run_user_dir);
    }

    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(home).join(".veyron").join("run");
        if std::fs::create_dir_all(&dir).is_ok()
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok()
        {
            return Some(dir);
        }
    }

    None
}

pub fn default_socket_path() -> String {
    default_private_dir()
        .map(|dir| dir.join("veyron.sock").to_string_lossy().to_string())
        .unwrap_or_default()
}
```

- [ ] **Step 2: Register the module**

In `wire/src/lib.rs`, add:

```rust
pub mod socket;
```

- [ ] **Step 3: Update `veyron/src/utils/config.rs`**

Delete the local `default_private_dir` and `default_socket_path` functions (lines 104-133). Find every remaining caller of the deleted local functions:

Run: `grep -n "default_private_dir\|default_socket_path" veyron/src/utils/config.rs`

Update `default_pid_path()` and `default_log_path()` (and any `pub use` re-export used elsewhere in the kernel, e.g. `veyron/src/main.rs` or `veyron/src/kernel/`) to call `veyron_wire::socket::default_private_dir()` / add:

```rust
pub use veyron_wire::socket::default_socket_path;
```

at the top of `config.rs` so `crate::utils::config::default_socket_path` keeps working for any existing kernel callers.

- [ ] **Step 4: Build and test**

Run: `cd veyron && cargo build --all-features 2>&1 | tail -20 && cargo test --all --all-features 2>&1 | tail -60`
Expected: clean build and pass. Pay attention to any test asserting on `~/.veyron/run` permission bits (AUDIT M-09 comment) — those should still pass since logic is byte-for-byte identical.

- [ ] **Step 5: Commit**

```bash
git add wire/src/socket.rs wire/src/lib.rs veyron/src/utils/config.rs
git commit -m "refactor: move default_socket_path into veyron-wire"
```

---

### Task 5: Point the kernel's proto codegen at `veyron-wire` and delete the duplicate `.proto`

**Files:**
- Modify: `veyron/Cargo.toml` — remove `prost-build` from `[build-dependencies]` (no longer needed; wire crate owns codegen)
- Delete: `veyron/build.rs`
- Modify: `veyron/src/proto.rs` — replace body with a re-export
- Delete: `proto/veyron_protocol.proto` (the original at repo root — `wire/proto/veyron_protocol.proto` copied in Task 1 is now the single source of truth)
- Check: any other repo-root reference to `proto/veyron_protocol.proto` (docs, other build scripts, the C++ SDK's `CMakeLists.txt` path) — update those in a follow-up, out of scope for this Rust-only plan, but flag them

**Interfaces:**
- Produces: `veyron::proto::veyron::*` — unchanged import path for all existing kernel code (`grep -rn "crate::proto::veyron" veyron/src` / `use veyron::proto::veyron::*` external users), now backed by `veyron_wire::proto::veyron`.

- [ ] **Step 1: Find every consumer of the root proto file before deleting it**

Run: `grep -rln "proto/veyron_protocol.proto" . --include="*.rs" --include="*.toml" --include="CMakeLists.txt" --include="*.md" 2>/dev/null`
Expected output includes `veyron/build.rs` (being deleted this task) and `sdk/cpp/CMakeLists.txt` / `veyron-sdk-cpp/CMakeLists.txt` (out of scope — note but don't touch).

- [ ] **Step 2: Delete `veyron/build.rs`**

```bash
rm veyron/build.rs
```

- [ ] **Step 3: Remove `prost-build` from `veyron/Cargo.toml`**

Delete the `[build-dependencies]` section (it currently contains only `prost-build = "0.13"`).

- [ ] **Step 4: Replace `veyron/src/proto.rs`**

```rust
pub use veyron_wire::proto::veyron;
```

- [ ] **Step 5: Delete the now-duplicate root proto file**

```bash
rm proto/veyron_protocol.proto
```

Leave the `proto/` directory itself only if other non-Rust consumers (C++ SDK) still need a path there — check `sdk/cpp/CMakeLists.txt`'s `PROTO_FILE` variable. If it's the only remaining file in `proto/`, leave the directory for now; the C++ path fix is a separate, already-identified follow-up (not this plan's scope).

- [ ] **Step 6: Build and test**

Run: `cd veyron && cargo build --all-features 2>&1 | tail -30 && cargo test --all --all-features 2>&1 | tail -60`
Expected: clean build (no `OUT_DIR`/prost codegen errors), all tests pass — `veyron::proto::veyron::Envelope` etc. resolve identically through the re-export.

- [ ] **Step 7: Commit**

```bash
git add -A veyron/Cargo.toml veyron/src/proto.rs
git rm veyron/build.rs proto/veyron_protocol.proto
git commit -m "refactor: delegate proto codegen to veyron-wire, drop duplicate .proto"
```

---

### Task 6: Point the core repo's embedded `sdk/rust` copy at `veyron-wire`

**Files:**
- Modify: `veyron/sdk/rust/Cargo.toml` — replace `veyron = { path = "../../", version = "0.1.0" }`-style dep (verify exact current path first) with `veyron-wire = { path = "../../wire", version = "0.1.0" }`
- Modify: `veyron/sdk/rust/src/lib.rs`, `client.rs`, `plugin.rs`, `framing.rs` — swap every `veyron::` import for `veyron_wire::`, and swap `VeyronError` to `veyron_wire::WireError` (re-exported under the same name for source compat)

**Interfaces:**
- Consumes: `veyron_wire::{proto::veyron::*, framing::*, mac::*, socket::default_socket_path, WireError}`.
- Produces: `veyron_sdk::VeyronError` — kept as the public name via `pub use veyron_wire::WireError as VeyronError;` in `lib.rs`, so downstream plugin authors (and `veyron/tests/integration/test_sdk_rust.rs`) see no type-name change. Note: `WireError` does **not** carry `PluginNotFound`/`PluginAlreadyRegistered`/`InvalidPluginId`/`Incompatible`/`NetworkError`/`CacheError` — confirm (grep from Task-0 investigation) that SDK code never constructs those; it only uses `Io`, `Proto`, `FrameMagicMismatch`/`FrameCrcMismatch`/`FrameReadTimeout`, `PayloadTooLarge`, `Timeout`, `Internal`, `PermissionDenied` — all present in `WireError`.

- [ ] **Step 1: Check the exact current path dependency**

Run: `grep -n "veyron" veyron/sdk/rust/Cargo.toml`

- [ ] **Step 2: Update the dependency**

Replace the `veyron = { path = ..., version = "0.1.0" }` line with:

```toml
veyron-wire = { path = "../../wire", version = "0.1.0" }
```

Keep `tokio`, `prost`, `crc32fast` dependencies unchanged.

- [ ] **Step 3: Update imports in each SDK source file**

In `veyron/sdk/rust/src/plugin.rs`:

```rust
use veyron_wire::proto::veyron::{envelope, Envelope, Event, PluginManifest, Pong};
use crate::VeyronError; // re-exported alias, see lib.rs change below
```

Replace the `.unwrap_or_else(|_| veyron::utils::config::default_socket_path())` call with `.unwrap_or_else(|_| veyron_wire::socket::default_socket_path())`.

In `veyron/sdk/rust/src/framing.rs`, replace:

```rust
pub use veyron::ipc::framing::{
```

with:

```rust
pub use veyron_wire::framing::{
```

In `veyron/sdk/rust/src/lib.rs`, replace:

```rust
pub use veyron::utils::errors::VeyronError;
pub use veyron::auth::frame_mac;
pub mod proto {
    pub use veyron::proto::veyron::*;
}
```

with:

```rust
pub use veyron_wire::WireError as VeyronError;
pub use veyron_wire::mac as frame_mac;
pub mod proto {
    pub use veyron_wire::proto::veyron::*;
}
```

In `veyron/sdk/rust/src/client.rs`, replace every:
- `use veyron::auth::frame_mac::{compute_tag, derive_session_key, verify_tag};` → `use veyron_wire::mac::{compute_tag, derive_session_key, verify_tag};`
- `use veyron::ipc::framing::{...};` → `use veyron_wire::framing::{...};`
- `use veyron::proto::veyron::{...};` → `use veyron_wire::proto::veyron::{...};`
- `use veyron::utils::errors::VeyronError;` → `use veyron_wire::WireError as VeyronError;`

All `VeyronError::Io`, `VeyronError::Internal`, `VeyronError::PayloadTooLarge`, `VeyronError::Proto`, `VeyronError::Timeout` construction call sites in `client.rs` need no further change since the alias covers them.

- [ ] **Step 4: Build the embedded SDK standalone**

Run: `cd veyron/sdk/rust && cargo build 2>&1 | tail -40`
Expected: clean build with only `veyron-wire` as a real dependency (no `veyron` in the dependency tree — verify with `cargo tree | grep -c "^veyron v"`, expect `0`... note `veyron-wire`'s own tree line will match `veyron-wire`, so use `cargo tree | grep "^veyron v"` (exact package name, not `veyron-wire`) and expect no output).

- [ ] **Step 5: Rebuild and retest the whole core repo**

Run: `cd veyron && cargo build --all-features 2>&1 | tail -30 && cargo test --all --all-features 2>&1 | tail -80`
Expected: `tests/integration/test_sdk_rust.rs` and `tests/integration/sdk_harness.rs` still pass — they import `veyron_sdk::VeyronClient` and `veyron::proto::veyron::*` from the kernel side, both unaffected by the SDK-internal rewiring.

- [ ] **Step 6: Commit**

```bash
git add veyron/sdk/rust/
git commit -m "refactor: point embedded rust SDK at veyron-wire instead of the kernel crate"
```

---

### Task 7: Decouple the standalone `veyron-sdk-rust` repo from the core checkout

**Files:**
- Modify: `~/projects/veyron-core/veyron-sdk-rust/Cargo.toml`
- Modify: `~/projects/veyron-core/veyron-sdk-rust/src/lib.rs`, `client.rs`, `plugin.rs`, `framing.rs` (same import swap as Task 6 — this repo and `veyron/sdk/rust` are kept in sync as copies today per the diff check already done)
- Create: `~/projects/veyron-core/veyron-sdk-rust/.gitignore` entry for `.cargo/config.toml` (local-dev patch override, never committed)

**Interfaces:**
- Produces: a `Cargo.toml` with `veyron-wire = "0.1.0"` (version-only, **no path key**) — this repo must build with nothing but a `git clone` once `veyron-wire` is on crates.io.

- [ ] **Step 1: Apply the same source edits as Task 6**

Copy the edited files from `veyron/sdk/rust/src/` over `veyron-sdk-rust/src/` (they are meant to be kept identical per the existing `diff -rq` check):

```bash
cp ~/projects/veyron-core/veyron/sdk/rust/src/lib.rs ~/projects/veyron-core/veyron-sdk-rust/src/lib.rs
cp ~/projects/veyron-core/veyron/sdk/rust/src/client.rs ~/projects/veyron-core/veyron-sdk-rust/src/client.rs
cp ~/projects/veyron-core/veyron/sdk/rust/src/plugin.rs ~/projects/veyron-core/veyron-sdk-rust/src/plugin.rs
cp ~/projects/veyron-core/veyron/sdk/rust/src/framing.rs ~/projects/veyron-core/veyron-sdk-rust/src/framing.rs
```

- [ ] **Step 2: Set the version-only dependency**

In `~/projects/veyron-core/veyron-sdk-rust/Cargo.toml`, replace the `veyron = { path = "../veyron", version = "0.1.0" }` line with:

```toml
veyron-wire = "0.1.0"
```

Remove the "Publishing note" comment above it (no longer applicable — `veyron-wire` is a thin crate, publishable immediately, no ordering dependency on the kernel).

- [ ] **Step 3: Add a local-dev-only patch template (not committed as active config)**

Create `~/projects/veyron-core/veyron-sdk-rust/.cargo/config.toml.example`:

```toml
# Copy to .cargo/config.toml (gitignored) to build against an unpublished
# local veyron-wire checkout, e.g. while both repos are being developed
# together before the first crates.io release.
[patch.crates-io]
veyron-wire = { path = "../veyron/wire" }
```

Add to `~/projects/veyron-core/veyron-sdk-rust/.gitignore`:

```
.cargo/config.toml
```

- [ ] **Step 4: Verify standalone build using the local patch (pre-publish state)**

```bash
cp ~/projects/veyron-core/veyron-sdk-rust/.cargo/config.toml.example ~/projects/veyron-core/veyron-sdk-rust/.cargo/config.toml
cd ~/projects/veyron-core/veyron-sdk-rust && cargo build 2>&1 | tail -40
```

Expected: clean build, resolving `veyron-wire` from the local `wire/` path via the patch — proves the crate graph is correct ahead of publishing.

- [ ] **Step 5: Prove the repo has no hard filesystem coupling left**

```bash
rm ~/projects/veyron-core/veyron-sdk-rust/.cargo/config.toml
mv ~/projects/veyron-core/veyron ~/projects/veyron-core/veyron.hidden
cd ~/projects/veyron-core/veyron-sdk-rust && cargo build 2>&1 | tail -20
mv ~/projects/veyron-core/veyron.hidden ~/projects/veyron-core/veyron
```

Expected (until `veyron-wire` is actually published): build **fails** with a crates.io resolution error for `veyron-wire` (not a missing-local-path error) — this is the expected/acceptable state pre-publish, and confirms no remaining path reference to the `veyron` core checkout. Note this explicitly in the commit message.

- [ ] **Step 6: Commit**

```bash
cd ~/projects/veyron-core/veyron-sdk-rust
git add Cargo.toml src/ .cargo/config.toml.example .gitignore
git commit -m "refactor: depend on veyron-wire only, drop path dependency on veyron core"
```

---

### Task 8: Publish `veyron-wire` and cut over the standalone SDK repo to the real version

**Files:**
- Modify: `~/projects/veyron-core/veyron-sdk-rust/Cargo.lock` (regenerated after real publish)

**Interfaces:** none new — this task validates the end state.

- [ ] **Step 1: Dry-run the publish from the core repo**

Run: `cd ~/projects/veyron-core/veyron/wire && cargo publish --dry-run 2>&1 | tail -40`
Expected: no errors about missing metadata (license, description, repository already set in Task 1) or forbidden path dependencies.

- [ ] **Step 2: Publish**

Run: `cd ~/projects/veyron-core/veyron/wire && cargo publish`

(This step touches a shared external registry — confirm with the user before running it for real.)

- [ ] **Step 3: Remove the local patch and rebuild the standalone SDK repo against the published version**

```bash
rm -f ~/projects/veyron-core/veyron-sdk-rust/.cargo/config.toml
cd ~/projects/veyron-core/veyron-sdk-rust && cargo update -p veyron-wire && cargo build 2>&1 | tail -20
```

Expected: clean build resolving `veyron-wire` from crates.io.

- [ ] **Step 4: Re-run the "no sibling checkout" proof from Task 7 Step 5 — this time expect success**

```bash
mv ~/projects/veyron-core/veyron ~/projects/veyron-core/veyron.hidden
cd ~/projects/veyron-core/veyron-sdk-rust && cargo build 2>&1 | tail -20
mv ~/projects/veyron-core/veyron.hidden ~/projects/veyron-core/veyron
```

Expected: clean build with `veyron` core checkout entirely absent — this is the plan's success criterion.

- [ ] **Step 5: Commit the regenerated lockfile**

```bash
cd ~/projects/veyron-core/veyron-sdk-rust
git add Cargo.lock
git commit -m "chore: pin veyron-wire 0.1.0 from crates.io"
```
