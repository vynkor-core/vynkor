# Veyron ROADMAP v3

> **Manifesto (non-negotiable):**
> - Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
> - Intra-host IPC = UDS only. No TCP, no Redis, no queues.
> - Protocol = single `.proto` file. Changes there propagate to all SDKs.
> - Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
> - External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-06-30

| Metric | Value |
|--------|-------|
| Audit score | 85/100 |
| Open VULNs | 0 (VULN-001 – VULN-022 resolved) |
| Open AUDIT items | AUDIT-002 through AUDIT-005 (AUDIT-001 closed by T-01) |
| CLI commands | start, stop, restart, status, logs |
| Flag constants | FLAG_MAC_PRESENT (Bit 0), FLAG_RAW_BINARY (Bit 4) — canonical in docs/FRAMING.md |
| Marketplace | Not implemented |
| Audio streaming | Not implemented |

## Phase 2.1 progress — 2026-06-30

| Task | Status | Notes |
|------|--------|-------|
| T-01 | ✅ Done | `docs/FRAMING.md` created; `FLAG_RAW_BINARY = 0x0010` in kernel + all 3 SDKs; AUDIT-001 closed |
| T-02 | ✅ Done | WS JWT `Sec-WebSocket-Protocol` deviation documented in `docs/FRAMING.md` |
| T-03 | ✅ Done | `PluginSupervisor::graceful_shutdown(grace_seconds)` added; hardcoded 200ms removed; `grace_seconds` in `PluginConfig` + `config.yaml` |

---

## Phase 2.1 — Flag Space Canonicalization

**Goal:** Single authoritative flag table. Audio routing unblocked. AUDIT-001 closed.

**Done-when:**
- `docs/FRAMING.md` exists and is the sole flag reference
- `FLAG_RAW_BINARY` exported from `src/ipc/framing.rs` and all three SDKs
- `cargo test --all-features` passes
- No flag constant is defined anywhere except `framing.rs` (all SDKs import, not redefine)

---

### T-01 — Canonicalize flag bit space (AUDIT-001)

**Files:** `src/ipc/framing.rs`, `sdk/rust/src/framing.rs`, `sdk/cpp/include/veyron/framing.hpp`, `sdk/python/veyron_sdk/framing.py`, `docs/FRAMING.md` (new)

**What to do:**

Create `docs/FRAMING.md` as the single source of truth for all flag bits:

```
Bit  0  (0x0001)  FLAG_MAC_PRESENT    — 32-byte HMAC-SHA256 tag appended after payload
Bit  1  (0x0002)  FLAG_COMPRESSED     — payload compressed with zstd (reserved, not yet impl)
Bit  2  (0x0004)  FLAG_FRAGMENTED     — frame is one fragment of a larger message (reserved)
Bit  3  (0x0008)  FLAG_PRIORITY       — high-priority system frame (reserved)
Bit  4  (0x0010)  FLAG_RAW_BINARY     — payload is raw bytes (PCM or Opus); skip Protobuf parse
Bits 5–15         reserved
```

Add to `src/ipc/framing.rs`:
```rust
/// Payload is raw binary (PCM/Opus audio). Router skips Protobuf decode.
pub const FLAG_RAW_BINARY: u16 = 0x0010;
```

Mirror the constant (value only, no logic) in each SDK.

**Acceptance test:** `grep -r "0x0010\|FLAG_RAW_BINARY" sdk/` returns hits in all three SDKs. `cargo test --all-features` clean.

---

### T-02 — Document WS JWT delivery deviation (AUDIT-002)

**Files:** `docs/FRAMING.md`

**What to do:** Add a section to `docs/FRAMING.md`:

> **WebSocket JWT delivery:** The Veyron manifesto originally specified `?token=<jwt>` as
> the URL query parameter for WebSocket auth. The implementation uses
> `Sec-WebSocket-Protocol: veyron, <jwt>` instead. This is intentional: tokens in URL
> query strings appear in server access logs, browser history, and proxy logs.
> The header approach is superior. The manifesto text is superseded by this document.
> Third-party clients must use the `Sec-WebSocket-Protocol` header.

No code change.

**Acceptance test:** `docs/FRAMING.md` contains the string "Sec-WebSocket-Protocol".

---

### T-03 — Configurable shutdown grace period (AUDIT-003)

**Files:** `src/plugins/supervisor.rs`

**What to do:** `PluginShutdown.grace_seconds` already exists in proto (field 2). The supervisor
must read it. In `graceful_shutdown()`:

```rust
// Read grace_seconds from PluginShutdown proto; default 5s if zero.
let grace = if shutdown_msg.grace_seconds > 0 {
    Duration::from_secs(shutdown_msg.grace_seconds as u64)
} else {
    Duration::from_secs(5)
};
tokio::time::sleep(grace).await;
```

Remove the hardcoded `200ms` wait. Add `grace_seconds` to `config.yaml` plugin entry:
```yaml
plugins:
  - id: my-plugin
    grace_seconds: 10   # optional, default 5
```

**Acceptance test:** Integration test sends `PluginShutdown { grace_seconds: 1 }`, verifies
supervisor waits ~1s (±200ms) before SIGKILL.

---

## Phase 2.2 — Audio Stream Protocol

**Goal:** Kernel routes raw audio frames between plugins. Kernel never decodes audio payload.
Permission check enforced before routing.

**Done-when:**
- `AudioStreamChunk` in proto, fields reserved for `ai_*` not reused
- `PERMISSION_AUDIO_STREAM` in `PermissionType` enum
- Routing of `FLAG_RAW_BINARY` frames blocked for plugins without permission
- Integration test: unpermissioned plugin sends audio frame → `ERR_PERMISSION_DENIED`

---

### T-04 — Add AudioStreamChunk proto message

**Files:** `proto/veyron_protocol.proto`

**What to do:** Add inside `Envelope` oneof (use field numbers 60–61, currently unused):

```protobuf
message AudioStreamChunk {
  uint32     stream_id      = 1;  // identifies stream session; plugins assign this
  AudioCodec codec          = 2;
  uint32     sample_rate    = 3;  // Hz: 16000, 44100, 48000
  uint32     channels       = 4;  // 1 = mono, 2 = stereo
  bytes      data           = 5;  // raw PCM_S16LE or Opus packet
  bool       end_of_stream  = 6;  // sender signals stream closed
}

enum AudioCodec {
  AUDIO_CODEC_UNSPECIFIED = 0;
  AUDIO_CODEC_PCM_S16LE   = 1;  // raw PCM, 16-bit signed LE — use for local UDS
  AUDIO_CODEC_OPUS        = 2;  // Opus — use for WebSocket (bandwidth)
}
```

Add to `Envelope` oneof:
```protobuf
AudioStreamChunk audio_stream_chunk = 60;
```

**Rules (enforced by convention, not kernel logic):**
- When `FLAG_RAW_BINARY` (Bit 4) is set: payload is raw PCM or Opus bytes, no Envelope wrapper.
  Kernel routes without Protobuf parse. Stream metadata was negotiated out-of-band via
  `AudioStreamChunk` on a prior frame.
- When `FLAG_RAW_BINARY` is clear and payload type is `AudioStreamChunk`: structured frame,
  used for the first frame of a stream (codec negotiation) and `end_of_stream` signal.

**Transport convention (documented in `docs/FRAMING.md`):**
- Local plugin-to-plugin over UDS: prefer `PCM_S16LE` + `FLAG_RAW_BINARY`. Zero transcoding.
- Plugin-to-external-client over WebSocket gateway: prefer `OPUS`. Gateway transparently
  forwards; transcoding is the sending plugin's responsibility.
- Kernel never chooses codec. Kernel is dumb.

**Acceptance test:** `cargo build` passes with new proto. `protoc` generates `AudioStreamChunk`
in Rust bindings.

---

### T-05 — Add PERMISSION_AUDIO_STREAM

**Files:** `proto/veyron_protocol.proto`, `src/auth/permissions.rs`

**What to do:**

In proto `PermissionType` enum, add:
```protobuf
PERMISSION_AUDIO_STREAM = 11;  // send/receive raw audio frames via FLAG_RAW_BINARY
```

Note: `PERMISSION_AUDIO = 5` (existing) covers `play_audio` / `record_audio` actions via
`ActionRequest`. `PERMISSION_AUDIO_STREAM` is separate — it gates peer-to-peer raw audio
frame routing.

In `src/auth/permissions.rs`, no logic change needed yet (permission name is sufficient for T-06).

Document in `docs/FRAMING.md`:

```
PERMISSION_AUDIO_STREAM — required for any plugin that sends frames with FLAG_RAW_BINARY
set, or sends AudioStreamChunk messages to another plugin.
```

**Acceptance test:** `cargo build --all-features` clean. `veyron_protocol.proto` contains
`PERMISSION_AUDIO_STREAM = 11`.

---

### T-06 — Enforce audio stream permission in router

**Files:** `src/ipc/protocol.rs` (ConnectionHandler), `src/auth/permissions.rs`

**What to do:** In `ConnectionHandler`, after frame is read and before routing:

```rust
// Audio stream permission gate
if frame.flags & FLAG_RAW_BINARY != 0 {
    if let Err(e) = check_permission(&registry, &sender_id, PermissionType::PermissionAudioStream) {
        send_error(&mut stream, ErrorCode::ErrPermissionDenied, e.to_string()).await?;
        return; // drop frame, do not route
    }
    // Also enforce ipc_targets allowlist (same as regular IPC send)
    if let Err(e) = check_ipc_target(&registry, &sender_id, &target_str) {
        send_error(&mut stream, ErrorCode::ErrPermissionDenied, e.to_string()).await?;
        return;
    }
}
```

Add `ERR_PERMISSION_DENIED = 8` to `ErrorCode` enum in proto (field 8 is free).

**Acceptance test:** Integration test — plugin without `PERMISSION_AUDIO_STREAM` sends a
frame with `FLAG_RAW_BINARY` set → receives `ErrorMessage { code: ERR_PERMISSION_DENIED }`.
Frame not delivered to target. Confirmed via mock target that asserts zero messages received.

---

## Phase 2.3 — Plugin Marketplace (GitHub-backed)

**Goal:** `vyn plugin list/search/install` fully working. Plugins fetched from
`github.com/veyron-core/veyron-plugins`. Archive integrity verified before extraction.

**Done-when:**
- `vyn plugin list` prints registry table
- `vyn plugin search <query>` filters by substring
- `vyn install <slug>` downloads, verifies SHA-256, extracts, validates `plugin.json`
- `vyn completions <shell>` outputs working tab-completion including plugin slugs
- All commands work offline against cached registry (TTL 1h)

---

### T-07 — Define registry.json schema

**Files:** `docs/PLUGIN_REGISTRY_SCHEMA.md` (new)

**What to do:** Write the normative schema document. Canonical registry location:

```
https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json
```

Each entry in the registry array:

```json
{
  "id": "001",
  "slug": "stt-whisper",
  "name": "Whisper STT",
  "description": "Speech-to-text via OpenAI Whisper. Supports 16kHz mono PCM input.",
  "version": "1.2.0",
  "permissions": ["audio_stream", "network"],
  "archive_url": "https://github.com/veyron-core/veyron-plugins/releases/download/stt-whisper-1.2.0/stt-whisper-1.2.0.zip",
  "source_url":  "https://github.com/veyron-core/veyron-plugins/releases/download/stt-whisper-1.2.0/stt-whisper-1.2.0-src.zip",
  "sha256": "<64-char hex>",
  "min_kernel_version": "0.3.0"
}
```

Field rules:
- `id`: zero-padded 3-digit numeric string, monotonically increasing, never reused
- `slug`: `[a-z0-9-]+`, globally unique, stable across versions (same slug = same plugin)
- `permissions`: string list matching `PERMISSION_*` names (lowercase, without prefix)
- `sha256`: SHA-256 of the `.zip` archive bytes before extraction
- `min_kernel_version`: semver; `vyn install` rejects if running kernel is older

**Acceptance test:** `docs/PLUGIN_REGISTRY_SCHEMA.md` merged to repo. A sample `registry.json`
in `testdata/registry_sample.json` validates against the schema (add a unit test using `serde_json`).

---

### T-08 — Registry fetch and cache

**Files:** `src/marketplace/registry.rs` (new), `src/marketplace/mod.rs` (new),
`Cargo.toml` (add `reqwest`, `sha2`, `zip` crates)

**What to do:**

```rust
pub struct PluginEntry {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub permissions: Vec<String>,
    pub archive_url: String,
    pub source_url: String,
    pub sha256: String,
    pub min_kernel_version: String,
}

pub async fn fetch_registry(refresh: bool) -> Result<Vec<PluginEntry>, VeyronError>;
```

Cache location: `$XDG_CACHE_HOME/veyron/registry.json` (fallback: `~/.cache/veyron/registry.json`).
Cache TTL: 1 hour (compare `mtime` of cache file).
On `refresh = true`: bypass TTL, re-fetch unconditionally.
On network failure with valid cache: use stale cache, print warning.
On network failure without cache: return error.

Kernel does NOT call `fetch_registry` on startup. Only CLI commands trigger it.

**Acceptance test:** Unit test with `mockito` (or similar) HTTP mock:
- First call fetches and writes cache
- Second call within TTL reads from disk (zero HTTP requests)
- `refresh = true` fetches even within TTL

---

### T-09 — `vyn plugin list` and `vyn plugin search`

**Files:** `src/cli/mod.rs`, `src/cli/plugin.rs` (new)

**What to do:** Add to `Commands` enum:

```rust
Plugin {
    #[command(subcommand)]
    cmd: PluginCmd,
},
```

```rust
pub enum PluginCmd {
    List {
        #[arg(long)]
        refresh: bool,
    },
    Search {
        query: String,
        #[arg(long)]
        refresh: bool,
    },
    Start { id: String },
    Stop  { id: String },
    Restart { id: String },
    Logs  { id: String, #[arg(long, default_value = "20")] lines: usize },
    Install {
        target: String,   // slug or numeric id
        #[arg(long)]
        refresh: bool,
    },
}
```

`list` output format (tab-aligned columns):
```
ID   SLUG            VERSION  PERMISSIONS              DESCRIPTION
001  stt-whisper     1.2.0    audio_stream, network    Speech-to-text via Whisper
002  tts-kokoro      0.9.1    audio_stream             Text-to-speech via Kokoro
```

`search <query>`: case-insensitive substring match against `slug`, `name`, `description`.
Same output format as `list`.

`start/stop/restart/logs`: HTTP calls to running kernel REST API. Wrap existing endpoints.
Fail with clear error if kernel not running.

**Acceptance test:** `vyn plugin list` with mocked registry prints header + ≥1 row.
`vyn plugin search stt` with mock returns only rows where slug/name/description contains "stt".

---

### T-10 — `vyn install <slug-or-id>`

**Files:** `src/cli/plugin.rs`, `src/marketplace/installer.rs` (new)

**What to do:** Steps executed in order; any failure aborts with non-zero exit code:

```
1. Resolve target: match registry entry by slug (exact) or id (exact).
   Error: "Plugin '<target>' not found. Run 'vyn plugin search <query>' to browse."

2. Version check: compare entry.min_kernel_version against running kernel version.
   Error: "Plugin requires kernel ≥ X.Y.Z, running Z.Y.X. Upgrade vyn first."

3. Download: HTTP GET entry.archive_url → temp file in $TMPDIR/veyron-install-<slug>/.
   Show progress bar (indicatif crate).

4. Integrity check: SHA-256 of downloaded bytes == entry.sha256.
   If mismatch: delete temp file, return error "Archive integrity check failed.
   Expected <sha256>, got <actual>. Do not proceed."

5. Extract: unzip archive to $VEYRON_PLUGIN_DIR/<slug>/ (default: ~/.local/lib/veyron/plugins/).
   Never extract outside target dir (zip-slip check: reject entries with `..` in path).

6. Validate plugin.json inside extracted dir:
   Required fields: plugin_id, version, permissions (array), binary (path).
   Error: "Invalid plugin.json: missing field '<field>'."

7. Print:
   "✓ Installed <slug> v<version> to ~/.local/lib/veyron/plugins/<slug>/
    Add to config.yaml to activate:
      plugins:
        - id: <plugin_id>
          binary: ~/.local/lib/veyron/plugins/<slug>/<binary>"
```

**Acceptance test:** Integration test with local HTTP server serving a valid signed zip:
all 7 steps pass, plugin directory exists, `plugin.json` readable.
Second test: SHA-256 mismatch → error returned, no directory created.
Third test: zip-slip entry (`../../evil`) → rejected at step 5.

---

### T-11 — Shell tab-completion for `vyn install <TAB>`

**Files:** `Cargo.toml` (add `clap_complete`), `src/cli/mod.rs`, `src/cli/complete.rs` (new)

**What to do:**

Add `Commands::Completions { shell: Shell }` where `Shell` is `clap_complete::Shell`.

```bash
vyn completions bash  > /etc/bash_completion.d/vyn
vyn completions zsh   > ~/.zfunc/_vyn
vyn completions fish  > ~/.config/fish/completions/vyn.fish
```

For dynamic completion of `vyn install <TAB>` and `vyn plugin search <TAB>`:
- Shell completion script calls `vyn __complete-slugs` (hidden subcommand)
- `__complete-slugs` reads cached registry (no network if cache fresh) and prints one slug per line
- Shell captures output as completion candidates

Document in `README.md` (Getting Started section):
```bash
# Enable tab-completion (zsh)
vyn completions zsh > ~/.zfunc/_vyn
echo 'fpath=(~/.zfunc $fpath)' >> ~/.zshrc
echo 'autoload -Uz compinit && compinit' >> ~/.zshrc
```

**Acceptance test:** `vyn completions zsh` exits 0 and outputs non-empty shell script.
`vyn __complete-slugs` with mocked cache prints slugs, one per line.

---

## Phase 2.4 — Production Hardening

**Goal:** All P2 AUDIT items resolved. CI covers fuzz. Rate limiting in place. Fragmentation unblocked.

**Done-when:**
- Fuzz runs in CI on every PR (60s budget)
- HTTP API has per-token rate limiting
- macOS sandbox emits warning instead of silently skipping
- Fragment reassembly logic exists (even if gated behind feature flag)
- Socket path uses XDG_RUNTIME_DIR

---

### T-12 — Fragmentation (Flag Bit 2)

**Files:** `src/ipc/framing.rs`, `src/ipc/protocol.rs`

**What to do:** Add `FLAG_FRAGMENTED = 0x0004` to `framing.rs`.

Fragment metadata packed in first 8 bytes of payload when `FLAG_FRAGMENTED` is set:
```
[fragment_id: u16 BE][sequence: u16 BE][total: u16 BE][stream_id: u32 BE]
```

Reassembly buffer in `ConnectionHandler`:
- `HashMap<u32, ReassemblyBuf>` keyed by `stream_id`
- `ReassemblyBuf` holds fragments + `Instant` of first fragment received
- On each frame arrival: insert fragment, check if `sequence == total - 1` and all prior received
- If complete: reassemble in order, dispatch as single frame to router
- Per-connection timeout: 30s from first fragment. On timeout: discard buffer, send
  `ERR_INTERNAL` to sender. Prevents fragment-based memory exhaustion.

**Acceptance test:** Unit test: 3 fragments sent out-of-order, reassembled correctly.
Timeout test: incomplete fragment set after 30s → buffer cleared, no memory leak.

---

### T-13 — CI fuzz integration (AUDIT-004)

**Files:** `.github/workflows/fuzz.yml` (new)

**What to do:**

```yaml
name: Fuzz
on: [pull_request]
jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz
      - run: cargo fuzz run fuzz_frame_parse -- -max_total_time=60
        working-directory: fuzz/
```

Run all three libFuzzer targets: `fuzz_frame_parse`, `fuzz_proto_envelope`, `fuzz_target_routing`
(add any missing targets to `fuzz/` directory).

**Acceptance test:** CI job appears in PR checks. No crash in 60s run on a clean corpus.

---

### T-14 — macOS sandbox warning (AUDIT-005)

**Files:** `src/plugins/runner.rs`

**What to do:** In `spawn_internal()`, where `cfg!(target_os = "linux")` gates namespace setup:

```rust
#[cfg(not(target_os = "linux"))]
if config.sandbox {
    warn!(
        plugin_id = %config.plugin_id,
        "sandbox=true has no effect on this OS (Linux required for namespace isolation)"
    );
}
```

**Acceptance test:** Unit test on non-Linux: mock config with `sandbox: true`, verify
`warn!` is emitted (use `tracing-test` crate to capture spans).

---

### T-15 — Per-token rate limiting on HTTP API

**Files:** `src/api/server.rs`, `Cargo.toml` (add `tower-governor` or `axum-governor`)

**What to do:** Apply rate limiting middleware to all authenticated API routes:

```rust
let governor = GovernorConfigBuilder::default()
    .per_second(config.api_rate_limit_rps.unwrap_or(100))
    .burst_size(config.api_rate_limit_burst.unwrap_or(20))
    .finish()
    .unwrap();
```

Key by JWT `sub` claim (per-token, not per-IP) so shared NAT does not cause false throttling.
On limit exceeded: return `429 Too Many Requests` with `Retry-After` header.

Add to `config.yaml`:
```yaml
api_rate_limit_rps: 100
api_rate_limit_burst: 20
```

**Acceptance test:** Integration test: send 150 requests in 1s with same token →
first 120 (100 + burst 20) succeed, remainder return 429.

---

### T-16 — Socket path hardening (AUDIT P3)

**Files:** `src/utils/config.rs`

**What to do:** Change default socket path resolution:

```rust
fn default_socket_path() -> String {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        format!("{runtime_dir}/veyron.sock")
    } else {
        "/tmp/veyron.sock".to_string()
    }
}
```

Update `config.yaml` example in README and CLAUDE.md to use `XDG_RUNTIME_DIR`-relative path.

**Acceptance test:** Unit test: with `XDG_RUNTIME_DIR=/run/user/1000` set →
default socket path is `/run/user/1000/veyron.sock`.

---

### T-17 — Structured JSON logging

**Files:** `src/utils/logging.rs`

**What to do:** Gate JSON output on `LOG_FORMAT` env var:

```rust
if std::env::var("LOG_FORMAT").as_deref() == Ok("json") {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();
} else {
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();
}
```

**Acceptance test:** `LOG_FORMAT=json vyn start --foreground 2>&1 | head -1 | jq .` exits 0.

---

## Definition of Done

| Criterion | Phase |
|-----------|-------|
| Flag bit space canonical, all SDKs aligned | 2.1 |
| WS JWT deviation documented | 2.1 |
| Configurable shutdown grace period | 2.1 |
| `FLAG_RAW_BINARY` exported and enforced | 2.2 |
| `AudioStreamChunk` in proto | 2.2 |
| Audio routing requires `PERMISSION_AUDIO_STREAM` | 2.2 |
| `ERR_PERMISSION_DENIED` returned on audio auth failure | 2.2 |
| `registry.json` schema defined and tested | 2.3 |
| `vyn plugin list` prints registry table | 2.3 |
| `vyn plugin search` filters correctly | 2.3 |
| `vyn install` verifies SHA-256 before extraction | 2.3 |
| Zip-slip check on extraction | 2.3 |
| Tab-completion for `vyn install <TAB>` | 2.3 |
| Fragment reassembly with DoS timeout | 2.4 |
| Fuzz runs in CI on every PR | 2.4 |
| macOS sandbox emits warning | 2.4 |
| HTTP API rate-limited per token | 2.4 |
| Socket path uses XDG_RUNTIME_DIR | 2.4 |
| JSON log output available via LOG_FORMAT=json | 2.4 |

---

## Task Summary

| ID  | Title | Phase | Priority | Status |
|-----|-------|-------|----------|--------|
| T-01 | Canonicalize flag bit space | 2.1 | P0 | ✅ 2026-06-30 |
| T-02 | Document WS JWT deviation | 2.1 | P0 | ✅ 2026-06-30 |
| T-03 | Configurable shutdown grace period | 2.1 | P1 | ✅ 2026-06-30 |
| T-04 | AudioStreamChunk proto message | 2.2 | P1 |
| T-05 | Transport negotiation convention | 2.2 | P1 |
| T-06 | Enforce audio stream permission in router | 2.2 | P1 |
| T-07 | Define registry.json schema | 2.3 | P1 |
| T-08 | Registry fetch and cache | 2.3 | P1 |
| T-09 | `vyn plugin list` and `vyn plugin search` | 2.3 | P1 |
| T-10 | `vyn install <slug-or-id>` | 2.3 | P1 |
| T-11 | Shell tab-completion | 2.3 | P2 |
| T-12 | Fragmentation (Flag Bit 2) | 2.4 | P2 |
| T-13 | CI fuzz integration | 2.4 | P2 |
| T-14 | macOS sandbox warning | 2.4 | P2 |
| T-15 | Per-token rate limiting on HTTP API | 2.4 | P2 |
| T-16 | Socket path hardening | 2.4 | P3 |
| T-17 | Structured JSON logging | 2.4 | P3 |

---

*Supersedes ROADMAP_v2.md. Next revision scheduled after Phase 2.3 ships.*
