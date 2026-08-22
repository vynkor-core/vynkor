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
| Plugin binaries | `~/.local/lib/veyron/plugins/<slug>/` | `~/.local/lib/vyn/plugins/<slug>/` |
| State (ledger, registry cache) | `~/.local/share/veyron/` | `~/.local/share/vyn/` |
| Runtime socket | `$XDG_RUNTIME_DIR/veyron.sock` | `$XDG_RUNTIME_DIR/vyn.sock` — **deferred**, see §6 |
| PID / log files | `~/.veyron/run`, log under data dir | `~/.local/state/vyn/` (XDG state home) |

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
  (`~/.config/vyn/`) — its current draft targets `~/.config/veyron/` and
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
- **Socket rename is NOT free:** the default lives in `veyron-wire/socket.rs`
  and is duplicated in every SDK's connection code — renaming means a
  coordinated wire + 3-SDK release. Recommendation: rename filesystem paths
  now (§1 rows above except socket), keep `veyron.sock` until the full
  rename milestone, then flip wire+SDKs together. Operators who care today
  can set `socket_path:` explicitly in config.

Env-var policy this cycle: keep current names working (`VYNM_STATE_DIR`,
`VYNM_PLUGIN_DIR`, `VEYRON_SOCKET_PATH`, …). Full `VEYRON_*` → `VYN_*` env
rename rides the same future milestone as the socket.

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
