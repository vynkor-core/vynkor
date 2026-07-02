# Plugin Registry Schema

Normative reference for `registry.json` (central marketplace index) and `plugin.json` (per-plugin
manifest). All tooling (`vyn install`, `vyn plugin list/search`) and the kernel load-time gate are
derived from this document.

---

## registry.json — Central Plugin Registry

**Canonical URL:**

```
https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json
```

Top-level structure: a JSON array of plugin entries.

```json
[
  { ... },
  { ... }
]
```

### Entry Schema

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
  "sha256": "<64-char lowercase hex>",
  "min_kernel_version": "0.3.0",
  "max_kernel_version": "1.0.0"
}
```

### Field Rules

| Field | Type | Rules |
|-------|------|-------|
| `id` | string | Zero-padded 3-digit numeric string (e.g. `"001"`). Monotonically increasing. Never reused after deletion. |
| `slug` | string | Pattern `[a-z0-9-]+`. Globally unique. Stable across versions — same slug always refers to the same plugin. |
| `name` | string | Human-readable display name. No length limit. |
| `description` | string | One-sentence summary. Used by `vyn plugin search`. |
| `version` | string | Semver of the released archive (`MAJOR.MINOR.PATCH`). |
| `permissions` | string[] | Lowercase permission names without `PERMISSION_` prefix, matching names in `PermissionType` enum. |
| `archive_url` | string | HTTPS URL to the `.zip` binary archive. |
| `source_url` | string | HTTPS URL to the `.zip` source archive. Required for audit; may be same as `archive_url` for source-only plugins. |
| `sha256` | string | SHA-256 of the archive bytes at `archive_url`, as 64-char lowercase hex. Verified before extraction. |
| `min_kernel_version` | string | Semver lower bound (inclusive). `vyn install` rejects if running kernel is older. |
| `max_kernel_version` | string | Semver upper bound (inclusive). `vyn install` rejects if running kernel is newer. Use `"*"` for no upper bound. |

### Invariants

- Every `id` appears exactly once in the array.
- Every `slug` appears exactly once in the array.
- `min_kernel_version` must be a valid semver string or absent (treated as `"0.0.0"`).
- `max_kernel_version` must be a valid semver string or `"*"`.
- `min_kernel_version <= max_kernel_version` when both are semver (not `"*"`).

---

## plugin.json — Plugin Manifest

Every **marketplace-installed** plugin directory must contain a `plugin.json` at its root
(`vyn install` validates it in Step 7). For config-declared local plugins the file is
**optional**: when present the kernel validates it before spawning (compatibility, permissions,
dependencies); when absent the plugin is spawned with no manifest-derived checks.

### Schema

```json
{
  "plugin_id": "stt-whisper",
  "version": "1.2.0",
  "permissions": ["audio_stream", "network"],
  "kernel_compatibility_range": {
    "min": "0.3.0",
    "max": "1.0.0"
  },
  "binary": "stt-whisper",
  "events": ["system.ready"],
  "actions": ["transcribe_audio"],
  "requires": ["audio-router"]
}
```

### Field Rules

| Field | Type | Required | Rules |
|-------|------|----------|-------|
| `plugin_id` | string | Yes | Must match the registry `slug` for marketplace plugins. Free-form for local-only plugins. |
| `version` | string | Yes | Semver of the installed plugin. |
| `permissions` | string[] | Yes | Exhaustive list. Kernel denies any permission the plugin tries to use that is not listed here. |
| `kernel_compatibility_range` | object | Yes | Must contain `min` and `max` fields (both strings). |
| `kernel_compatibility_range.min` | string | Yes | Semver lower bound (inclusive). |
| `kernel_compatibility_range.max` | string | Yes | Semver upper bound (inclusive) or `"*"`. |
| `binary` | string | Yes | Relative path to the executable within the plugin directory. |
| `events` | string[] | No | Event types the plugin subscribes to (auto-subscribed at load). Empty array if none. |
| `actions` | string[] | No | Action identifiers the plugin exposes. Empty array if none. |
| `requires` | string[] | No | Plugin IDs that must be declared in config and are loaded first. Missing deps or dependency cycles refuse the plugin. |

---

## Kernel Load-Time Enforcement

Before spawning any plugin process, the kernel MUST perform these checks in order. On any
failure, the plugin is **skipped** (not loaded) and an error is logged. The kernel continues
loading remaining plugins — one bad plugin does not crash the kernel.

### Step 1 — Read manifest

Read `plugin.json` from the plugin binary's directory. A **missing** file skips all
manifest-derived checks (Steps 2–4) — the plugin is spawned unvalidated (local/dev plugins).
A **present but unparseable** file refuses the plugin:

```
Refusing to load plugin '<dir>': Invalid plugin.json: <parse error>
```

### Step 2 — Kernel version compatibility

Parse `kernel_compatibility_range.min` and `kernel_compatibility_range.max`. Compare against the
running kernel version (from `env!("CARGO_PKG_VERSION")`).

If `running_kernel < min`:

```
Refusing to load plugin '<plugin_id>': requires kernel >= <min>, <= <max>, running <current>
```

If `running_kernel > max` (and `max != "*"`):

```
Refusing to load plugin '<plugin_id>': requires kernel >= <min>, <= <max>, running <current>
```

### Step 3 — Permission validation

Each entry in `permissions` must map to a known `PermissionType` in the proto enum.

Unknown permission:

```
Refusing to load plugin '<plugin_id>': unknown permission '<perm>'
```

### Step 4 — Config-granted permission cross-check

Each declared permission must be granted for this plugin in `config.yaml`. Permission declared
but not granted:

```
Plugin '<plugin_id>' requests permission '<perm>' which is not granted in config
```

---

## vyn install Compatibility Check

`vyn install <slug>` checks registry `min_kernel_version` / `max_kernel_version` **before**
downloading the archive (Step 2 of the install pipeline). Error message format:

```
Plugin '<slug>' requires Veyron kernel >= <min>, <= <max>. You are running <current>. Upgrade Veyron first.
```

After extraction, `vyn install` re-validates using the local `plugin.json`
`kernel_compatibility_range` as a defense-in-depth check (Step 7 of the install pipeline).

---

## Permissions Reference

String names used in `registry.json` `permissions` and `plugin.json` `permissions` map to
`PermissionType` proto enum values (lowercase, `PERMISSION_` prefix stripped):

| String name | Proto value | Meaning |
|-------------|-------------|---------|
| `network` | `PERMISSION_NETWORK` | Outbound HTTP requests |
| `files_read` | `PERMISSION_FILES_READ` | Read local files |
| `files_write` | `PERMISSION_FILES_WRITE` | Write/delete local files |
| `system` | `PERMISSION_SYSTEM` | System metrics (CPU, RAM, disk) |
| `audio` | `PERMISSION_AUDIO` | play_audio / record_audio via ActionRequest |
| `notify` | `PERMISSION_NOTIFY` | Send notifications |
| `scheduler` | `PERMISSION_SCHEDULER` | Timers and alarms |
| `browser` | `PERMISSION_BROWSER` | Browser control |
| `ipc_send` | `PERMISSION_IPC_SEND` | Unicast/broadcast to other plugins (also needs `ipc_targets`) |
| `audio_stream` | `PERMISSION_AUDIO_STREAM` | Peer-to-peer raw audio via FLAG_RAW_BINARY |
| `kernel_admin` | `PERMISSION_KERNEL_ADMIN` | Admin `KernelCommand`s (e.g. `reload_config`); `health_check` is exempt |

This list is normative and mirrors `KNOWN_PERMISSIONS` in `src/marketplace/installer.rs` and the
`PermissionType` enum in `proto/veyron_protocol.proto`.
