# CD-06 — Version Negotiation in Handshake

*Track B — `vynkor` + `vynkor-wire` · P2 · Source: `CLIENT_DRIVEN_KERNEL_TASKS.md` §6*

## Goal

Client sends `protocol_version=1.7`. Behavior on mismatch is currently "defined ad hoc" — agree on `min/max` and tell the client what to do.

---

## Already Exists

- `../vynkor-wire/src/lib.rs:PROTOCOL_VERSION = "1.7"` (E-01).
- `src/ipc/protocol.rs:402` — on `PluginRegister`:
  ```rust
  wire_major = PROTOCOL_VERSION.split('.').next()
  plugin_major = reg.protocol_version.split('.').next().unwrap_or(wire_major)
  if !reg.protocol_version.is_empty() && plugin_major != wire_major { ERR_PROTOCOL_MISMATCH }
  ```
  - `major` mismatch → `PluginRegisterAck{accepted:false}`, minor/patch → accepted.
  - Empty `protocol_version` → accept (v1.5 host plugins).
- `src/plugins/registry.rs:DeviceInfo` — already stores `protocol_version` indirectly via `manifest` (but not as field).

## Required

- [ ] **CD-06 — min/max negotiation (vynkor+wire, 3–5h):**
  - In `vynkor-wire` add `MIN_SUPPORTED_PROTOCOL_VERSION = "1.6"` (or `"1.5"` — decide) + export.
  - In `PluginRegisterAck` add `negotiated_version: string` + `min_supported_version: string` (additive, check free tags in `Envelope`; if none, new field in `PluginRegisterAck` tag 5/6).
  - In `protocol.rs` — branch: if `plugin_major < min_major || plugin_major > wire_major` → `ERR_PROTOCOL_MISMATCH` with `reject_reason = "kernel supports 1.6–1.7, got 1.5"`; else `accepted=true` + `negotiated_version = min(wire, plugin)`.
  - Logic does not break D-03: old `""` remains accepted.

  - **Files:** `../vynkor-wire/proto/vynkor_protocol.proto` (`PluginRegisterAck.negotiated_version`, `min_supported_version`), `../vynkor-wire/src/lib.rs` (`MIN_SUPPORTED_PROTOCOL_VERSION`), `src/ipc/protocol.rs` (reject/accept branch), `src/plugins/registry.rs` (optional store `negotiated_version` on `PluginEntry`).
  - **Acceptance:** client 1.7 → `accepted true, negotiated 1.7`; client 2.0 → `REJECT "kernel supports 1.6–1.7"`; client 1.5 (`""`) → accepted; test `test_version_negotiation_min_max` green; six vendored copies byte-identical (R8-05 drift test).
  - **Do not:** break wire major without RFC, do not require `min` from client (kernel decides).

## Anticipate (verified in code)

- **Single-commit bump:** `proto header + Cargo.toml + PROTOCOL_VERSION + 6 vendored copies` — otherwise R8-05 drift test fails. Verified: wire is 1.7, `REMOTE_DEVICES_ROADMAP` still on 1.5/1.6.
- **`""` stays accepted:** old `v1.5` host plugins without `protocol_version` — do not break. Verified: `if !reg.protocol_version.is_empty()` already does this.
- **No bump — docs only:** if you want to avoid touching wire, document in `docs/FRAMING.md` that `major=reject, minor/patch=accepted`, but plan asks for `min/max`.

## Implementation Plan

1. `vynkor-wire/proto` — bump `PROTOCOL_VERSION` header + add fields in `PluginRegisterAck` (one commit with `Cargo.toml` — like D-01).
2. `vynkor-wire/src/lib.rs` — `pub const MIN_SUPPORTED: &str = "1.6"`.
3. `src/ipc/protocol.rs` — replace `if plugin_major != wire_major` with `if plugin_major < min_major || plugin_major > wire_major`.
4. Test: `tests/unit/test_router.rs` — 4 cases (1.6, 1.7, 2.0, ""), check `ack.negotiated_version`.

## Complexity / Value / Time

| | Estimate |
|---|---|
| **Complexity** | Low (S) — 15 lines logic + proto bump in one commit |
| **Value** | Low-medium (P2 hygiene) — predictable upgrade path for APK |
| **Time** | **3–5h** (1h proto + 2h logic/test) |
| **Risk** | Low — additive, does not break existing clients |
| **Dependencies** | `wire` bump → requires sync in `sdk-{cpp,python,rs}` (can defer — kernel alone) |

---

## Alternative Without Wire Bump

If you do not want to touch `wire` — leave as is and document in `docs/FRAMING.md` that `major mismatch = reject`, `minor/patch = accepted`. But plan asks for min/max — bump is justified.
