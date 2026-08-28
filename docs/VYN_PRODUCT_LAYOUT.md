# Vynkor product layout — paths, config discovery, packaging

Authoritative record of the stage-3 productization decisions (2026-08-22,
design session). Brand/name of the product: **vynkor**. Short name used for
**all user-facing filesystem paths**: **`vyn`** (the stable binary name per
the rename policy). Where this file and older docs disagree, this file wins.

## 1. Path map (old → unified)

| Purpose | Today | Target |
|---|---|---|
| Product config | `./config.yaml` (cwd-relative dev habit), explicit `--config` | `~/.config/vyn/config.yaml` — ONE file, both tools |
| Drop-ins | `<config dir>/plugins.d/*.yaml` | unchanged mechanism → `~/.config/vyn/plugins.d/` by derivation |
| Plugin binaries | `~/.local/lib/vyn/plugins/<slug>/` | `~/.local/lib/vyn/plugins/<slug>/` |
| State (ledger, registry cache) | `~/.local/share/vyn/` | `~/.local/share/vyn/` |
| Runtime socket | `$XDG_RUNTIME_DIR/vynkor.sock` | `$XDG_RUNTIME_DIR/vyn.sock` — **deferred**, see §6 |
| PID / log files | `~/.vynkor/run`, log under data dir | `~/.local/state/vyn/` (XDG state home) |

Rationale for the split (canonical XDG): config = small human-edited files;
lib = heavy executables; share = persistent state; runtime = tmpfs sockets;
state = logs/pids. Backups of `.config` then never drag plugin gigabytes.

## 2. One shared config, sections per tool

Both binaries read the SAME file; each parses only its sections and ignores
the rest (both parsers are already unknown-key-tolerant):

```yaml
# ~/.config/vyn/config.yaml
port: 8080                # kernel
jwt_secret: change-me     # kernel (first-run template writes a random one)
tls_cert_path: ...        # kernel

plugins_dir: ...          # OPTIONAL override; both honor it (contract!)
registries:               # vynm (V-09 schema)
  - name: official
    url: ...
    public_key: ...
allow_unsigned: false     # vynm §7.3 knob
```

Why one file instead of a separate `~/.config/vynm/`: single source of truth
for the drop-in contract kills the classic "installed but never spawns"
divergence; each component reading its own sections violates nothing (post
V-07 the kernel has zero marketplace code and ignores those keys entirely).

### Discovery chain

1. `--config <path>` explicit flag wins (`--kernel-config` accepted as alias
   for vynm; old `--config` kept)
2. `$VYN_CONFIG` env
3. `~/.config/vyn/config.yaml` (product default)
4. `./config.yaml` — legacy dev fallback, kept working; `vyn status` prints
   WHICH file was loaded and warns when a cwd file shadows the XDG one.

Registry keys in old locations stay readable for back-compat, marked
deprecated in docs (removal = stage-4 cleanup at earliest).

## 3. Why the drop-in dir is called `plugins.d`

Unix `.d` convention: fragments consumed by ONE program, merged in sorted
order (`apt/sources.list.d`, `sudoers.d`, systemd `*.service.d`). Per-plugin
files instead of one central blob ⇒ atomic per-file add/remove/rename
(disable = rename to `.disabled`, R10-04), deterministic glob order, and
future plugin packages can drop their own fragment without editing shared
files. Name stays; the path it lives under follows §1 automatically via the
existing derivation — zero extra logic.

Binaries deliberately do NOT live in the config tree: executables are lib
content (and can be huge). Kernel spawns whatever absolute `binary:` path
each drop-in records, so the two trees never need to coincide.

## 4. First run & templates

- `vyn start` with no config at the discovered location → writes a commented
  template with a freshly generated random `jwt_secret`, prints its path,
  proceeds (auth ON by default).
- `vyn init` — explicit, idempotent template generator (never overwrites an
  existing file; `--force` to reset).
- `install.sh` (repo root installer) must generate the SAME location
  (`~/.config/vyn/`) — its current draft targets `~/.config/vyn/` and
  needs updating together with V-17.
- `vyn status` gains "loaded config: <path>" output.

## 5. Binaries, updates, packaging

- Packaged world: `/usr/bin/vyn` + `/usr/bin/vynm`, owned exclusively by the
  package manager (AUR single package `vynkor` shipping BOTH binaries —
  pacman `-Syu` updates the pair atomically, no version skew by construction).
- NO self-update command: it fights the package manager's database. Updates
  belong to pacman/yay. Local-dev world keeps `cargo build/install --path`.
- Version-skew tolerance between the two tools rests on the drop-in mini-spec
  (field → type → since-version, maintained in vynkor-manager) plus the
  advisory-only `/health` pre-flight. Independent releases remain possible at
  the crate/tag level (D7); distribution-wise ship them together.
- Plugins update through `vynm update` (V-12): origin-source aware, ledger
  sha256 drift detection, running plugin keeps executing the old binary until
  kernel restart (hint printed).

## 6. Migration & the socket caveat

Filesystem moves (data, lib, config) are cheap and local:

- One-time migration on first access: if the new dir is absent and the old
  one exists → move it (same-FS rename, copy otherwise), then proceed;
  one-release fallback READ of old paths for safety.
- **Pitfall:** drop-ins record ABSOLUTE `binary:` paths. Migrating the plugin
  tree must rewrite `binary:` prefixes in every `plugins.d/*.yaml`
  (or regenerate them from the ledger) — a plain directory move silently
  breaks spawning. Ship as part of the migration step (vynm-side helper +
  kernel-side acceptance test).
- **Socket rename is NOT free:** the default lives in `vynkor-wire/socket.rs`
  and is duplicated in every SDK's connection code — renaming means a
  coordinated wire + 3-SDK release. Recommendation: rename filesystem paths
  now (§1 rows above except socket), keep `vynkor.sock` until the full
  rename milestone, then flip wire+SDKs together. Operators who care today
  can set `socket_path:` explicitly in config.

~~Socket/env renames were initially deferred to a future milestone~~
**SUPERSEDED 2026-08-22: the vynkor 0.0.1 big-bang does the HARD CUTOVER
now** (§8). Pre-production status means zero external users ⇒ no fallback
windows, no `VYNKOR_*` shims: everything flips to `vyn.sock`, `share/vyn`,
`lib/vyn/plugins`, `VYN_*`/`VYNM_*` env vars in one coordinated wave.

## 7. Task breakdown (amends stage-3 backlog)

- **V-09 amendment:** `registries:` section lives in the SHARED
  `~/.config/vyn/config.yaml` (not a vynm-private file); discovery chain per
  §2; `--kernel-config` alias; legacy single keys deprecated.
- **V-17 (new, kernel+manager):** unified config home & discovery chain —
  precedence chain, first-run template on `start`, idempotent `vyn init`,
  status transparency, `install.sh` alignment, cwd-fallback deprecation
  notice.
- **V-18 (new, cross-repo light):** path unification `share/vyn`,
  `lib/vyn/plugins` + one-time migration incl. drop-in `binary:` rewrite +
  fallback-read window. Socket/env rename explicitly deferred (§6).
- **V-19 (new, packaging):** single-package AUR PKGBUILD shipping both
  binaries; release-notes flow; completions ride V-16.

Acceptance for the whole block: fresh machine + `pacman -U vynkor.pkg` →
`vyn start` → `vynm install database` works touching ONLY `~/.config/vyn`,
`~/.local/share/vyn`, `~/.local/lib/vyn` — no source checkouts, no manual
paths.


## 8. Decision: vynkor 0.0.1 big-bang (2026-08-22)

Full ecosystem rename + package reboot happens NOW, not at a future
milestone. Uniquely cheap window: zero external users (hard cutover beats
migration shims), the registry needs re-signing anyway (S1 fix, PR
vynkor-plugins#23 folds in), and `install.sh` is being born right now.

| Layer | Now | Becomes |
|---|---|---|
| GitHub | org `vynkor-core`, repos `vynkor*` | org `vynkor`, repos `vynkor*` (renames last — redirects cover old clones) |
| crates.io | `vynkor-wire 0.2.6`, `vynkor-sdk 0.1.3` | **new packages** `vynkor-wire 0.0.1`, `vynkor-sdk 0.0.1` (names verified free); old crates frozen forever + final deprecation notice patch each |
| PyPI | `vynkor-sdk` (module `vynkor`) | `vynkor-sdk 0.0.1`, module `vynkor` |
| Kernel crate | lib name `vynkor` (unpublished) | `vynkor`; binary stays `vyn` |
| Manager | already `vynkor-manager` ✓ | dep swap to `vynkor-wire` |
| Env vars | `VYNKOR_*` | `VYN_*` / `VYNM_*` hard cutover |
| Paths/socket | `vynkor.sock`, `share/vynkor`, `lib/vynkor/plugins` | `vyn.sock`, `share/vyn`, `lib/vyn/plugins` (§1) |
| Registry artifacts | GitHub raw URLs | **Cloudflare R2** behind `cdn.vynkor.dev` |

### Phases (every phase leaves all repos green)

- **Phase A — code & packages rename:** wire 0.0.1 → sdk-rust + pypi 0.0.1 →
  kernel crate/env/paths hard cutover → manager dep swap → GitHub renames
  LAST (redirects cover everything meanwhile).
- **Phase B — registry on R2:** domain + bucket + `cdn.vynkor.dev`;
  `package.sh` gains upload step AND the S1 seven-field signature
  (absorbs vynkor-plugins Sequencing #4); all plugins re-signed as **0.0.1**
  against the new URLs (maintainer key required). `registry.json` STAYS IN
  GIT — PR review + revocation history; R2 hosts archives only.
- **Phase C — installer & docs sweep:** `install.sh` targets
  `~/.config/vyn/` + `https://vynkor.dev/install.sh`; docs/tracker closure;
  AUR PKGBUILD (V-19) lands on the renamed world.

### Stage 4/A wave 1 — verified complete (2026-08-22, independent check)

crates.io artifacts live (`vynkor-wire 0.0.1`, `vynkor-sdk 0.0.1`,
deprecation `vynkor-wire 0.2.7`); GitHub repos renamed (`vynkor-wire`,
`vynkor-sdk`); all four PRs merged; gates re-run independently green in all
four repos (kernel 430 / manager 92 / wire 26 / sdk 35).

**Loose ends & resolutions:**
1. `vynkor-sdk` never got its deprecation notice patch (§8 said "each") →
   scheduled into **wave 2**: publish `0.1.7` from a short-lived branch with
   a renamed-to-vynkor-sdk banner; do NOT merge that banner into `vynkor-sdk`
   main (it is meaningless on the new crate). Same accepted pattern as the
   wire `0.2.7` banner, whose unmerged branch is closed as-is.
2. Public API polish moved to wave 2: exactly one prefixed public type
   exists — `VynkorClient` → becomes `VynkorClient`; tree-wide doc/comment
   sweep of ~170 residual "vynkor" mentions (EXCLUDING the generated
   `proto::vynkor` module path — protobuf package rename requires a
   coordinated change across kernel/wire/cpp/python copies and stays a
   separate milestone).
3. Republish: public types change vs the published `0.0.1` ⇒ wave 2 ships
   **`vynkor-sdk 0.0.2`** so crates.io matches main.
4. Local checkout dir `vynkor-sdk-rust` → `vynkor-sdk` (matches GitHub).
5. **C++/Python SDKs explicitly DEFERRED** to after Phase B's plugin
   re-release wave: they vendor the proto and read socket/JWT env strings;
   when touched, they flip to `VYN_SOCKET_PATH` / `VYN_JWT_*` in one pass.

### Prerequisites (owner-side)

1. Buy `vynkor.dev` (~$10–15/yr; `.dev` is HSTS-preloaded — fine, served via
   Cloudflare TLS). Short-term fallback: `vynkor.vynkor.online`.
2. Cloudflare account + R2 bucket + custom-domain binding.
3. Signing key (`VYN_SIGNING_KEY_HEX`) available for the re-sign pass.
