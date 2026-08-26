# Veyron ROADMAP — vynm marketplace extraction (F1 / DC-1)

**Baseline:** 2026-08-21 · Kernel `0.1.0` · `veyron-wire` `0.2.3` (proto v1.6)
**Branch:** `develop`
**Source:** architecture + decisions in `docs/VYNM_PLAN.md` (authoritative);
this file is the task breakdown. Mirrors `ROADMAP.md` F1 and
`docs/DUMB_CORE_AUDIT.md` §6-F1 / §7-decision-1.
**Status:** stages 1–2 DONE (V-01…V-08). **Stage-4 FINALE EXECUTED &
VERIFIED 2026-08-22**: all repos/dirs → `vynkor*` (org `veyron-core` stays
— `vynkor` handle is TAKEN on GitHub; owner to pick alternative), protocol
identifiers cutover everywhere (`proto::vynkor`, subprotocol `vynkor`,
`vynkor-frame-mac-v1`; PROTOCOL_VERSION stays 1.6), published:
`vynkor-wire 0.0.2`, `vynkor-sdk 0.0.3` (PyPI pending credentials).
Gates re-run independently: kernel 430 / manager 92 / wire / sdk / cpp 62 /
python 40 — all green; live-registry smoke OK. Stage-3 manager backlog is
now FULLY shipped incl. former parked items (`rollback`, `bundle`,
`package`, non-linux drop-ins — 2026-08-26). **Phase B (V-18) is
functionally SHIPPED** — registry live on Cloudflare R2, every published
version S1-signed and verified by the pinned key; only the custom-domain
binding is pending. **Phase C (V-19) is PARTIAL** — installer exists and
targets `~/.config/vyn`; custom hosting + AUR outstanding. Also open:
org-name decision.

> ID prefix `V-` = vynm. One task ≈ one reviewable PR; every merged commit is
> green in its repo. Where this file and VYNM_PLAN differ, VYNM_PLAN wins.

---

## Non-negotiables (carried from the plan)

- **Every merged commit green** — kernel `cargo test --all --all-features` +
  `clippy -D warnings` + `fmt --check`; manager and wire same gates.
- **Security boundaries port verbatim, never rewritten:** sha256 archive
  digest, zip-slip guard, Ed25519 signature verify, revocation gate,
  `create_new` (O_EXCL) drop-in write, slug/path traversal guards.
- **Dependency direction acyclic:** `veyron-wire ← {kernel, vynm}`. The
  manager never depends on the kernel crate; the kernel never depends on the
  manager. Manager CI asserts `cargo tree -i veyron` is empty.
- **Manifest module behind a cargo feature, default off** — SDK consumers
  compile neither serde_json nor semver of it.
- **The kernel never calls vynm and has no runtime awareness of it** — drop-ins
  are just files.

## Priority legend

- **P0** — foundation; blocks everything (wire manifest module).
- **P1** — extraction core (loader swap, manager port, kernel cutout).
- **P2** — stage-2 closeout (docs & gates).
- **P3** — stage-3 productization backlog (independent releases per D7).

Complexity: **S** ≤ half day · **M** ~1–2 days · **L** multi-session.

## Task index

| ID | Task | Pri | Size | Repo | Depends on |
|---|---|---|---|---|---|
| V-01 | wire: manifest module behind feature | P0 | L | veyron-wire | — |
| V-02 | kernel: loader import switch | P0 | S | veyron | V-01 |
| V-03 | manager scaffold + state + drop-in writer | P1 | M | vynkor-manager | V-01 |
| V-04 | manager: registry client + RegistrySource seam | P1 | L | vynkor-manager | V-03 |
| V-05 | manager: install pipeline port (+ D2/D3 deletions) | P1 | L | vynkor-manager | V-04 |
| V-06 | manager: vynm CLI | P1 | M | vynkor-manager | V-05 |
| V-07 | kernel: cut out src/marketplace | P1 | M | veyron | V-02, V-03…V-06 |
| V-08 | docs & gates, close stage 2 | P2 | S | veyron | V-07 |
| V-09 | multi-source config + resolution engine | P3 | M | vynkor-manager | V-06 |
| V-10 | permission preview on install | P3 | S | vynkor-manager | V-09 |
| V-11 | version pinning `slug@ver` | P3 | S | vynkor-manager | V-09 |
| V-12 | `vynm outdated` + `vynm update` | P3 | M | vynkor-manager | V-09 |
| V-13 | `vynm verify` | P3 | S | vynkor-manager | V-09 |
| V-14 | key tooling: `keygen` + `sign` | P3 | M | vynkor-manager | V-09 |
| V-15 | install from local archive / direct URL | P3 | M | vynkor-manager | V-05 |
| V-16 | CLI polish package | P3 | M | vynkor-manager | V-09…V-15 |
| V-17 | **stage 4/A:** rename code+packages (`vynkor-wire/sdk 0.0.1`, kernel crate, env/paths hard cut, GitHub last) | P0 | L | all repos | wire publish; domain not required |
| V-18 | **stage 4/B:** registry on Cloudflare R2 + S1 re-sign, plugins 0.0.1 | P0 | M | veyron-plugins + manager + kernel | ✅ shipped 2026-08-26, domain binding pending |
| V-19 | **stage 4/C:** installer `install.sh` on `~/.config/vyn/` + docs sweep + AUR PKGBUILD | P1 | M | veyron + distro | ◐ partial — installer done; hosting/AUR pending |

Parallel lanes after V-01: kernel lane (V-02) and manager lane
(V-03 → V-04 → V-05 → V-06) proceed independently; V-07 joins them.

```
            ┌─→ V-02 (kernel loader swap) ───────────────┐
V-01 wire ──┤                                            ▼
            └─→ V-03 → V-04 → V-05 → V-06 (manager) ─→ V-07 → V-08
                 state   registry installer CLI      cutout  docs

Stage 3 (manager only, independent releases): V-09 → {V-10, V-11, V-12,
V-13, V-14} → V-16; V-15 hangs off V-05 directly (needs only the pipeline).
```

---

## P0 — Stage 1: wire repo (`../vynkor-wire`)

- [x] **V-01 — wire: manifest module behind the `manifest` feature.**
  (shipped 2026-08-21: veyron-wire PR #6 merged; published to crates.io as
  **0.2.4** — V-02 needs no `[patch.crates-io]` override, just bump the
  requirement + `features = ["manifest"]`)
  Single implementation of plugin-manifest parsing/validation, consumed by
  both kernel and manager from crates.io (resolves coupling point C1).
  - New `src/manifest.rs`, entire module behind
    `#[cfg(feature = "manifest")]`; re-export from `src/lib.rs`.
  - Port verbatim from the kernel:
    `InstallManifest` (`veyron/src/marketplace/installer.rs:59`),
    `KernelCompatRange`, `ActionSpec` (V1 string / V2 object enum),
    `known_permissions()` (`installer.rs:27`), `check_kernel_compatibility()`
    (`registry.rs:626`), `validate_manifest()` (`installer.rs:754`).
  - Keep the `known_permissions()` probe logic exactly: prost 0.13 has no
    `values()`; walk enum codes, stop after 4 consecutive misses (reserved
    gap 7). Wire already generates `PermissionType` via prost — no new proto
    dependency.
  - Decouple `check_kernel_compatibility` from `RegistryEntry` (registry-domain
    type stays with the manager): take `(slug, min_kernel_version,
    max_kernel_version)` or `&InstallManifest` directly. Today's caller builds
    a throwaway fake `RegistryEntry` — that hack does not move.
  - D1 signature change: `validate_manifest(path, kernel_ver, resolver)` where
    `resolver: fn(&str) -> Option<PermissionType>` is injected. Expose a
    `default_resolver` in the module implementing today's normalization
    (strip `PERMISSION_` prefix, uppercase, `from_str_name`, exclude UNKNOWN)
    so the manager needs no kernel auth code. F5 can later change kernel
    policy without a wire change.
  - Errors: descriptive messages mapped into `WireError` (new variant or
    existing message variant); kernel wraps into `VeyronError` at the call
    site. Same fail-loud semantics as today.
  - `Cargo.toml`: `[features] manifest = ["dep:serde_json", "dep:semver"]`
    with optional deps, off by default.
  - Move the R8-02 drift test here: every proto `PermissionType` variant must
    pass `validate_manifest` in both name forms (`PERMISSION_X` + lowercase).
  - CI guard: default-feature build unaffected (SDK weight); MSRV 1.85 holds.
  - Files: `../vynkor-wire/src/manifest.rs` (new), `src/lib.rs`,
    `Cargo.toml`, tests (drift + manifest unit tests move from
    `tests/unit/test_installer.rs` manifest cases).
  - Acceptance: `cargo build` (no features) green; `cargo test --features
    manifest` green incl. drift test; version bump per README rules
    (additive → patch, `0.2.3 → 0.2.4`) with header/`PROTOCOL_VERSION`
    untouched (no proto change); published to crates.io.

---

## P0/P1 — Stage 2: kernel + manager repos

### Kernel lane

- [x] **V-02 — kernel: switch loader import to wire manifest.**
  (shipped 2026-08-21: kernel PR #41 merged; no `[patch.crates-io]` needed —
  wire 0.2.4 was already published)
  Marketplace still present and untouched → mergeable alone.
  - `src/plugins/loader.rs:3`:
    `use crate::marketplace::installer::{validate_manifest, InstallManifest};`
    → `use veyron_wire::manifest::{validate_manifest, InstallManifest};`.
    Pass `crate::auth::permissions::resolve_permission` as the injected
    resolver (same function the old code called inline at
    `installer.rs:800`).
  - `Cargo.toml:24`: wire dep gains `features = ["manifest"]`. While 0.2.4 is
    unpublished, add `[patch.crates-io]` git override (R8-07 precedent);
    drop it once published.
  - `validate_plugin_def` (`loader.rs:262`) keeps its config-permission
    cross-check via `normalize_permission` — that check is config semantics
    and stays kernel-side.
  - Files: `src/plugins/loader.rs`, `Cargo.toml`.
  - Acceptance: boot-time validation behavior identical (compat range,
    unknown permission, v2 per-action resolution); full suite green;
    `test_installer.rs` still passes against the untouched marketplace copy.

### Manager lane (new repo `../vynkor-manager`, born at extraction)

- [x] **V-03 — vynkor-manager scaffold + state module + drop-in writer.**
  (shipped 2026-08-22: manager PR #1)
  - Repo init per rename policy (`vynkor-manager`, binary `vynm` + lib
    target); README vynkor naming throughout; CI mirrors kernel gates
    (test/clippy/fmt) + the no-kernel-dep assertion (`cargo tree -i veyron`
    empty).
  - Port `state.rs` (154 L) verbatim, then apply seams:
    - §6.6: `state_dir()`, cache/plugin-dir helpers become `pub` (stage-3
      config layering must not fork path logic).
    - §6.2: `InstalledEntry` gains `source: String` next to `source_url`;
      ledger `schema_version` bump with back-compat read (missing field →
      `"official"`); updates/reinstalls resolve against origin source.
  - Extract the self-contained drop-in surface from `installer.rs`:
    `write_plugin_config(plugins_dir, params)` decoupled from the install
    flow (§6.4), plus toggle/uninstall (`disable_plugin_config`
    `installer.rs:702`, `enable_plugin_config` `:729`, uninstall) taking a
    plain params struct.
  - Files: new repo — `Cargo.toml`, `src/state.rs`, `src/dropin.rs`,
    `tests/` (port `tests/unit/test_state.rs`, 267 L).
  - Acceptance: `cargo test` green in the new repo; ledger round-trip keeps
    old ledgers readable (`source` defaults to `"official"`).

- [x] **V-04 — manager: registry client port + RegistrySource seam.**
  (shipped 2026-08-22: manager PR #2)
  - Port `registry.rs` (665 L prod) + `registry_tests.rs` (842 L, keep the
    `#[path]` separation pattern MA-16 established).
  - Introduce `RegistrySource { name, url, public_key: Option<String>,
    allow_unsigned: bool, cache_ttl_secs, enabled }` (§6.1). The port
    constructs exactly one instance from today's config keys
    (`registry_url` / `marketplace_public_key` / `registry_cache_ttl_secs`);
    all fetch/verify functions take `&RegistrySource` instead of loose tuples
    (also fixes `install()`'s current 8 arguments).
  - §6.3: cache path scheme `registry-cache/<name-or-urlhash>.json` under the
    state dir from day one, even with one source.
  - D8 https-only: an http `archive_url` (or http registry url) without
    `allow_unsigned: true` on that source is refused, same error shape as
    §7.3.
  - §7.3 unsigned gating lands now (fetch path is being touched anyway):
    key present → Ed25519 verify unchanged; no key + TTY → loud warning +
    `download anyway? [y/N]` default NO; no key + non-TTY + `allow_unsigned`
    → proceed with warning; no key + non-TTY + no flag → hard error naming
    the exact knob.
  - D5: pinned key and `DEFAULT_REGISTRY_URL` leave source code — they become
    the built-in default `official` source entry, overridable by config.
  - C2: `complete_slugs` moves here wholesale (`src/cli/complete.rs:13`).
  - reqwest: `default-features = false, features = ["json", "rustls-tls"]`.
  - Files: `src/registry.rs`, `src/source.rs`, `tests/`.
  - Acceptance: registry tests green; new tests for the unsigned-gating
    matrix (TTY prompt / flag bypass / hard error) and http-archive refusal;
    no compiled-in official URL/key outside the default source entry.

- [x] **V-05 — manager: install pipeline port (+ D2/D3 deletions during the
  (shipped 2026-08-22: manager PR #3 + wire 0.2.5/0.2.6)
  port, not after).**
  - Port the 8-step atomic pipeline from `installer.rs` (812 L): download w/
    progress bar, sha256, zip-slip-guarded extraction, bak+rename atomic
    swap, manifest validation, ledger record, drop-in write.
  - Manifest validation calls `veyron_wire::manifest::validate_manifest` with
    `default_resolver` (or injected equivalent).
  - D3: delete the hardcoded `let sandbox = installed.plugin_id != "network"`
    (`installer.rs:637`). Sandbox preference comes from the plugin's own
    `plugin.json` optional `sandbox` hint field (default `true`); operator
    edits the drop-in for anything else. (Manifest-side field addition lands
    in veyron-plugins separately.)
  - D2: drop hard install-time compat enforcement (`installer.rs:189` area) —
    the kernel re-validates authoritatively at boot (`loader.rs:277`). Keep
    integrity enforcement (sha256/signature/zip-slip/revocation). Optional
    pre-flight: query `GET /status` when the target kernel is reachable;
    unreachable → warn-only note ("compat enforced at kernel boot"). Never
    guess from vynm's own `CARGO_PKG_VERSION`.
  - Port `tests/unit/test_installer.rs` (1019 L) minus tests covering deleted
    behaviors; keep every security-boundary test (digest, zip-slip, signature,
    revocation, O_EXCL).
  - Files: `src/installer.rs`, `tests/`.
  - Acceptance: end-to-end install/remove/enable/disable against real
    archives; `database`/`secrets` install and run; no compat gate at install
    time; no plugin-id special case anywhere.

- [x] **V-06 — manager: vynm CLI.**
  (shipped 2026-08-22: manager PR #4)
  - clap CLI: `install/search/list/remove/enable/disable`; subcommands accept
    `--source <name>` from day one (validated against the single configured
    source at port time — adding N sources later changes resolution logic,
    not grammar, §6.5).
  - `list` reads the `installed.json` ledger; `search` hits the registry
    through `RegistrySource`.
  - plugins.d path resolution: port the kernel's `resolve_plugins_dir` logic
    verbatim (both sides derive `<config dir>/plugins.d` from the same
    `--config` file) + round-trip test on both sides (risk-table item).
  - Exit codes defined now as the scripting contract: 0 ok / 2 network /
    3 verification failure (finalized in V-16).
  - Files: `src/main.rs`, `src/cli.rs` (or equivalent), `tests/`.
  - Acceptance: manual e2e against a scratch `$HOME`; `--source` rejects
    unknown names with the configured list; exit codes asserted in tests.

### Join

- [x] **V-07 — kernel: cut the marketplace out.**
  (shipped 2026-08-22: veyron PR #43; reqwest kept for the surviving REST client — documented deviation)
  Requires V-02 (loader already on wire) + V-03…V-06 (manager functional).
  - Delete `src/marketplace/` entirely; `src/lib.rs` drops
    `pub mod marketplace` (C6).
  - `src/cli/plugin.rs` (538 L): `start/stop/restart/logs` STAY unchanged —
    they are pure REST calls to the kernel API. `list/search/install/remove/
    enable/disable` become delegation shims (D4): print
    `'marketplace' commands moved to vynm — run: vynm <args>`, exec `vynm`
    with forwarded args when present (zero user breakage); shim removal
    deferred to stage 3.
  - `__complete-slugs` shim follows D4/D6 (exec vynm or actionable message).
  - C3: inline `format_ts` (30-line civil-from-days formatter,
    `state.rs:131`) into `src/cli/devices.rs`.
  - `Cargo.toml` drops: `zip` (:70), `indicatif` (:71), `reqwest` (:66),
    `ed25519-dalek` (:47), `sha2` (:44). Bonus cleanup: verify `hmac` (:43)
    and `hkdf` (:45) have no users via `cargo tree -i` (frame MAC comes via
    `pub use veyron_wire::mac::*`) and drop if confirmed. `semver` (:68)
    stays — loader compat check uses it.
  - Delete marketplace unit tests here (`test_installer.rs`,
    `test_state.rs`, `registry_tests.rs` live in the manager now).
  - Files: `src/marketplace/` (deleted), `src/lib.rs`, `src/cli/plugin.rs`,
    `src/cli/complete.rs`, `src/cli/devices.rs`, `Cargo.toml`, `tests/`.
  - Acceptance: `vyn` binary contains no marketplace code (no module exists
    in the crate; spot-check symbols if paranoid); `cargo tree` shows the
    dropped deps gone; full suite green WITHOUT the manager checked out;
    `vynm install database` works standalone against this kernel and the
    plugin runs.

- [x] **V-08 — docs & gates (closes stage 2).**
  (shipped 2026-08-22: README operator section → vynm, ROADMAP F1 SHIPPED, AUDIT DC-1 CLOSED, VYNM_PLAN status)
  - README operator section rewritten around vynm (`vyn plugin install` →
    shim note); `ROADMAP.md` F1 marked shipped with a pointer here;
    `AUDIT.md` DC-1/F1 closed; Task Summary table row updated;
    `docs/VYNM_PLAN.md` status line updated ("implementation shipped").
  - Files: `README.md`, `ROADMAP.md`, `AUDIT.md`, `docs/VYNM_PLAN.md`.
  - Acceptance: docs consistent across repos; all gates green in every
    touched repo (kernel, wire, manager).

---

## P3 — Stage 3: manager productization (independent releases per D7)

Full-detail backlog (committed in plan §7.5/§7.6). Each task ships in its
own release; the drop-in format mini-spec (field → type → since-version)
lives in the manager repo and any format change is deliberate.

- [x] **V-09 — multi-source registries: config parsing + resolution engine.**
  ✅ DONE & verified 2026-08-22 (0.2.1; 136 tests incl. precedence matrix +
  live-binary multisource e2e; deviations documented: env mutates
  sources[0] in-place, explicit pin ignores `enabled:`, flag-tier for
  url/key reserved).
  - Config schema — AMENDED 2026-08-22 per `docs/VYN_PRODUCT_LAYOUT.md` §2:
    sections live in the SHARED product config `~/.config/vyn/config.yaml`
    (kernel reads its sections and ignores these; both parsers are
    unknown-key-tolerant). `registries:` list of `{name, url, public_key?,
    allow_unsigned?, cache_ttl_secs?, enabled?}`. Back-compat: single keys
    (`registry_url:` / `marketplace_public_key:`) keep working, mapped to
    name `"official"`. Omitting `registries:` entirely → built-in official
    default.
  - Precedence: CLI flags > env (`VYNM_REGISTRY_URL` etc.) > `registries:`
    list > back-compat single keys > built-in official default.
  - Resolution rules (§7.2): bare slug searches enabled sources in listed
    order, first match wins, always prints `resolved from <source>`;
    explicit `corp/database` form targets a named source, no search; unknown
    source name errors listing configured sources; `--source <name>` forces
    one for search/list.
  - Per-source caches activate the V-04 path scheme; ledger `source` field
    (V-03) drives origin enforcement.
  - Files: `src/config.rs` (new), `src/source.rs`, `src/registry.rs`,
    `src/cli.rs`.
  - Acceptance: parse-precedence tests; slug-shadowing prints the resolver
    warning; explicit form bypasses search; unsigned consent applies per
    source.

- [x] **V-10 — permission preview on install.**
  ✅ DONE & verified 2026-08-22 (`feat/v10-permission-preview` merged before
  V-12; gate sits in the manager's `commit_staged` AFTER staged-manifest
  validation, BEFORE any rename — so the preview comes from the real parsed
  manifest and a bad manifest never touches dest; refusal removes staging,
  nothing recorded).
  Before writing anything, print what the manifest declares
  (`permissions: [storage, network]` + v2 per-action requirements) and
  require confirmation. In a default-deny ecosystem the operator must see
  what they grant. `-y/--yes` skips for scripts; composes with the
  unsigned-source consent prompt (§7.3). Files: `src/installer.rs`,
  `src/cli.rs`. Acceptance: preview shown pre-write; `-y` skips; refusal
  aborts cleanly with nothing written.

- [x] **V-11 — version pinning.** ✅ DONE & verified 2026-08-22
    (`[source/]slug[@ver]` grammar; miss lists per-source availability;
    composes with V-09 ordering and the V-10 gate).
  `vynm install database@0.1.0` — exact-version install; mirrors the
  registry's own `slug@ver` revocation syntax. Files: `src/cli.rs`,
  `src/registry.rs` (entry lookup by exact version). Acceptance: pinned
  install resolves the exact version or fails listing available ones.

- [x] **V-12 — `vynm outdated` + `vynm update [slug]`.**
  ✅ DONE & verified 2026-08-23 (0.3.0): survey engine (origin-only
  resolution, one fetch per distinct source), ONE batch confirmation,
  rebuild detection w/ --force, restart hint; E2E verified incl.
  origin-enforcement against shadowing and digest refresh on repair.
  - `outdated`: report-only comparison of `installed.json` versions vs
    registry versions (the R10-03 cache already snapshots both).
  - `update`: bare form checks every installed plugin against its *origin*
    source (ledger `source` field) and applies all newer; `update <slug>`
    restricts to one. Batch prints old→new per plugin, one confirmation for
    the batch (`-y` skips). Only strictly newer semver triggers an update —
    equal version with different sha256 warns and needs `--force` (rebuild
    detection, never silent). Update = ordinary install pipeline from the
    origin source at the new version; drop-in rewritten; running plugin keeps
    executing the old binary until restarted (print restart hint). Unsigned
    gating applies unchanged.
  - Files: `src/commands/update.rs` (new), `src/registry.rs`, `src/cli.rs`.
  - Acceptance: outdated table correct; batch update honors origin sources;
    equal-version-different-hash requires `--force`.

- [x] **V-13 — `vynm verify [slug]`.** ✅ DONE & verified 2026-08-22.
    DESIGN NOTE (roadmap text was loose): archive-sha alone cannot verify a
    TREE offline — implemented as a canonical tree digest recorded at
    install time (`tree_sha256`, LEDGER_SCHEMA_VERSION 2→3 with back-compat
    read; pre-v3 entries report UNKNOWN BASELINE). Digest covers content +
    exec-mode + layout; chmod alone ⇒ TAMPERED exit 3. Unreadable tree =
    fail-closed TAMPERED.
  Re-hash installed trees against ledger sha256; detects manual edits /
  substitution offline. Acceptance: tampered tree reported with
  expected/actual hashes; clean tree passes; exit code contract honored.

- [x] **V-14 — key tooling: `vynm keygen` + `vynm sign`.** ✅ DONE & verified
    2026-08-22 (107→111 tests; E2E roundtrip + tamper-exit-3 confirmed;
    follow-up fix merged: `--verify` runs keyless, warn when a secret is
    passed). SCOPE AMENDED
    2026-08-22: crypto primitives move INTO vynm so the canonical S1 form
    has a single implementation (package.sh shrinks to a thin wrapper that
    calls `vynm sign`, then dies).
  - `keygen`: ed25519 pair via OS RNG (no heavy deps); public key printed
    hex for `marketplace_public_key:`; secret written as hex-seed file
    `0600` (`VEYRON_SIGNING_KEY_HEX`-compatible format), refuses overwrite
    without `--force`.
  - `sign`: ENTRY-level S1 signing —
    `vynm sign --key <file> --slug --version --sha256 --status
    --archive-url --min --max` prints the 128-hex signature (byte-equivalent
    replacement for package.sh's inline python). Document-level signing
    DEFERRED — the verify model is per-entry.
  - Follow-up backlog: `vynm package <dir>` orchestration (zip + checksum +
    sign + registry upsert) replacing package.sh entirely.
  - Files: `src/commands/keygen.rs`, `src/commands/sign.rs` (new),
    `ed25519-dalek` dep (already a transitive concept from the port).
  - Acceptance: generated key verifies a signed document end-to-end against
    a local source; secret file permissions asserted in tests.

- [x] **V-15 — install bypassing registries.**
  ✅ DONE & verified 2026-08-22 (0.2.4): shared `commit_staged` tail reused
  by both pipelines; ledger `source: "local"`; D8 names `--allow-unsigned`;
  full dev-loop E2E green (new→build→install ./x.zip→drop-in).
  `vynm install ./plugin-0.1.0.zip` or a direct archive URL — same pipeline
  minus resolution/download-from-registry; manifest validation + zip-slip +
  drop-in write unchanged; prints that no registry signature/sha256-channel
  guarantee applies. Primary use: developing own plugins. Files:
  `src/installer.rs`, `src/cli.rs`. Acceptance: local zip installs; D8
  https-only still enforced for direct http URLs without explicit consent.

- [x] **V-16 — CLI polish package.**
  ✅ DONE & verified 2026-08-24 (vynkor-manager **0.4.0**, PR vynkor-manager#20;
  notes in manager `docs/V-16_EXPERIENCE.md`): `vynm info <target>` full entry
  details; `--json` for search/list/info/outdated (no-match = null+empty
  results, never a special case); `--source` column in search tables;
  `--dry-run` for install/update (moves no bytes, ledger/dirs untouched —
  e2e verified); `vynm cache clean`; completions generated from the clap
  surface (`clap_complete`); hidden `__complete-slugs` reads LOCAL caches
  first per the NOTE below. Exit codes finalized TYPE-based: security
  refusals raise `VynmError::Verification` → exit 3 by variant, the
  message-sniffing heuristic is gone.
  `--source` column in search/list tables; `vynm info <slug>`; `--dry-run`;
  `--json` output; `vynm cache clean`; shell completions; documented exit
  codes finalized (0 ok / 2 network / 3 verification failure / …) as the
  scripting contract. NOTE 2026-08-22: `__complete-slugs`
  must read the local registry CACHE first (instant, offline); network only
  on miss/stale refresh. Registry sharding explicitly parked until plugin
  count demands it. Files: `src/cli.rs`, output formatting modules.
  Acceptance: `--json` machine-readable everywhere it applies; completions
  generated for bash/zsh/fish.

- [x] **V-17 — stage 4/A: rename code + packages to vynkor 0.0.1 (hard cut).**
  Wave 1 ✅ DONE & verified 2026-08-22 (wire/sdk/kernel/manager PRs merged,
  artifacts published, gates independently re-run green).
  Wave 2 ✅ DONE & verified 2026-08-22 (`vynkor-sdk 0.0.2` published with
  `VynkorClient/VynkorError`; `veyron-sdk 0.1.7` deprecation patch live;
  CI workflow added; 9 residual mentions — all documented exclusions incl.
  WS-subprotocol `veyron` kept paired with the kernel gateway). Scope in
  `docs/VYN_PRODUCT_LAYOUT.md` §8 "wave 2": `VeyronClient → VynkorClient`
  (sole prefixed public type), ~170 residual doc mentions, republish
  **0.0.2**, `veyron-sdk 0.1.7` deprecation patch, dir rename to
  `vynkor-sdk`. C++/Python SDKs deferred until after Phase B.
  Full design: `docs/VYN_PRODUCT_LAYOUT.md` §8. Zero external users ⇒ NO
  back-compat shims: old paths/envs die in this wave.
  1. `vynkor-wire 0.0.1`: crate rename + `vyn.sock`, `share/vyn`,
     `lib/vyn/plugins`, `VYN_*` env defaults → publish.
  2. `vynkor-sdk` (Rust) + PyPI `vynkor-sdk` 0.0.1: module `veyron` →
     `vynkor`; C++ SDK follows the repo/proto paths.
  3. Kernel: lib crate `veyron` → `vynkor`; imports tree-wide; env vars;
     data/lib/socket dirs; tests updated. Binary stays `vyn`.
  4. Manager: dep swap to `vynkor-wire 0.0.1`; config discovery lands here:
     precedence `--config`(alias `--kernel-config`) > `$VYN_CONFIG` >
     `~/.config/vyn/config.yaml` > `./config.yaml` legacy fallback; first-run
     template + idempotent `vyn init [--force]`; `vyn status` prints loaded
     path and warns on shadowing.
  5. Old crates get one final deprecation-notice patch each.
  6. GitHub renames LAST (org `veyron-core` → `vynkor`, repos → `vynkor*`;
     redirects cover old clones meanwhile).
  - Acceptance: every repo green on the new names end-to-end; fresh clone +
    build + full suites pass with ZERO references to `veyron` identifiers in
    code (docs history excepted).

- [x] **V-18 — stage 4/B: registry on Cloudflare R2 + re-sign.**
  ✅ SHIPPED (verified live 2026-08-26), with two recorded deviations from
  the text below:
  - DEVIATION 1 — versions: plugins were re-signed at their REAL released
    versions (0.1.x) via `vynkor-plugins/scripts/resign.py` (S1 seven-field
    form, `--check` mode verifies against the pinned key), NOT reset to
    0.0.1. Resetting would have discarded release history for zero value.
  - DEVIATION 2 — hosting: the registry + dist/ tree serve from the R2
    bucket's public r2.dev endpoint
    (`pub-6fd4….r2.dev`, wired into manager `source::official_source`);
    upload runs through `vynkor-plugins/scripts/publish-r2.sh` (rclone/S3:
    archives immutable, sidecars 1h, registry.json 5min, post-upload sha256
    verification) instead of an in-package.sh step. The custom domain
    `cdn.vynkor.dev` is still UNBOUND — swap one URL in `source.rs` once it
    is.
  1. ~~Domain + CF account + R2 bucket bound to `cdn.vynkor.dev`.~~
     CF account + bucket DONE; domain binding PENDING (only open item).
  2. ~~Upload step AND S1 seven-field signatures~~ DONE as publish-r2.sh +
     resign.py (see deviations).
  3. ~~Re-sign ALL plugins~~ DONE at real versions (deviation 1);
     `registry.json` regenerated, `signature.sig` refreshed.
  4. ~~`registry.json` STAYS IN GIT~~ DONE as specified.
  - Acceptance RE-RUN LIVE 2026-08-26: `vynm search database`,
    `info database`, and a real `vynm install database` against the DEFAULT
    official source all exit 0 with signature verification on; tamper
    refusal stays exit 3. Cosmetic finding: 19/27 published entries carry
    explicit `"status": ""` (older packagers); vynkor-manager normalizes
    empty → stable at parse time since PR #24.

- [ ] **V-19 — stage 4/C: installer, docs sweep, packaging.** ◐ PARTIAL
  1. ☐ Installer: the curl-pipe `install.sh` was REMOVED (kernel root and
     the diverged vynkor-web copy) — distribution is `cargo install
     vynkor-manager` (0.1.0 crates.io prep) plus a future AUR PKGBUILD.
     The script already targeted `~/.config/vyn/`, so no path migration
     was lost with it.
  2. ◐ Docs sweep across repos: README/roadmaps/audit largely speak vynkor;
     residual old-name mentions kept only as history.
  3. ☐ AUR PKGBUILD ships `/usr/bin/vyn` + `/usr/bin/vynm`; pacman owns
     updates (NO self-update by policy); completions ride V-16 later.
  - Acceptance: fresh machine + package install → `vyn start` → `vynm
    install database` works touching only XDG dirs under `~/.config/vyn`.
    (Install-from-source path verified manually; binary-package acceptance
    blocked on AUR.)

- [x] **V-20 — `vynm new <name>`: plugin scaffolding.** ✅ DONE & verified
    2026-08-22 (`include_str!` templates, identifier gate, acceptance build
    vs published sdk green).
  Decision 2026-08-22: templates are EMBEDDED in the binary (`include_str!`,
  zero new deps) — offline-first like the rest of vynm, no trust question
  for twenty-line hello-worlds. Remote signed templates revisit later via
  R2 (`templates/` prefix in the bucket, same signing model) once Phase B
  lands.
  - `vynm new <name>` scaffold: `plugin.json` (id/name, version 0.0.1,
    compat range), `Cargo.toml` (dep `vynkor-sdk`, bin `<name>`),
    `src/main.rs` minimal `Plugin` impl (hello action), `.gitignore`,
    README stub; prints next-step hints (build → sign → submit).
  - Name validated via `validate_identifier`; refuses an existing dir
    without `--force`.
  - Acceptance: the scaffolded plugin builds against the published
    `vynkor-sdk` out of the box.

**Parked (not promised):** ~~`rollback <slug>`~~ and ~~air-gapped
`bundle export/import`~~ — both SHIPPED 2026-08-26 in vynkor-manager
(rollback keeps `<slug>.prev` + ledger `previous`; bundle is a
transactional digest-verified offline zip; see manager
`docs/STAGE4_MANAGER_WAVES.md` wave 5). Nothing remains parked.

**Open questions to settle during stage 3** (plan §10): whether
`vynm list --installed` also queries `GET /plugins` when reachable (lean: no
— keeps vynm offline-pure); non-linux drop-in template should omit
`sandbox:` instead of writing a flag the kernel ignores there.

---

## Execution notes

- Risks and mitigations are catalogued in `docs/VYNM_PLAN.md` §9 (duplicated
  validation rot, accidental cross-deps, wire scope creep, publish friction,
  shim breakage, plugins.d divergence, slug shadowing, unsigned substitution,
  TLS bloat) — consult before deviating.
- Wire iteration before publish: git-dep `[patch.crates-io]` escape hatch
  (R8-07 precedent) in whichever repo needs unreleased manifest APIs.
- Related roadmap items: F5 (action→permission fallback removal) changes
  kernel-side policy only — the wire manifest module's injected resolver
  absorbs it; MA-01 (protocol/registry split) completes naturally inside
  vynm; R8-01/R8-02 permission enum derivation + drift tests moved with the
  manifest module (V-01).
