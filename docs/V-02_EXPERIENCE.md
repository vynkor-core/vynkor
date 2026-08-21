# V-02 implementation notes — kernel loader on wire manifest

Task: **V-02** from `docs/VYNM_ROADMAP.md` (kernel lane, stage 2). Branch
`feat/v02-loader-wire-manifest`, base `develop`. Implemented 2026-08-21.

## What changed

| File | Change |
|---|---|
| `src/plugins/loader.rs` | Import swap: `crate::marketplace::installer::{validate_manifest, InstallManifest}` → `veyron_wire::manifest::*`. `validate_plugin_def` passes `crate::auth::permissions::resolve_permission` as the injected resolver (D1 seam — same fn the marketplace copy called inline). Config-permission cross-check via `normalize_permission` stays kernel-side untouched. |
| `Cargo.toml` | `veyron-wire = "0.2.3"` → `{ version = "0.2.4", features = ["manifest"] }`. |
| `docs/V-02_EXPERIENCE.md` | this file |

Marketplace module is untouched — its own manifest copy keeps compiling and
its tests keep passing (acceptance criterion). The two copies coexist until
V-07 deletes the marketplace; a comment at the new import warns against
adding new users of the marketplace one in the meantime.

## Why it was this small

The roadmap planned a `[patch.crates-io]` git override "while 0.2.4 is
unpublished" — but V-01 shipped the publish the same day, so the kernel just
bumps the requirement. No override, no git dep.

## Type-swap safety check (the one real risk)

Wire's `InstallManifest` and the marketplace's are distinct types. Verified
no type leakage before building:

- `load_all` (loader) — reads `.actions` / `spec.permission()` / `spec.name()`
  / `.requires` / `.events`, all field-identical on the wire type ✓
- `src/api/routes.rs:152` — matches on the `Result` only, never names the
  manifest type ✓
- `tests/unit/test_manifest_enforcement.rs` + `test_installer.rs` — assert on
  error message text; wire's messages are byte-identical and arrive as
  `VeyronError::Internal` via the existing `From<WireError>` impl, so
  `contains()` assertions hold unchanged ✓

## Gates (all green)

| Gate | Result |
|---|---|
| `cargo build` | ✓ (pulls published veyron-wire 0.2.4 from crates.io) |
| `cargo test --all --all-features` | ✓ 521 passed / 0 failed — incl. `test_installer::manifest_*` against the untouched marketplace copy |
| `cargo clippy --all-targets --all-features -- -D warnings` | ✓ clean |
| `cargo fmt --check` | ✓ |

## Follow-ups

1. **V-07 (join)**: delete `src/marketplace/` entirely; the loader comment
   marking the coexistence window goes away with it.
2. Boot-time behavior verified identical by the existing suites (compat
   range, unknown permission, v2 per-action resolution) — no new tests needed;
   the drift surface moved to veyron-wire's own suite in V-01.
