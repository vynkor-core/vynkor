# V-07 implementation notes — kernel marketplace cutout

Task: **V-07** from `docs/VYNM_ROADMAP.md` (the join: V-02 loader + V-03…V-06
manager all landed first). Branch `feat/v07-marketplace-cutout`.
Implemented 2026-08-22.

## What changed

| File | Change |
|---|---|
| `src/marketplace/` | **DELETED** (5 files, ~2.3k lines incl. tests) — the implementation lives in vynkor-manager since V-03…V-05. |
| `src/lib.rs` | `pub mod marketplace;` dropped (C6). |
| `src/cli/plugin.rs` | start/stop/restart/logs stay pure REST (unchanged behavior); list/search/install/remove/enable/disable became D4 delegation shims exec'ing `vynm` with forwarded args (`--config` forwarded so both binaries resolve the same plugins.d). Clap grammar untouched — accepted-but-ignored flags keep scripts working. `handle()` signature slims 14 params → 6. |
| `src/cli/complete.rs` | `__complete-slugs` → D4/D6 shim: tries vynm's own hidden command, degrades to an actionable note (completion hooks must not hard-fail prompts). |
| `src/cli/devices.rs` | C3: `format_ts` inlined (sole remaining user after marketplace deletion); algorithm provenance documented at the site. |
| `src/plugins/loader.rs` | stale coexistence-window comment removed (V-07 happened). |
| `src/main.rs` | plugin dispatch updated to the slim signature; `resolve_plugins_dir` import gone. |
| `Cargo.toml` | direct deps dropped: `zip`, `indicatif`, `ed25519-dalek`, `sha2`, plus bonus cleanup `hmac`/`hkdf` (verified unused — frame MAC arrives via `pub use veyron_wire::mac::*`). |
| `tests/unit/` | `test_installer.rs`, `test_state.rs` deleted (+ mod entries) — they live in vynkor-manager now. |

## One documented deviation from the roadmap text

The roadmap's dep-drop list includes `reqwest`. But the same task says
"start/stop/restart/logs STAY unchanged — they are pure REST calls", and
those REST calls ARE reqwest (`build_client`/`api_get`/`api_post`, with their
own test coverage). Dropping reqwest would mean rewriting the surviving
kernel-API client onto another HTTP stack mid-task — out of scope and against
"STAY unchanged". So **reqwest stays**, now serving only the API client.
Everything else on the list is gone; verified via `cargo tree -i` (zip/
indicatif/sha2/ed25519-dalek/hmac/hkdf show zero direct kernel deps;
sha2/hmac remain only as transitive wire deps for framing MAC).

## Acceptance checks

- ✅ no `marketplace` module exists in the crate; symbol spot-check of the
  built binary finds none
- ✅ `cargo tree`: zip/indicatif/sha2/ed25519-dalek not direct deps anymore
- ✅ full suite green without any manager checkout: **430 passed** (93 unit +
  91 integration-slow + 246 lib), clippy `-D warnings`, fmt clean
- ⏳ `vynm install database` end-to-end against this kernel = manual smoke,
  doable right after merge (vynm CLI exists as of V-06)

## Notes

- The shims forward `--config <path>` so vynm resolves drop-ins identically;
  registry keys in config.yaml (`registry_url`, `marketplace_public_key`,
  `registry_cache_ttl_secs`) are read by vynm itself now — operator config
  format unchanged.
- Shim removal itself is deferred to stage 3 per plan (D4).
