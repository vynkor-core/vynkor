# Wire Crate Docs + Non-Rust SDK Integration

**Date:** 2026-07-04
**Status:** Approved

## Context

The `veyron-wire` crate (`wire/`) was split out of the kernel repo and is now
published on crates.io (`veyron-wire = "0.1.0"`). `veyron-sdk-rust` already
depends on it correctly (version-only, no path dependency on `veyron` core).

Two gaps remain:

1. **No documentation for the `veyron-wire` crate itself.** It has no
   `README.md`, so its crates.io/docs.rs page is blank.
2. **The C++ and Python SDKs still reach into the core repo's filesystem
   layout for the `.proto` file**, the same coupling the wire-crate split
   was meant to eliminate for Rust:
   - `veyron-sdk-cpp/CMakeLists.txt` hardcodes
     `${CMAKE_CURRENT_SOURCE_DIR}/../veyron/proto/veyron_protocol.proto`.
     That path is now **dead** — Task 5 of the wire-crate-split plan deleted
     `veyron/proto/veyron_protocol.proto` (moved to `wire/proto/`). The C++
     SDK's CMake configure step is currently broken for anyone who doesn't
     have a sibling `veyron` checkout with a stale layout.
   - `veyron-sdk-python` ships a checked-in `veyron_protocol_pb2.py` but has
     no `.proto` source file in the repo at all, so there's nothing to
     regenerate from without reaching into the core checkout by hand.

## Goal

- Give `veyron-wire` a real README so its published crate page is useful.
- Remove the C++ SDK's dependency on a sibling `veyron` core checkout by
  vendoring the proto file, matching what the Rust SDK already achieves via
  the crates.io dependency.
- Give the Python SDK a vendored `.proto` source so `pb2` regeneration
  doesn't require reaching into the core repo either.

Non-goals: no FFI/bindings work, no C++/Python dependency on the Rust crate
itself (not possible — Cargo crates aren't consumable from C++/Python).
Proto stays the shared contract; each SDK vendors its own copy.

## Design

### 1. `wire/README.md` (new file)

Standard crate README, becomes the crates.io/docs.rs landing page:
- One-line description + what problem it solves (shared protocol surface
  between the kernel and its Rust/C++/Python SDKs).
- What's inside: `framing` (frame read/write, fragmentation, zstd
  compression threshold), `mac` (HMAC frame authentication), `socket`
  (default socket path resolution), `proto::veyron` (generated protobuf
  types), `WireError`.
- Who consumes it: `veyron` kernel, `veyron-sdk` (Rust, via crates.io).
- MSRV (1.75) and license (MIT) footer.

No rustdoc `///` comments and no root-repo README changes — scope is the
crate README only, per user decision.

### 2. `veyron-sdk-cpp`

- Copy `wire/proto/veyron_protocol.proto` → `veyron-sdk-cpp/proto/veyron_protocol.proto`.
- Edit `CMakeLists.txt`: `PROTO_FILE` becomes
  `${CMAKE_CURRENT_SOURCE_DIR}/proto/veyron_protocol.proto`. Drops the
  `../veyron/` reach entirely — fixes the currently-broken configure step.
- Add a short "Protocol source" note to `veyron-sdk-cpp/README.md`: the
  `.proto` is vendored from `veyron-wire`'s `wire/proto/`, re-sync by hand
  when the protocol changes (no automated sync in this pass).

### 3. `veyron-sdk-python`

- Copy `wire/proto/veyron_protocol.proto` → `veyron-sdk-python/proto/veyron_protocol.proto`.
- Add the same "Protocol source" note to `veyron-sdk-python/README.md`.
- No change to the checked-in `veyron_protocol_pb2.py` — it already matches
  this proto (same source content), this just gives the repo a source file
  to regenerate from next time instead of nothing.

### 4. `veyron-sdk-rust`

No change. Already depends on `veyron-wire = "0.1.0"` from crates.io with no
path dependency. Verify `cargo build` is clean as a sanity check, nothing
else.

## Testing / Verification

- `cd wire && cargo publish --dry-run` still succeeds with README present
  (metadata unaffected, but sanity-check no packaging errors).
- `cd veyron-sdk-cpp && cmake -B build` configures cleanly using the new
  vendored proto path (no reach into `../veyron`).
- `veyron-sdk-python/proto/veyron_protocol.proto` diffed against
  `wire/proto/veyron_protocol.proto` — byte-identical at vendor time.
- `veyron-sdk-rust`: `cargo build` clean, `cargo tree | grep "^veyron v"`
  empty (no dependency on the `veyron` kernel package).
