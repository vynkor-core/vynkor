# Wire Crate Docs + Non-Rust SDK Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the published `veyron-wire` crate a README, and stop the C++/Python SDKs from reaching into the `veyron` core repo's filesystem layout for the `.proto` file by vendoring a copy into each SDK repo.

**Architecture:** `wire/README.md` documents the crate for its crates.io/docs.rs page. `veyron-sdk-cpp` and `veyron-sdk-python` each get their own `proto/veyron_protocol.proto`, copied verbatim from `wire/proto/veyron_protocol.proto` — the single source of truth stays in the core repo's wire crate, but downstream SDKs no longer need a sibling checkout to build. `veyron-sdk-rust` needs no change; it already depends on `veyron-wire = "0.1.0"` from crates.io.

**Tech Stack:** Rust (crate docs), CMake (C++ SDK build), Python/protoc (no code changes, just a vendored source file).

## Global Constraints

- `veyron-wire`'s crate metadata (license MIT, repository URL) is already set in `wire/Cargo.toml` — the README must not duplicate or contradict it.
- The vendored `.proto` copies in `veyron-sdk-cpp/proto/` and `veyron-sdk-python/proto/` must be byte-identical to `wire/proto/veyron_protocol.proto` at vendor time.
- `veyron-sdk-cpp/CMakeLists.txt`'s `PROTO_FILE` must resolve inside `veyron-sdk-cpp/` — no `../veyron` or any path escaping the SDK repo.
- No FFI, bindings, or Cargo-crate consumption from C++/Python — out of scope per the approved spec (`docs/superpowers/specs/2026-07-04-wire-docs-and-sdk-integration-design.md`).

---

### Task 1: Write `wire/README.md`

**Files:**
- Create: `wire/README.md`

**Interfaces:** None — this is a documentation-only file, no code interfaces.

- [ ] **Step 1: Write the README**

```markdown
# veyron-wire

Shared wire-protocol crate for [Veyron](https://github.com/veyron-core/veyron):
frame framing, frame authentication, socket-path defaults, and the generated
Protobuf types. This is the protocol surface the Veyron kernel and its SDKs
both build on, so a plugin author can depend on `veyron-wire` alone instead
of pulling in the whole kernel crate.

## What's inside

- `framing` — frame read/write over an async stream (`write_frame`,
  `write_frame_raw`, `read_frame`, `read_frame_with_timeout`), fragmentation
  (`FragmentHeader`, `parse_frag_header`), and the wire constants
  (`MAX_PAYLOAD_SIZE`, `COMPRESS_THRESHOLD`, `FLAG_MAC_PRESENT`,
  `FLAG_COMPRESSED`, `FLAG_RAW_BINARY`, `FLAG_FRAGMENTED`).
- `mac` — HMAC-SHA256 frame authentication (`derive_session_key`,
  `compute_tag`, `verify_tag`).
- `socket` — default Unix socket path resolution (`default_socket_path`,
  `default_private_dir`), matching the kernel's `$XDG_RUNTIME_DIR` →
  `/run/user/<uid>` → `~/.veyron/run` fallback order.
- `proto::veyron` — protobuf types generated from `proto/veyron_protocol.proto`
  at build time via `prost-build`.
- `WireError` — the protocol-level error type returned by `framing` and
  `mac` functions.

## Who uses this

- The `veyron` kernel (path dependency, re-exports/wraps this crate's API
  so kernel call sites see no behavior change).
- `veyron-sdk` (Rust), via the published crates.io version.

C++ and Python SDKs can't depend on a Cargo crate directly — they vendor a
copy of `proto/veyron_protocol.proto` instead; see
[`veyron-sdk-cpp`](https://github.com/veyron-core/veyron-sdk-cpp) and
[`veyron-sdk-python`](https://github.com/veyron-core/veyron-sdk-python).

## MSRV

Rust 1.75.

## License

MIT
```

- [ ] **Step 2: Verify the crate still packages cleanly with the README present**

Run: `cd wire && cargo publish --dry-run 2>&1 | tail -30`
Expected: no errors about metadata or packaging; README is picked up automatically (Cargo includes it because `Cargo.toml` has no `readme` key override — confirm `wire/Cargo.toml` doesn't need one added: `grep -n readme Cargo.toml` should show nothing, which is fine, Cargo defaults to `README.md` in the crate root).

- [ ] **Step 3: Commit**

```bash
git add wire/README.md
git commit -m "docs: add README for veyron-wire crate"
```

---

### Task 2: Vendor the proto into `veyron-sdk-cpp` and fix the CMake path

**Files:**
- Create: `~/projects/veyron-core/veyron-sdk-cpp/proto/veyron_protocol.proto` (copy of `wire/proto/veyron_protocol.proto`)
- Modify: `~/projects/veyron-core/veyron-sdk-cpp/CMakeLists.txt:20`
- Modify: `~/projects/veyron-core/veyron-sdk-cpp/README.md`

**Interfaces:**
- Consumes: `wire/proto/veyron_protocol.proto` (copied byte-for-byte, not referenced by path).
- Produces: `veyron-sdk-cpp/proto/veyron_protocol.proto` — new local source that `CMakeLists.txt`'s `protobuf_generate_cpp` call reads.

- [ ] **Step 1: Copy the proto file**

```bash
mkdir -p ~/projects/veyron-core/veyron-sdk-cpp/proto
cp ~/projects/veyron-core/veyron/wire/proto/veyron_protocol.proto ~/projects/veyron-core/veyron-sdk-cpp/proto/veyron_protocol.proto
```

- [ ] **Step 2: Verify the copy is byte-identical**

Run: `diff ~/projects/veyron-core/veyron/wire/proto/veyron_protocol.proto ~/projects/veyron-core/veyron-sdk-cpp/proto/veyron_protocol.proto`
Expected: no output (files identical).

- [ ] **Step 3: Fix the CMake proto path**

In `~/projects/veyron-core/veyron-sdk-cpp/CMakeLists.txt`, replace:

```cmake
# Generate C++ bindings from the shared proto
set(PROTO_FILE ${CMAKE_CURRENT_SOURCE_DIR}/../veyron/proto/veyron_protocol.proto)
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS ${PROTO_FILE})
```

with:

```cmake
# Generate C++ bindings from the vendored proto (sourced from veyron-wire's
# wire/proto/veyron_protocol.proto — re-copy by hand when the protocol changes)
set(PROTO_FILE ${CMAKE_CURRENT_SOURCE_DIR}/proto/veyron_protocol.proto)
protobuf_generate_cpp(PROTO_SRCS PROTO_HDRS ${PROTO_FILE})
```

- [ ] **Step 4: Configure and build to confirm the fix**

Run: `cd ~/projects/veyron-core/veyron-sdk-cpp && cmake -B build -S . 2>&1 | tail -30 && cmake --build build 2>&1 | tail -40`
Expected: configure succeeds (no "proto file not found" error), build compiles the SDK library and tests cleanly. This also proves the previously-broken `../veyron/proto/...` path is no longer needed — confirm by running `mv ~/projects/veyron-core/veyron ~/projects/veyron-core/veyron.hidden && rm -rf build && cmake -B build -S . 2>&1 | tail -30 && mv ~/projects/veyron-core/veyron.hidden ~/projects/veyron-core/veyron` and expect the same clean configure with the core repo entirely absent.

- [ ] **Step 5: Add a "Protocol source" note to the README**

In `~/projects/veyron-core/veyron-sdk-cpp/README.md`, after the intro paragraph (the one ending "...HMAC-SHA256 frame authentication, and fragmentation."), add:

```markdown
## Protocol source

`proto/veyron_protocol.proto` is vendored from
[`veyron-wire`](https://crates.io/crates/veyron-wire)'s `wire/proto/`. It's
copied by hand, not path-referenced — re-sync it when the protocol changes
upstream.
```

- [ ] **Step 6: Commit**

```bash
cd ~/projects/veyron-core/veyron-sdk-cpp
git add proto/ CMakeLists.txt README.md
git commit -m "build: vendor proto instead of reaching into sibling veyron checkout"
```

---

### Task 3: Vendor the proto into `veyron-sdk-python`

**Files:**
- Create: `~/projects/veyron-core/veyron-sdk-python/proto/veyron_protocol.proto` (copy of `wire/proto/veyron_protocol.proto`)
- Modify: `~/projects/veyron-core/veyron-sdk-python/README.md`

**Interfaces:**
- Consumes: `wire/proto/veyron_protocol.proto` (copied byte-for-byte).
- Produces: `veyron-sdk-python/proto/veyron_protocol.proto` — source file to regenerate `veyron/veyron_protocol_pb2.py` from; the checked-in `pb2` file is unchanged (it already matches this content).

- [ ] **Step 1: Copy the proto file**

```bash
mkdir -p ~/projects/veyron-core/veyron-sdk-python/proto
cp ~/projects/veyron-core/veyron/wire/proto/veyron_protocol.proto ~/projects/veyron-core/veyron-sdk-python/proto/veyron_protocol.proto
```

- [ ] **Step 2: Verify the copy is byte-identical**

Run: `diff ~/projects/veyron-core/veyron/wire/proto/veyron_protocol.proto ~/projects/veyron-core/veyron-sdk-python/proto/veyron_protocol.proto`
Expected: no output.

- [ ] **Step 3: Verify the checked-in pb2 still matches this proto (no drift)**

Run:
```bash
cd ~/projects/veyron-core/veyron-sdk-python
python -m grpc_tools.protoc -I proto --python_out=/tmp/pb2check proto/veyron_protocol.proto 2>&1 | tail -20 || pip install grpcio-tools --quiet && python -m grpc_tools.protoc -I proto --python_out=/tmp/pb2check proto/veyron_protocol.proto
diff <(grep -v '^# Protobuf Python Version' /tmp/pb2check/veyron_protocol_pb2.py) <(grep -v '^# Protobuf Python Version' veyron/veyron_protocol_pb2.py)
```
Expected: no diff besides possibly whitespace/version-comment noise already filtered out. If there's a real diff, stop and flag it — it means the checked-in `pb2` predates the current proto and needs regeneration (out of scope for this plan; note it, don't silently regenerate).

- [ ] **Step 4: Add a "Protocol source" note to the README**

In `~/projects/veyron-core/veyron-sdk-python/README.md`, after the intro paragraph (ending "...HMAC-SHA256 frame authentication, and fragmentation."), add:

```markdown
## Protocol source

`proto/veyron_protocol.proto` is vendored from
[`veyron-wire`](https://crates.io/crates/veyron-wire)'s `wire/proto/`. It's
copied by hand, not path-referenced — re-sync it when the protocol changes
upstream, then regenerate `veyron/veyron_protocol_pb2.py`.
```

- [ ] **Step 5: Commit**

```bash
cd ~/projects/veyron-core/veyron-sdk-python
git add proto/ README.md
git commit -m "docs: vendor proto source, document regeneration path"
```

---

### Task 4: Verify `veyron-sdk-rust` needs no change

**Files:** None modified — verification only.

**Interfaces:** None.

- [ ] **Step 1: Confirm the dependency is version-only**

Run: `grep -n veyron-wire ~/projects/veyron-core/veyron-sdk-rust/Cargo.toml`
Expected: `veyron-wire = "0.1.0"` with no `path = ...` key.

- [ ] **Step 2: Confirm a clean build with no dependency on the kernel package**

Run: `cd ~/projects/veyron-core/veyron-sdk-rust && cargo build 2>&1 | tail -30 && cargo tree | grep "^veyron v"`
Expected: build succeeds; the `cargo tree | grep "^veyron v"` command produces **no output** (only `veyron-wire` appears in the tree, never the bare `veyron` kernel package).

- [ ] **Step 3: No commit needed** — this task makes no changes, it's a verification checkpoint confirming Task 6/7 of the prior wire-crate-split plan already left this repo correct.

---

## End-to-end verification

- [ ] Run `cd wire && cargo publish --dry-run 2>&1 | tail -10` — clean.
- [ ] Run `cd ~/projects/veyron-core/veyron-sdk-cpp && rm -rf build && cmake -B build -S . 2>&1 | tail -10 && cmake --build build 2>&1 | tail -20` — clean, no `../veyron` reach.
- [ ] Run `diff ~/projects/veyron-core/veyron/wire/proto/veyron_protocol.proto ~/projects/veyron-core/veyron-sdk-cpp/proto/veyron_protocol.proto` and the same for `veyron-sdk-python` — both empty diffs.
- [ ] Run `cd ~/projects/veyron-core/veyron-sdk-rust && cargo build 2>&1 | tail -10` — clean.
