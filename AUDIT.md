# Veyron Kernel — Full Project Audit

**Date:** 2026-07-02
**Branch:** `develop` · Commit: `d21c842` (post-Phase-4)
**Method:** Full read of all Rust kernel sources, all three SDKs, proto contract, docs, tests. Verification runs: `cargo clippy --all-targets --all-features` (clean), `cargo test --all --all-features` (263 passed, 0 failed), `cargo fmt --check` implied clean by CI history.

---

## Executive Summary

Phase 4 fixed BUG-001..006 **on the kernel and Rust-SDK side only**. The zstd compression feature (`FLAG_COMPRESSED`, auto-applied to any payload ≥ 64 KiB) is still **not implemented in the Python and C++ SDKs**: any kernel→plugin frame at or above the threshold breaks non-Rust plugins — either MAC verification fails (secured kernel) or protobuf parsing fails (unsecured). ROADMAP.md acknowledges the Python half of this; the C++ half is unrecorded.

Beyond that, the kernel core is in good shape: framing normalization, fragmentation caps, MAC ordering, socket hygiene, and supervisor lifecycle are all correct and regression-tested. The remaining findings are permission gaps (`KernelCommand` unauthenticated relative to permissions), a broken CLI command (`vyn plugin start` hits a nonexistent route), a CLI that cannot talk to a secured kernel at all, incomplete features (`ActionRequest` is a stub, `AudioStreamChunk` envelope unhandled), and significant documentation drift (README/FRAMING.md/CLAUDE.md all contradict the code in places).

| Dimension | Score | Note |
|-----------|-------|------|
| Core architecture | 9/10 | Dumb router holds; double-compile of crate via `main.rs` mods |
| Binary framing (kernel/Rust) | 9/10 | Compression+MAC normalization correct, tested |
| Cross-SDK protocol parity | **3/10** | Python & C++ cannot receive compressed frames; WS inbound asymmetric |
| Auth & permissions | 7/10 | `KernelCommand` ungated; REST has auth but no authz granularity |
| Lifecycle / supervision | 9/10 | Per-plugin grace, watchdog escalation correct |
| CLI | 5/10 | `plugin start` 404s; no JWT/TLS support against secured kernel |
| Docs | 4/10 | FRAMING.md, README, CLAUDE.md, ROADMAP all stale in places |
| **Overall** | **~78/100** | Fix SDK compression parity before any release |

---

## 1. Critical Bugs

### C-01 — Python SDK cannot receive compressed frames (Critical)

**Location:** `sdk/python/veyron/framing.py:84-119` (`read_frame`, `async_read_frame`)

The kernel's `write_frame_raw` (`src/ipc/framing.rs:135-145`) transparently zstd-compresses any outbound payload ≥ 64 KiB and sets `FLAG_COMPRESSED`. The Python SDK:

1. Never decompresses — `FLAG_COMPRESSED` is defined (`framing.py:12`) but never checked on read. The compressed bytes are handed to `Envelope.ParseFromString` → `DecodeError`.
2. On a secured kernel it fails earlier: the kernel computes the HMAC over the **plaintext** header/payload (pre-compression, `src/ipc/connection.rs:321-326`), but Python verifies against the **wire** header (compressed length/flags/CRC) and compressed payload → `ValueError("MAC verification failed")` → plugin loop dies.

**Failure scenario:** kernel publishes an `Event` whose `payload_json` ≥ 64 KiB to a Python subscriber → subscriber crashes or disconnects. Silent in the kernel (frame was sent fine).

**Fix:** in both read paths: after CRC check, if `flags & FLAG_COMPRESSED`: decompress (`zstandard` package, bounded to `MAX_PAYLOAD`), then reconstruct the plaintext header (`flags & ~FLAG_COMPRESSED`, plaintext length, plaintext CRC) and verify the MAC against that — mirroring `src/ipc/framing.rs:230-243`. Add a cross-SDK integration test with a ≥ 64 KiB payload (see §4).

### C-02 — C++ SDK: same defect (Critical)

**Location:** `sdk/cpp/src/framing.cpp:121-169` (`read_frame_full`)

Identical to C-01: no decompression, MAC verified against the raw wire header (`framing.cpp:159-164`). `FLAG_COMPRESSED` is declared in `framing.hpp` but never consumed. Same fix (link libzstd, normalize before verify). Unlike the Python half, this is **not** recorded in ROADMAP.md.

### C-03 — WebSocket gateway inbound: no decompression, no fragmentation (High, borderline critical)

**Location:** `src/api/websocket.rs:168-212` (`parse_frame`)

The UDS read path normalizes `FLAG_COMPRESSED` and reassembles `FLAG_FRAGMENTED` frames (`src/ipc/connection.rs:132-272`); the WS gateway does neither. A WS client that sends a compressed or fragmented frame gets it routed as-is: the router protobuf-decodes compressed bytes (decode error → error budget) or treats the fragment header as payload. Nothing documents that WS clients must not use these flags.

**Fix:** either implement both in `handle_socket` (share the reassembly logic from `connection.rs`) or reject frames carrying `FLAG_COMPRESSED`/`FLAG_FRAGMENTED` at `parse_frame` with a clear error, and document the restriction in `docs/FRAMING.md`.

---

## 2. High Severity

### H-01 — `KernelCommand` has no permission gate

**Location:** `src/ipc/protocol.rs:408-423`

`ActionRequest` is permission-checked (`protocol.rs:374-387`), but `KernelCommand` dispatches directly. Any registered plugin — regardless of manifest/JWT permissions — can invoke `reload_config` (re-reads the config file, changes kernel log level) and `health_check`. On a JWT-secured kernel, a plugin with an empty permission list still controls kernel config reload.

**Fix:** add a permission (e.g. `PERMISSION_KERNEL_ADMIN`, next free enum value 12 in `proto/veyron_protocol.proto`) and check it before `CommandHandler::dispatch`. `health_check` may stay open or move behind the same gate.

### H-02 — `vyn plugin start` calls a route that does not exist

**Location:** `src/cli/plugin.rs:71-74` vs `src/api/server.rs:64-70`

CLI POSTs `/plugins/{id}/start`; the router only registers `stop`, `restart`, `logs`. Every invocation returns "API error: HTTP 404". No test covers this command (which is why it survived).

**Fix:** add a `POST /plugins/:id/start` route that resolves the plugin from `config.yaml` `plugins:` and calls `PluginManager::start`, or remove the CLI subcommand. Add a route test either way.

### H-03 — CLI cannot talk to a secured kernel

**Location:** `src/cli/plugin.rs:177-200` (`api_get`, `api_post`)

No `Authorization: Bearer` header is ever attached, and the URL is hardcoded `http://`. With `jwt_secret` set (the documented production posture), every `vyn plugin stop/restart/logs` gets 401; with TLS enabled, connection fails outright. The CLI is effectively dev-mode-only, which contradicts README's "JWT validation is mandatory" stance.

**Fix:** accept a token (flag / env var / config field), attach the header, honor `tls_cert_path`-implied `https://`. Also: `Commands::Plugin` hardcodes `config.yaml` (`src/main.rs:109`) while every other subcommand takes `--config`.

### H-04 — SDK plugin harnesses cannot run against a secured kernel

**Locations:** `sdk/python/veyron/plugin.py:17-18`, `sdk/rust/src/plugin.rs:19-27`

- Python `Plugin.__init__` constructs `VeyronClient(socket_path)` with **no secret**, so even when `jwt_token` is supplied and registration succeeds, no session key is derived. The kernel then requires MACs on every subsequent inbound frame (`src/ipc/connection.rs:137-156`) — the plugin's first post-registration send drops the connection.
- Rust `Plugin::run` calls `client.register()` with an **empty token** and `connect()` without a secret — registration itself is rejected when auth is on.

The base-class abstraction (the documented "SDK Pattern" in CLAUDE.md) only works with `allow_no_auth: true`.

**Fix:** thread `secret`/`jwt_token` through both plugin base classes (constructor params or env vars, e.g. `VEYRON_JWT_TOKEN`/`VEYRON_SECRET`), and add secured-mode SDK harness integration tests.

### H-05 — `ActionRequest` is a stub: permissions checked, nothing executed

**Location:** `src/ipc/protocol.rs:366-406`

The handler maps action → permission, checks it, then returns `ACTION_OK` with empty `data_json`. `params_json` and `timeout_ms` are ignored. A plugin calling `read_file` gets a success response with no data. The proto (`veyron_protocol.proto:109-129`) and `action_to_permission`'s eight action families imply real execution.

**Fix:** decide the architecture: (a) kernel executes built-in actions (contradicts "dumb core" manifesto), or (b) actions are routed to provider plugins declaring them in `manifest.actions` (currently only logged, `src/plugins/loader.rs:122-126`). If (b), implement action routing + response correlation via `action_id`; until then return `ACTION_NOT_FOUND` instead of a misleading `ACTION_OK`.

### H-06 — README documents wrong CRC failure behavior

**Location:** `README.md` ("CRC32 — Corrupt frame → connection intact, frame dropped") vs `src/ipc/connection.rs:274-277`

The code **drops the connection** on CRC mismatch (`break`). Plugin authors reading README will implement retry logic against a connection that no longer exists. One of the two is wrong; pick and align (current code behavior is defensible — corruption on a UDS is not line noise).

---

## 3. Medium Severity

### M-01 — Rate-limiter key maps grow without bound

**Locations:** `src/ipc/protocol.rs:89-91` (keyed by `conn_id`), `src/api/rate_limit.rs:16-21` (keyed by JWT `sub`)

`governor`'s keyed limiter retains state per key forever unless `retain_recent()` is called. Conn IDs are monotonic — every connection ever made leaves an entry; attacker-supplied `sub` values persist too. Slow memory leak on long-running kernels.

**Fix:** periodic `limiter.retain_recent()` (e.g. piggyback on the watchdog tick).

### M-02 — Duplicate fragment sequence double-counts `buffered_bytes`

**Location:** `src/ipc/connection.rs:235-236`

`entry.fragments.insert(seq, data)` overwrites an existing fragment, but `buffered_bytes += len` was already added for the first copy. Repeated resends of the same sequence inflate the counter until the `MAX_PAYLOAD_SIZE` guard falsely trips and drops the connection.

**Fix:** `if let Some(old) = entry.fragments.insert(...) { entry.buffered_bytes -= old.len(); }` or reject duplicate sequences outright (stricter, arguably better).

### M-03 — Resource limits only apply with `sandbox: true`, and are hardcoded

**Location:** `src/plugins/runner.rs:17-28`, applied only from `src/plugins/supervisor.rs:139-146`

`RLIMIT_NPROC=64` / `RLIMIT_AS=512MiB` are constants, and a plugin spawned without `sandbox: true` (the default) has **no** limits. CLAUDE.md's troubleshooting table ("Plugin leaks memory → supervisor.rs — resource limits enforced?") implies they're always on. README §5 says limits apply "with sandbox: true" — correct but easy to miss.

**Fix:** split `apply_resource_limits` from namespace isolation (it already is a separate fn — call it unconditionally in `pre_exec`), and expose limits in `PluginDef`.

### M-04 — `forward()` keeps the sender's MAC flag/tag; `broadcast()` strips it

**Location:** `src/ipc/protocol.rs:497-500` vs `:566-576`

Broadcast rebuilds the frame with `flags & !FLAG_MAC_PRESENT, mac: None` (with a comment explaining why); unicast forward sends `msg.frame` untouched. Today the target's write loop re-tags when secured, masking it — but on a mixed/unsecured path the stale flag+tag from the sender's session key goes on the wire. Latent inconsistency.

**Fix:** normalize in `forward()` exactly as `broadcast()` does.

### M-05 — Dead protocol surface: version negotiation, audio envelope, MAC error codes

**Locations:** `proto/veyron_protocol.proto`, `src/ipc/protocol.rs:432-435`

- `Envelope.version` (field 4) is never set or checked; `ERR_PROTOCOL_MISMATCH` never emitted.
- `AudioStreamChunk` (field 60) has no kernel handler — a plugin sending it to `"kernel"` gets "unhandled message" and burns error budget. (Peer-to-peer routing of it works since the kernel doesn't decode forwarded payloads.)
- `ERR_MAC_MISSING`/`ERR_MAC_INVALID` (proto v1.2 additions) are defined but the kernel silently drops the connection instead of sending them.
- `PluginManifest.needs_gpu`, `priority`, `PluginRegister.version/description` are parsed and ignored.

**Fix:** implement or `reserved`-retire each. Minimum: reject unknown-to-kernel payloads with a specific error; document that MAC failures are fail-silent by design.

### M-06 — Marketplace: unbounded download and zip-bomb exposure

**Location:** `src/marketplace/installer.rs:209-218` (whole archive buffered in RAM, no size cap), `installer.rs:224-288` (`extract_zip` has path-traversal and symlink protection but no decompressed-size or entry-count cap)

A malicious/compromised registry entry can OOM the CLI host or fill the disk. SHA-256 verification happens **after** the full download.

**Fix:** cap download size (e.g. 256 MiB, checked against `content_length` and during streaming), cap cumulative extracted bytes and entry count in `extract_zip`.

### M-07 — Whole crate compiles twice; dead-code warnings papered over

**Location:** `src/main.rs:1-11` re-declares `mod api; mod auth; ...` instead of using the `veyron` library crate

Consequences: every module compiles twice (lib + bin), unit tests in `src/` run twice (30 duplicated tests in the `--all` run), binary bloat, and ~20 `#[allow(dead_code)]` attributes exist mainly to silence cross-target false positives — which also hides genuinely dead code (e.g. `write_frame` in `framing.rs:92` is only used by SDK re-export; `PluginState` has one variant).

**Fix:** `main.rs` → `use veyron::{cli, kernel, utils};`, delete the `mod` block, then remove now-unnecessary `#[allow(dead_code)]` and let the compiler find real dead code.

### M-08 — Registry registration is not atomic

**Location:** `src/plugins/registry.rs:47-77`

`contains_key` checks on two DashMaps followed by three inserts — a TOCTOU if `register` is ever called concurrently. Today it's safe only because the router is single-threaded; nothing enforces that invariant at the API boundary.

**Fix:** use `DashMap::entry` on `by_plugin_id` as the linearization point, insert `by_conn_id` under it, or document the single-caller invariant on the method.

### M-09 — Default `pid_file`/`log_file` in world-writable `/tmp`

**Location:** `src/utils/config.rs:138-140`, `config.yaml:3-5`, `src/main.rs:179-182,225-229`

`fs::write`/`File::create` follow symlinks; a predictable `/tmp/veyron.pid` path allows a local user to redirect the write (classic symlink attack) or spoof the PID that `vyn stop` will SIGTERM. Socket path was already fixed for this exact class (BUG-006) — pid/log files were not.

**Fix:** default under `$XDG_RUNTIME_DIR`/`~/.veyron` like `default_socket_path()` does; open pid file with `O_NOFOLLOW`.

### M-10 — SDK default socket path contradicts kernel default

**Locations:** `sdk/python/veyron/plugin.py:17`, `sdk/rust/src/plugin.rs:21`, `config.yaml` comment line 44

Fallback is `/tmp/veyron.sock` — a path the kernel now refuses to default to (BUG-006 fix, `src/utils/config.rs:95-119`). A plugin started manually without `VEYRON_SOCKET_PATH` connects to a socket that will never exist (or worse, to an attacker's socket in `/tmp`).

**Fix:** mirror the kernel's XDG-based resolution in both SDKs; update the `config.yaml` comment.

---

## 4. Test Gaps

| Gap | Risk | Suggested test |
|-----|------|----------------|
| **No cross-SDK compressed-frame test** — `tests/integration/test_sdk_python.rs` / `test_sdk_cpp.rs` never push a ≥ 64 KiB payload | Would have caught C-01/C-02 | Round-trip a 100 KiB event kernel→Python and kernel→C++, secured and unsecured |
| WS gateway: no test sending `FLAG_COMPRESSED`/`FLAG_FRAGMENTED` frames | C-03 unnoticed | Assert defined behavior (reject or handle) |
| `vyn plugin start` has zero coverage | H-02 shipped broken | Route-level test for every CLI-invoked endpoint |
| `KernelCommand` authorization | H-01 | Test that a permissionless plugin cannot `reload_config` (currently would fail — write after fix) |
| Duplicate fragment sequence | M-02 | Send same `seq` twice; assert no spurious disconnect |
| Rate-limiter memory growth | M-01 | Not easily testable; at least assert `retain_recent` is wired |
| SDK harness under auth | H-04 | Secured-kernel variant of `sdk_harness.rs` |
| SIGHUP config reload (`src/kernel/orchestrator.rs:39-50`) | untested path | Integration test flipping log level via SIGHUP |
| `graceful_shutdown` when a plugin ignores SIGTERM | SIGKILL-after-grace path only unit-approximated | Soak-style test with a SIGTERM-ignoring child |

Strengths worth keeping: fragmentation/MAC/BUG-00x regression tests are thorough; five fuzz targets cover framing, envelope decode, and the router pipeline.

---

## 5. Documentation Drift

| Doc | Claim | Reality |
|-----|-------|---------|
| `docs/FRAMING.md:11` | `FLAG_COMPRESSED` "reserved, **not yet implemented**" | Fully implemented and auto-applied ≥ 64 KiB (`src/ipc/framing.rs:135`) — this "single source of truth" being stale is plausibly *why* the SDKs were never updated |
| `docs/FRAMING.md:12` | `FLAG_FRAGMENTED` "(reserved)" | Implemented with reassembly, caps, timeouts |
| `README.md` | "Corrupt frame → connection intact, frame dropped" | Connection is dropped (H-06) |
| `CLAUDE.md:20` | "IPC: Unix sockets / **Named pipes**" | UDS only; named pipes appear nowhere |
| `CLAUDE.md:74,125` | Critical file `src/kernel/kernel.rs` | File doesn't exist — it's `src/kernel/orchestrator.rs` |
| `CLAUDE.md:132-133` | `docs/VEYRON_ARCHITECTURE.md`, `docs/ROADMAP_v2.md` | Both moved to `docs/archive/` |
| `docs/veyron_protocol.proto` | duplicate of the contract | **Diverged** from `proto/veyron_protocol.proto` (`diff` non-empty); one stale copy of the "single source of truth" |
| `ROADMAP.md:29-33` | "B-01/B-02 failing tests", "Audit score pending" | All 263 tests pass; B-01/B-02 fixed |
| `proto/veyron_protocol.proto:133-135` | `KernelCommand` "ЯДРО → ПЛАГИН" (kernel→plugin) | Implemented plugin→kernel (`protocol.rs:408`) |

---

## 6. Dependency Issues

- **Unused:** `chrono` and `prost-types` have zero references in `src/`, `tests/`, or SDKs — remove from `Cargo.toml`. `tower-http` is used only for `timeout`; the `cors` feature is dead — trim to `features = ["timeout"]`.
- **Dead build script:** `proto/build.rs` is orphaned (root `build.rs` does the codegen; `proto/` is not a crate). CLAUDE.md still references it.
- **Aging (no conflicts, upgrade when convenient):** `axum 0.7` (0.8 is current), `nix 0.28`, `rusqlite 0.31`, `opentelemetry 0.24` stack (API churn upstream — pin-and-plan), `metrics 0.23`/`exporter 0.15`, `governor 0.8`. `cargo tree -d` shows only benign `bytes`/hyper-family sharing — no version conflicts.
- **Python SDK:** if C-01 is fixed, `zstandard` becomes a dependency of `sdk/python/pyproject.toml`; C++ needs libzstd in `CMakeLists.txt`.

---

## 7. Inconsistencies & Technical Debt

- **Dead files:** `src/ipc/client.rs` and `src/kernel/config.rs` are **0 bytes** and not declared in any `mod.rs`. Delete.
- **Test-only code in prod build:** `create_test_token` (`src/auth/jwt.rs:40-64`) compiles into the release binary behind `#[allow(dead_code)]`. Move behind `#[cfg(any(test, feature = "test-util"))]`.
- **Per-frame payload clone:** `write_frame_raw` clones the payload even when not compressing (`src/ipc/framing.rs:141-144`) — up to 1 MiB memcpy per frame on the hot path. Restructure to borrow (`Cow` or write header+payload separately in the uncompressed branch). Same pattern: `broadcast` clones the full payload per subscriber (`protocol.rs:568-576`) — an `Arc<[u8]>` payload would fix both.
- **Error-code misuse:** `forward`/`broadcast` denials send `ErrUnknown` with a text message (`protocol.rs:453-459,531-536`) while the audio gate correctly uses `ErrPermissionDenied`. Use `ErrPermissionDenied` consistently.
- **`CLK_TCK` hardcoded to 100** (`src/plugins/supervisor.rs:465`) — true on mainstream kernels, wrong on some configs; `sysconf(_SC_CLK_TCK)` is one call away.
- **`orchestrator.rs` hardcodes `GRACE_SECONDS: u32 = 5`** for the `PluginShutdown` notice (`orchestrator.rs:250`) while the supervisor honors per-plugin `grace_seconds` — the advertised grace can disagree with the enforced one.
- **Env-var mutation in tests:** `src/utils/config.rs:172-193` sets/removes `XDG_RUNTIME_DIR` in-process — racy under parallel test execution (works today by luck of scheduling).
- **Python `__init__.py` silently degrades:** import failure sets `VeyronClient = None` (`sdk/python/veyron/__init__.py`) — callers get `TypeError: 'NoneType' is not callable` far from the cause. Re-raise with an actionable message instead.
- **Event `event_id` reuse:** system events use deterministic ids (`sys-joined-{id}`, `orchestrator.rs:315`); `INSERT OR IGNORE` (`store.rs:40`) means a re-registration within the 1 h retention window is never re-persisted — at-least-once delivery silently downgraded for repeat events. Suffix a timestamp/counter.
- **Naming:** two different `PluginEntry` types (`plugins/registry.rs:14` vs `marketplace/registry.rs:16`) and two `PluginManifest`s (proto vs `marketplace/installer.rs:27`) — confusing at call sites; rename the marketplace pair (`RegistryEntry`, `InstallManifest`).

---

## Quick Wins (< 5 min each)

> **Status 2026-07-02: all 12 executed** (same day as this audit). Verified with
> `cargo clippy --all-targets --all-features` (clean) and `cargo test --all --all-features`
> (263 passed). Item 12 became the full ROADMAP.md regeneration. Remaining findings are
> tracked as Phase 5 items R5-01..R5-16 in `ROADMAP.md`.

1. Delete 0-byte `src/ipc/client.rs` and `src/kernel/config.rs`.
2. Delete orphaned `proto/build.rs`.
3. Remove `chrono`, `prost-types` from `Cargo.toml`; trim `tower-http` to `["timeout"]`.
4. Delete stale `docs/veyron_protocol.proto` (or replace with a symlink/README pointer to `proto/`).
5. Fix `docs/FRAMING.md` flag table: mark `FLAG_COMPRESSED`/`FLAG_FRAGMENTED` implemented.
6. Fix README CRC-behavior sentence (H-06).
7. CLAUDE.md: `kernel.rs` → `orchestrator.rs`, drop "Named pipes", fix archived-doc paths.
8. `ErrUnknown` → `ErrPermissionDenied` in `forward`/`broadcast` denials.
9. SDK default socket fallback `/tmp/veyron.sock` → XDG path (both SDKs + `config.yaml` comment).
10. Guard `create_test_token` behind `#[cfg(any(test, feature = "test-util"))]`.
11. `graceful_shutdown`: pass the real per-plugin grace into the `PluginShutdown` message (or a config value instead of the `5` literal).
12. ROADMAP.md baseline table: update to 263 passing, B-01/B-02 done.

## Major Rework

1. **SDK compression parity (C-01/C-02/C-03)** — implement zstd decompress + plaintext-header MAC normalization in Python and C++ SDKs; define WS gateway behavior for compressed/fragmented inbound; add ≥ 64 KiB cross-SDK integration tests. *This blocks any release; until then, consider a kernel kill-switch config flag to disable outbound compression.*
2. **Secured-mode SDK/CLI story (H-03/H-04)** — thread JWT + secret through SDK plugin base classes and the CLI; add secured-kernel integration coverage. Today the documented production posture (JWT on) is unusable by the shipped tooling.
3. **Action system (H-05)** — decide kernel-executed vs plugin-routed actions and implement, or excise `ActionRequest` machinery from proto/SDKs.
4. **KernelCommand authorization (H-01)** — new permission + proto bump + SDK updates.
5. **De-duplicate crate compilation (M-07)** — `main.rs` consumes the lib crate; sweep `#[allow(dead_code)]` afterwards.
6. **Zero-copy hot path** — `Arc`-based payloads through router/broadcast/write path (M-03-adjacent perf debt).
