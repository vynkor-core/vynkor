# Veyron ROADMAP — Phase 8

**Baseline:** 2026-08-10 · Kernel `0.1.0`
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md` · Phase 6: `ROADMAP_v6.md` · Phase 7 (C++/Python SDK parity): `ROADMAP_v7.md`, all items complete)

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero application databases.
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Current baseline — 2026-08-10

Phase 7 shipped full C++/Python SDK parity with the Rust reference client
(`publish_event`, streaming actions, session close, cross-SDK integration
tests). On the protocol side, kernel support for `PERMISSION_STORAGE` and
`ActionRequest.caller_plugin_id` landed on `develop` (`13274d4`,
`e00ec96`, `6b3691f`) — but the marketplace installer's permission allowlist
(`src/marketplace/installer.rs`) was never updated to match, so installing the
`database` plugin fails with `Plugin 'database' declares unknown permission
'PERMISSION_STORAGE'`. Phase 8 fixes that drift at the root: the known
permission set stops being a hand-maintained list and is derived from the
`PermissionType` proto enum itself, so it can never fall behind the protocol
again.

## Phase 8 — Permission/protocol sync

The proto's `PermissionType` enum is the single source of truth for what a
plugin may declare. Phase 7's protocol work proved the enum can grow
(`PERMISSION_EVENT_PUBLISH`, `PERMISSION_STORAGE`) while the installer's
hand-typed list quietly rots. This phase derives the validation set from the
generated enum, adds drift-detection tests, and lands the follow-ups the
`database` plugin needs across the sibling repos.

- [x] R8-01 — **Installer permission validation derived from the proto enum:**
  replace `const KNOWN_PERMISSIONS` in `src/marketplace/installer.rs` with a
  set derived from `PermissionType::values()` (prost `Enumeration`), accepting
  both the documented lowercase form (`storage`) and the `PERMISSION_`-prefixed
  proto name (`PERMISSION_STORAGE`); unknown permissions keep the exact
  existing error.
  - Files: `src/marketplace/installer.rs`, `tests/unit/test_installer.rs`.
  - Acceptance: `vyn plugin install database` no longer fails with the
    unknown-permission error; `"teleport"` is still rejected.

- [x] R8-02 — **Permission drift-detection tests:** new test walking
  `PermissionType::values()`: every variant except `PERMISSION_UNKNOWN` must
  pass `validate_manifest` in both forms — a future proto permission can never
  silently fail installation again.
  - Files: `tests/unit/test_installer.rs`.
  - Acceptance: `cargo test --test unit manifest_accepts_every_proto_permission` green.

- [x] R8-03 — **Runtime `check_permission` form normalization:**
  `src/auth/permissions.rs::check_permission` compares declared permissions
  against `as_str_name()` with exact equality; normalize the comparison
  (case-insensitive, `PERMISSION_`-prefix-stripped on both sides) so a plugin
  declaring the lowercase form is not denied at runtime.
  - Files: `src/auth/permissions.rs`, unit tests.
  - Acceptance: a manifest declaring `"network"` satisfies a
    `PermissionNetwork` requirement; missing permissions still denied.

- [x] R8-04 — **Registry schema doc alignment:** `docs/PLUGIN_REGISTRY_SCHEMA.md`
  adds `storage`/`event_publish` rows to the permission mapping table and
  notes that the allowed set is derived from the `PermissionType` proto enum
  (both forms accepted).
  - Acceptance: mapping rows present; no schema restructuring.

- [x] R8-05 — **Proto-copy byte-identity drift test:** test asserting the three
  vendored `veyron_protocol.proto` copies (`wire/proto`, `sdk/python/proto`,
  `sdk/cpp/proto`) stay byte-identical.
  - Files: `tests/unit/test_proto_sync.rs` (new), `tests/unit/mod.rs`.
  - Acceptance: test passes; editing one copy makes it fail.

- [x] R8-06 — **(cross-repo) Land the `database` plugin in veyron-plugins:**
  merge `worktree-database-plugin` into `veyron-plugins` `main`
  (`plugins/database/`), add the registry entry via `scripts/package.sh`
  (permissions normalized to `["storage"]`), build `dist/database-0.1.0.zip`,
  move `database` from Planned to Shipped in `veyron-plugins/ROADMAP.md`.
  - Tracked in: `veyron-plugins/ROADMAP.md`.
  - Acceptance: `vyn plugin install database` against the fixed kernel succeeds
    and the plugin registers.

- [x] R8-07 — **(cross-repo) Publish `veyron-wire` 0.2.0, drop patch override:**
  `cargo publish` `veyron-wire` 0.2.0 (kernel already depends on it by
  path + `[patch.crates-io]` override whose comment says it is a no-op once
  0.2.0 is published); remove the override and verify the workspace still
  builds green.
- Tracked in: `veyron-wire/`.
- Acceptance: `cargo search veyron-wire` shows 0.2.0; no `patch.crates-io`
  block in `Cargo.toml`; full test suite green without it.

---

## Cross-repo coordination

- **veyron-plugins** (`veyron-plugins/ROADMAP.md`): database plugin landing
  (R8-06) depends on R8-01/R8-02 landing here first — the kernel must accept
  `PERMISSION_STORAGE` before the registry entry is installable.
- **veyron-wire** (`veyron-wire/`): 0.2.0 publish (R8-07) is release-process
  work; kernel changes here only consume it.
- SDK proto copies in this repo (`sdk/python/proto`, `sdk/cpp/proto`) are
  guarded by the R8-05 drift test; the veyron-wire repo's own copy is synced
  at release time.

## Task Summary

| Item | Scope | Depends on |
|------|-------|------------|
| R8-01 | installer permissions derived from `PermissionType` enum | none |
| R8-02 | permission drift-detection tests | R8-01 |
| R8-03 | runtime `check_permission` normalization | none |
| R8-04 | registry schema doc alignment | none |
| R8-05 | proto-copy byte-identity test | none |
| R8-06 | `database` plugin landing (veyron-plugins) | R8-01, R8-02 |
| R8-07 | `veyron-wire` 0.2.0 publish + patch removal | none |

**Ship gate:** R8-01..R8-05 are kernel-local and land together on `develop`;
R8-06/R8-07 are cross-repo coordination items shipped from their own repos.

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- C++: existing CMake test targets stay green; new tests follow the
  `sdk/cpp/tests/test_*.cpp` naming/registration pattern in `CMakeLists.txt`.
- Python: new tests follow the `tests/python/test_*.py` pattern.
- Docs updated in the same PR (README for operator-visible changes; no
  `docs/FRAMING.md` changes expected since the wire format doesn't change).
