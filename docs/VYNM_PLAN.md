# VYNM — marketplace extraction plan (F1 / DC-1)

Working plan for extracting the marketplace out of the kernel into a standalone
`vynm` binary ("vynkor plugin manager"). Covers the verified coupling map, the
architecture decision, the staged migration, and the multi-source registry
design. Mirrors `docs/DUMB_CORE_AUDIT.md` §6-F1 / §7-decision-1 and the
`ROADMAP.md` F1 entry.

**Status:** **implementation shipped 2026-08-22 (stage 2 complete).**
Stage 1: manifest module shipped in veyron-wire 0.2.4–0.2.6 behind the
`manifest` feature (V-01). Stage 2: `vynkor-manager` repo born and functional
(V-03…V-06, PRs #1–#4); loader switched to the wire manifest (V-02);
kernel `src/marketplace/` deleted, CLI commands are delegation shims to vynm
(V-07, veyron PR #43). Stage-3 productization backlog (V-09…V-16) tracked in
`docs/VYNM_ROADMAP.md`. Per-task implementation notes live in each repo's
`docs/V-0*_EXPERIENCE.md`.
**Strategy:** two stages inside this repo, optional third outside.

---

## 1. Goal and scope

### What vynm is

> **vynm is a plugin installer. Nothing more.**

- downloads plugins from registries, verifies them, writes `plugins.d/`
  drop-ins, keeps the `installed.json` ledger, enables/disables/removes
- a CLI tool an operator runs manually — **no daemon, no service, no kernel
  IPC surface, no runtime role**
- the kernel never calls vynm and never depends on it; vynm works even when
  the kernel is not installed or not running (drop-ins are just files)

### Why (dumb core)

`src/marketplace/` (~2 485 LOC) is product logic inside the dumb byte router:
catalog policy, governance keys, upgrade detection, install pipeline. Every
registry/catalog change currently ships a kernel release. The manifesto says
the kernel owns exactly four jobs (transport, lifecycle, security, plumbing) —
package management is none of them.

### Non-goals

- no auto-update agent, no background refresh loop (operator runs `vynm`)
- no new kernel commands/endpoints — the drop-in contract already exists
- no plugin *runtime* involvement — vynm stops at writing config files

---

## 2. Verified coupling map (research findings, 2026-08-21)

Everything below was verified by grep/codegraph against `develop` — not
assumed from the audit text.

### 2.1 Code that moves to vynm

| File | Size | Contents |
|---|---|---|
| `src/marketplace/registry.rs` | 1509 L | registry HTTP client, Ed25519 maintainer signature verify (pinned key), versioned disk cache (`registry-cache.json`, schema v1), revocation enforcement, semver kernel-compat range policy, registry v2 map-form parsing |
| `src/marketplace/installer.rs` | 822 L | 8-step atomic install pipeline (download w/ progress bar, sha256, zip-slip-guarded extraction, bak+rename atomic swap, manifest validation), drop-in writer/toggle, `uninstall`, `validate_slug` |
| `src/marketplace/state.rs` | 154 L | `installed.json` ledger (atomic temp+rename writes, corrupt-tolerant), `format_ts` |
| `src/cli/plugin.rs` (part) | ~half | `list/search/install/remove/enable/disable`. **`start/stop/restart/logs` STAY in vyn** — they are pure REST calls to the kernel API (`api_post /plugins/{id}/start`, …) |
| tests | ~41 cases | `tests/unit/test_installer.rs`, `tests/unit/test_state.rs`, `src/marketplace/registry_tests.rs` |

### 2.2 Coupling points (the hard part)

| # | Where | What | Severity |
|---|---|---|---|
| C1 | `src/plugins/loader.rs:3` | kernel boot imports `validate_manifest` + `InstallManifest` from `marketplace::installer`; `validate_plugin_def` (`loader.rs:262`) parses each plugin's `plugin.json` at boot: semver compat range vs kernel version, declared permissions ∈ known set, v2 per-action permission resolution; then cross-checks manifest permissions against config-granted permissions | **deepest — blocks everything** |
| C2 | `src/cli/complete.rs:4` | `complete_slugs` fetches the live registry for shell-completion slugs | easy — moves to vynm |
| C3 | `src/cli/devices.rs:38` | uses `marketplace::state::format_ts` (30-line civil-from-days date formatter) | trivial — inline a copy |
| C4 | `installer.rs:637` | hardcoded business rule `let sandbox = installed.plugin_id != "network"` — the kernel-side installer knows a specific plugin's sandbox constraints | delete (see §5-D3) |
| C5 | `installer.rs:189`, `loader.rs:274` | both compute "kernel version" as `env!("CARGO_PKG_VERSION")` — after the split vynm's own version ≠ running kernel's version | redesign (see §5-D2) |
| C6 | `src/lib.rs:8` | `pub mod marketplace` in the kernel lib target | delete after move |
| C7 | `Cargo.toml` | marketplace-only deps compiled into the kernel binary | drop (see §2.3) |

### 2.3 Dependency analysis (verified: zero usage outside `src/marketplace/`)

Move to vynm's Cargo.toml, drop from the kernel's:

- `zip` — archive extraction
- `indicatif` — download progress bar
- `reqwest` — registry/archive HTTP client (no other user anywhere in src/)
- `ed25519-dalek` — maintainer signature verification
- `sha2` — archive integrity digest

Bonus cleanup while touching Cargo.toml: `hmac` and `hkdf` have **no users in
src/** either — the frame MAC comes via `pub use veyron_wire::mac::*`
(`src/auth/frame_mac.rs:1`). Verify with `cargo tree -i` before removing.

Stays in the kernel: `semver` (used by `loader.rs` compat check), `serde_json`,
everything else.

### 2.4 Security analysis — what boot-time validation actually buys

Key finding that unlocks the whole design: **`validate_manifest` at boot is
NOT a trust anchor.**

- real enforcement is independent and stays in the kernel:
  default-deny permissions, JWT claims ∩ config allowlist clamp at
  registration (`protocol.rs` T-04), runtime `check_permission`
  normalization (`auth/permissions.rs`). An unknown permission name declared
  in a manifest simply never matches anything → denied, fail-closed.
- boot-time validation is **fail-early UX**: loud error at boot instead of
  silent permission degradation later. Worth keeping — but its home is a
  shared crate, not the marketplace.

What IS a security boundary and must survive the move intact (audit step 2):
sha256 archive digest, zip-slip guard, Ed25519 signature verification,
revocation gate, `create_new` (O_EXCL) drop-in write, slug/path traversal
guards. These are good; they port verbatim.

---

## 3. Strategy: separate repo immediately, wire carries the shared code

Revised 2026-08-21 — supersedes the earlier workspace-first plan. Decision:
**`vynkor-manager` is born as its own repository right away**, named per the
rename-in-progress policy (like `vynkor-client-android` was). The blocker
that originally forced a same-repo workspace (shared manifest code) dissolves
by putting the manifest module into **veyron-wire** behind a cargo feature:
wire is already the ecosystem's shared foundation crate consumed cross-repo
from crates.io (precedent: `veyron_wire::socket::default_private_dir`, S2).

```
Stage 1 (wire repo)        Stage 2 (kernel + manager)    Stage 3 (manager only)
───────────────────────    ──────────────────────────    ──────────────────────
wire PR: manifest          kernel: loader import         productization:
module behind the          switch → green; NEW           multi-source N,
"manifest" feature         vynkor-manager repo           keygen/sign,
(port InstallManifest,     ports marketplace code;       update/outdated,
validate_manifest,         kernel PR deletes             verify, permission
known_permissions,         src/marketplace, adds         preview, pin @ver,
compat check)              shims, drops 5 deps           CLI polish
```

Each stage is ordinary per-repo PRs; every merged commit is green. The old
audit assumption "same release cadence as vyn" is explicitly dropped — see
D7 (independent versioning): the only real contract between kernel and
manager is the drop-in file format, which the kernel validates loudly at boot
anyway.

---

## 4. Target architecture

```
veyron-wire/ (existing repo)        vynkor-manager/ (NEW repo)
├── proto/, framing, mac …          ├── Cargo.toml       bin vynm (+ lib target)
├── src/manifest.rs  ← NEW          ├── src/main.rs      clap CLI: install/search/
│   #[cfg(feature = "manifest")]    │                    list/remove/enable/disable
│   InstallManifest, ActionSpec,    ├── src/registry.rs  ← was kernel registry.rs
│   validate_manifest,              ├── src/installer.rs ← was installer.rs
│   known_permissions(),            ├── src/state.rs     ← was state.rs
│   check_kernel_compatibility      ├── src/source.rs    RegistrySource (§6.1)
│   adds: serde_json, semver        ├── tests/           ported unit tests
│   (feature-gated so Rust-SDK      └── README.md        vynkor naming throughout
│   consumers compile neither)
└── …

kernel (this repo): loader imports veyron_wire::manifest::*; src/marketplace/
GONE; lib.rs drops pub mod marketplace; Cargo.toml drops zip, indicatif,
reqwest, ed25519-dalek, sha2 (+ hmac/hkdf if confirmed unused); wire dep
gains features = ["manifest"].
```

Why this shape:

- **manifest module in wire** resolves C1 with zero new repos-for-a-purpose:
  both consumers already depend on published wire. The earlier rejection
  ("packaging metadata does not belong in the protocol crate") is overturned
  by precedent — S2 already put socket utilities into wire; wire IS the
  shared foundation crate. The cargo feature keeps Rust-SDK consumers free of
  the added serde_json/semver weight.
- **separate repo from day one** matches how the project already works
  (wire/sdk siblings), gives vynm clean CI from its first commit, and makes
  the rename policy trivial (born as vynkor-manager). A later move INTO a
  repo would have been cheap too — but born-here avoids the temporary
  workspace architecture entirely.
- dependency direction stays acyclic: `veyron-wire ← {kernel, vynm}`. The
  manager never depends on the kernel crate (it would drag axum/tokio-full
  along); the kernel never depends on the manager.

---

## 5. Sub-decisions (decision-complete)

### D1 — who validates manifests at boot

The manifest module in **veyron-wire** behind the `manifest` feature (§4).
Kernel loader switches import from
`crate::marketplace::installer::{validate_manifest, InstallManifest}` to
`veyron_wire::manifest::*`. Same checks, same fail-loud semantics, single
implementation consumed by both repos from crates.io.

Note: `validate_manifest` resolves v2 per-action permissions via
`auth::permissions::resolve_permission` — that helper is slated for removal
by F5 (hardcoded action→permission fallback). The wire module takes the
resolver as an injected `fn(&str) -> Option<PermissionType>` parameter so F5
can change kernel policy without a wire change.

### D2 — kernel-version compat check after the split

Finding: **the kernel already re-validates compat at boot with its own
authoritative version** (`loader.rs:277`). vynm's install-time check is
duplicate enforcement against a version it can't know reliably.

Decision: vynm keeps integrity enforcement (sha256/signature/zip-slip/
revocation — its job) and **drops hard compat enforcement at install time**.
Optional pre-flight: query `GET /status` on the target kernel when reachable;
if unreachable, print a warn-only note ("compat enforced at kernel boot").
No guessing from vynm's own `CARGO_PKG_VERSION`.

### D3 — the `!= "network"` special case

Deleted. Sandbox preference comes from the plugin's own `plugin.json`
(optional `sandbox` hint field, default `true`), operator edits the drop-in
for anything else. The generated drop-in template comment already invites
editing. (Manifest-side field addition lands in veyron-plugins separately.)

### D4 — `vyn plugin ...` surface after extraction

Lifecycle commands (`start/stop/restart/logs`) stay — they are kernel API
clients, not marketplace. Marketplace commands become thin delegation shims:

```
$ vyn plugin install database
'marketplace' commands moved to vynm — run: vynm install database
```

Shim execs `vynm` with forwarded args when present (zero user breakage);
removal of the shims happens in stage 2 once vynm ships everywhere vyn does.

### D5 — pinned key and DEFAULT_REGISTRY_URL leave source code

Audit step 4. Both become config (see §7 multi-source design). The compiled-in
official URL/key survive only as the built-in default source entry, overridable
by config. Rationale: private deployments point vynm at their own registry
without rebuilding.

### D6 — small untangles

- `complete_slugs` moves to the manager wholesale (it is a registry feature);
  `vyn __complete-slugs` shim follows D4.
- `format_ts` inlined into `cli/devices.rs` (30 lines, no reason to share).

### D7 — independent versioning (decided 2026-08-21)

The manager versions on its own semver; tags are its own (`v0.x.y` in its
repo). Supersedes the audit's "share the version and release cadence of vyn"
assumption — that was written before D2 dropped install-time compat
enforcement.

What actually couples kernel and manager is one thing: **the drop-in file
format** (flat optional-field YAML) and the `plugins.d` path convention.
Coupling rules:

- new field written by new manager + old kernel = serde ignores unknown
  fields, defaults apply → fail-safe direction;
- any real break = loud boot error in the kernel loader, never silent;
- the drop-in format gets a mini-spec in the manager repo (field → type →
  since-version) so changes are deliberate, not accidental;
- soft matrix check only: when the kernel is reachable, manager may warn if
  the running kernel version is outside its tested range (`GET /status`) —
  warn-only, never a gate (D2).

### D8 — https-only downloads by default (decided 2026-08-21)

Unencrypted transport is not the same risk as unsigned content, but both
reduce to "do you trust this channel". One flag covers it honestly:
archive downloads are **https-only unless the source has
`allow_unsigned: true`**, which then means exactly "I trust this channel
end-to-end" (unsigned registry document over http included). An http
`archive_url` without that flag is refused with the same error shape as
§7.3.

---

## 6. Seams to build during the port (stage 2, so stage 3 needs no rewrite)

These are the "change nothing later" guarantees. All cheap if done while the
code is being moved anyway, expensive after.

1. **`RegistrySource` struct inside the manager** — `{name, url,
   public_key: Option<String>, allow_unsigned: bool, cache_ttl_secs,
   enabled}`. The port constructs exactly one from today's config keys; all
   registry/installer functions take `&RegistrySource` instead of loose
   `(url, key, ttl)` argument tuples (today `install()` takes 8 args — this
   also fixes that).
2. **Ledger records origin** — `installed.json` entries gain `source: <name>`
   next to the existing `source_url`; ledger schema_version bump with
   back-compat read (missing field → `"official"`). Updates/reinstalls must
   resolve against the *origin* source, not whichever source answers first.
3. **Cache keyed per source** — cache path scheme
   `registry-cache/<name-or-urlhash>.json` under the state dir from day one,
   even with one source. Migrating a single flat cache file later means a
   format break nobody wants.
4. **Drop-in writer decoupled from install flow** —
   `write_plugin_config(plugins_dir, params)` where params is a plain struct;
   usable by install, and later by any future command that needs to emit a
   drop-in.
5. **CLI shaped for growth** — subcommands accept `--source <name>` from day
   one (validated against the single configured source at port time). Adding
   N sources later changes resolution logic, not the CLI grammar.
6. **State-dir helpers exported, not private** — `state_dir()`,
   `cache_path()`, `plugin_dir()` are `pub` in the manager so stage-3 config
   layering doesn't fork path logic.
7. **Unsigned-mode gating lands with the move** — TTY prompt / hard error /
   `allow_unsigned` bypass (§7.3) and https-only enforcement (D8) are part of
   the fetch path being touched anyway, not a follow-up.

---

## 7. Multi-source registries (stage 2 design, seams laid in stage 1)

Requirement: easily change where plugins come from; support several plugin
sources. Verdict: **yes — worth building, staged.** It aligns exactly with
audit step 5/D5 (keys and URLs out of source) and costs almost nothing when
the §6 seams exist. It widens the trust surface, so the consent rules in
§7.3 are non-negotiable — keys themselves are optional (decided 2026-08-21),
explicit consent is not.

### 7.1 Config schema (lives in the kernel config.yaml — same file vynm already reads for `plugins_dir`)

```yaml
# replaces registry_url: / marketplace_public_key: (both keep working as a
# single-entry back-compat form, mapped to name "official")
registries:
  - name: official                 # default source when omitted entirely
    url: https://raw.githubusercontent.com/veyron-core/vynkor-plugins/main/registry.json
    # public_key omitted → built-in pinned key applies by default;
    # relaxing official to unsigned needs allow_unsigned like any source
    cache_ttl_secs: 3600
    enabled: true

  - name: corp                     # private/corporate mirror example
    url: https://registry.corp.example.com/registry.json
    public_key: "<base64 ed25519>" # recommended; omit → unsigned mode (§7.3)
    enabled: true

  - name: lab                      # unsigned self-hosted example
    url: http://lab.lan/registry.json
    allow_unsigned: true           # explicit written consent (§7.3)
```

Precedence: CLI flags > env (`VYNM_REGISTRY_URL` etc.) > `registries:` list >
back-compat single keys > built-in official default.

### 7.2 Resolution rules

- bare slug (`vynm install database`) → search enabled sources in listed
  order, first match wins; print `resolved from <source>` so ambiguity is
  never silent; if the slug exists in several sources, suggest the explicit
  form.
- explicit form (`vynm install corp/database`) → named source, no search.
- unknown source name → error listing configured sources.
- `--source <name>` flag forces a source for search/list.

### 7.3 Trust rules — keys are OPTIONAL, consent is explicit (decided 2026-08-21)

Ecosystem precedent: pip/npm/homebrew ship unsigned; apt warns on unsigned
repos. Requiring keys for every self-hosted source would kill the feature.
Decision: **any** source may run without a key — including `official` — but
an unsigned source is always an explicit, recorded decision:

| situation | behavior |
|---|---|
| key present | Ed25519 verify; tamper = refuse (unchanged) |
| no key, interactive (TTY) | loud warning (`source 'x' is UNSIGNED — downloads are not tamper-proof`) + `download anyway? [y/N]`, default NO |
| no key, non-TTY, `allow_unsigned: true` on that source | proceed, same warning printed |
| no key, non-TTY, no flag | **hard error** naming the exact knob: `refusing unsigned source 'x' in non-interactive mode — set allow_unsigned: true for it in config.yaml` |

What keyless actually loses (honest framing): sha256 is still checked, but it
arrives over the same channel as the archive — it catches corruption, not
substitution. The signature is the only channel-substitution defense.
Mitigations that remain without a key:

- **TOFU via the ledger**: `installed.json` stores the archive sha256 of the
  first install; reinstall/skip logic already compares hashes, so silent
  substitution of the same version after first install is detected.
- revocation lists are still honored — trusting the registry document is
  exactly what `allow_unsigned` consents to.
- sha256, zip-slip guard, O_EXCL drop-in write apply identically to signed
  and unsigned sources. There is no "skip checks" flag — only "no signature
  to check".

Default posture: `official` ships keyed (built-in pinned key). Relaxing it
requires the same explicit `allow_unsigned: true` as any other source.

### 7.4 Cache and ledger layout (per source)

```
~/.local/share/veyron/           (state dir, XDG)
├── installed.json               ← gains "source" per entry
└── registry-cache/
    ├── official.json            ← existing RegistryCache schema, per source
    └── <urlhash>.json           ← unnamed/url-keyed fallback
```

### 7.5 Implementation split

- port (stage 2): `RegistrySource` exists (incl. `allow_unsigned`), exactly
  one instance, all internals source-parameterized (§6). Unsigned-mode gating
  and https-only enforcement land with the fetch-path move (§6.7).
- stage 3: config parser accepts the list, resolution engine, explicit-form
  grammar (`source/slug`), per-source caches, ledger origin enforcement, docs.

### 7.6 Stage 3 feature set (decided 2026-08-21)

Committed backlog beyond N sources:

- **Permission preview on install** — before writing anything, print what
  the manifest declares (`permissions: [storage, network]`, plus v2
  per-action requirements) and require confirmation. In a default-deny
  ecosystem the operator must see what they grant. `-y/--yes` skips for
  scripts; composes with the unsigned-source consent prompt (§7.3).
- **Version pinning**: `vynm install database@0.1.0` — exact-version install;
  mirrors the registry's own `slug@ver` revocation syntax.
- **`vynm outdated`** — report-only comparison of `installed.json` versions
  vs registry versions (the R10-03 cache already snapshots both).
- **`vynm update [slug]`** — perform updates. Bare `update` checks every
  installed plugin against its *origin source* (ledger field, §6.2) and
  applies all with newer versions; `update <slug>` restricts to one.
  - batch prints old→new per plugin, one confirmation for the batch
    (`-y` skips);
  - only strictly newer semver triggers an update — equal version with a
    different sha256 warns and needs `--force` (rebuild detection, never
    silent);
  - update = the ordinary install pipeline from the origin source at the new
    version; drop-in rewritten; the running plugin keeps executing the old
    binary until restarted (print the restart hint);
  - unsigned-source gating applies unchanged.
- **`vynm verify [slug]`** — re-hash installed trees against ledger sha256;
  detects manual edits/substitution offline. Data already exists.
- **Key tooling**: `vynm keygen` (ed25519 pair; public key printed for the
  config, secret written 0600) and `vynm sign <registry.json>` (signs the
  document so `public_key:`-configured sources verify it). Without these the
  key feature is unusable for self-hosters.
- **Install bypassing registries**: `vynm install ./plugin-0.1.0.zip` or a
  direct archive URL — same pipeline minus resolution/download-from-registry;
  manifest validation + zip-slip + drop-in write unchanged; prints that no
  registry signature/sha256 applies. Primary use: developing own plugins.
- **CLI polish package**: `--source` column in search/list tables,
  `vynm info <slug>`, `--dry-run`, `--json` output, `vynm cache clean`,
  shell completions, documented exit codes (0 ok / 2 network / 3 verification
  failure / …) as the scripting contract.

Parked (not promised): `rollback <slug>` (the install pipeline's `.bak`
mechanism is half of it), air-gapped `bundle export/import`.

---

## 8. Migration plan (cross-repo, every merged commit green)

Stages map to §3. Order chosen so each repo is green after every PR.

**Stage 1 — wire repo:**

1. **wire: manifest module** — new `src/manifest.rs` behind
   `#[cfg(feature = "manifest")]`: `InstallManifest`, `ActionSpec`,
   `validate_manifest(path, kernel_ver, resolver)`, `known_permissions()`,
   `check_kernel_compatibility()`; adds serde_json + semver under the
   feature, off by default (SDK consumers compile neither). R8-02's drift
   test moves here — every proto enum variant must pass `validate_manifest`
   in both name forms. Publish per wire's README bump rules.

**Stage 2 — kernel + manager repos (2 and 3 interleave; 4 needs both):**

2. **kernel: switch loader import** — `loader.rs` uses
   `veyron_wire::manifest::*`; wire dep gains `features = ["manifest"]`.
   Marketplace still present and untouched → mergeable alone.
3. **create vynkor-manager** — port registry/installer/state + CLI; apply
   the seams during the port (§6): `RegistrySource`, ledger `source` field,
   per-source cache paths, decoupled drop-in writer, `--source` flag,
   unsigned gating + https-only (D8). Delete `!= "network"` (D3) and hard
   install-time compat check (D2) during the port, not after.
4. **kernel: cut the marketplace out** — delete `src/marketplace/` +
   `pub mod marketplace`; marketplace CLI commands become shims (D4);
   inline `format_ts` into devices.rs (C3); Cargo.toml drops zip / indicatif
   / reqwest / ed25519-dalek / sha2 (+ verify hmac/hkdf via `cargo tree`);
   marketplace unit tests deleted here (they live in the manager now).

Acceptance (from the audit, unchanged):

- `vyn` binary contains no marketplace code (no marketplace module exists in
  the crate; spot-check symbols if paranoid)
- `vynm install` works standalone against a kernel that has no marketplace
  module; `database`/`secrets` still install and run
- full suite green in every touched repo: kernel `cargo test --all
  --all-features` + clippy `-D warnings` + `fmt --check`; manager same gates

Test moves: installer/state tests → manager `tests/`; `registry_tests.rs`
keeps the `#[path]` separation pattern MA-16 established; manifest + drift
tests move into wire with the module. New tests: ledger `source` back-compat,
`RegistrySource` parse precedence, unsigned-gating matrix (TTY prompt / flag
bypass / hard error), http-archive refusal (D8), shim exec behavior.

5. **docs & gates (kernel side, closes stage 2)** — README operator section
   rewritten around vynm (`vyn plugin install` → shim note); `ROADMAP.md` F1
   marked shipped with a pointer here; this plan's status line updated.

**Stage 3 — manager productization:** §7.5/§7.6 backlog; independent
releases per D7.

---

## 9. Risks and mitigations

| Risk | Mitigation |
|---|---|
| duplicated validation logic rots again (R8 lesson) | single implementation in wire's manifest module; R8-02 drift test (every proto enum variant passes both name forms) moves with it |
| accidental kernel ↔ manager dependency | direction is one-way through published wire; manager CI asserts no kernel dep (`cargo tree -i veyron` empty), kernel CI drops the 5 marketplace deps and stays green without the manager checked out |
| wire scope creep / SDK weight from the manifest module | cargo feature `manifest`, default off — SDK consumers compile neither serde_json nor semver of it; CI asserts default-feature build unaffected |
| wire publish friction slows manifest iteration | module is additive-only by design; policy changes ride the injected resolver (F5 never touches wire); git-dep `[patch]` escape hatch exists for pre-publish iterations (R8-07 precedent) |
| user scripts break on removed `vyn plugin install` | delegation shims with actionable message (D4); shim removal deferred to stage 3 |
| vynm/kernel plugins.d path divergence | both derive `<config dir>/plugins.d` from the same `--config` file; manager resolves via the kernel's `resolve_plugins_dir` logic ported verbatim + round-trip test on both sides |
| multi-source slug shadowing attacks (evil source defines `database`) | resolution order is operator-configured; `resolved from <source>` always printed; explicit `source/slug` form documented as the safe default for scripted use |
| unsigned-source archive substitution | explicit `allow_unsigned` consent + TTY prompt defaulting to NO + TOFU hash compare in the ledger (§7.3); signed sources unaffected |
| reqwest default-TLS feature bloat | start with `default-features = false, features = ["json", "rustls-tls"]`; revisit if a source needs native TLS |

---

## 10. Open questions (decide before/during stage 3)

1. Should `vynm list --installed` also show kernel-visible state by querying
   `GET /plugins` when reachable? (Lean: no — keeps vynm offline-pure;
   `vyn status` exists.)
2. RESOLVED 2026-08-26 (manager PR #23): non-linux drop-in templates omit
   the `sandbox:` key entirely instead of writing a flag the kernel ignores
   there.

---

## 11. Pointers

- Audit source: `docs/DUMB_CORE_AUDIT.md` §4-DC-1, §6-F1, §7-decision-1
  (this document supersedes its cadence/layout details where they differ)
- Roadmap entry: `ROADMAP.md` "Immediate — Dumb-core audit" F1 + Task Summary
- Related: MA-01 (split protocol.rs/registry.rs monoliths — registry split
  naturally completes inside vynm), MA-16 (registry tests separation, done),
  F5 (action→permission fallback removal — affects the wire manifest
  module's injected resolver),
  R8-01/R8-02 (permission enum derivation + drift tests — move with manifest
  crate)
