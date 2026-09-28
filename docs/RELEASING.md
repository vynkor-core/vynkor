# Releasing vynkor

How binaries reach users, for every repo in `vynkor-core`. One page, kept
current — update it in the same PR that changes a release workflow.

**Established:** 2026-09-28 (REL-01, first tagged releases: kernel `v0.1.3`,
vynm `v0.1.0`).

---

## What ships where

| Artifact | Repo | Trigger | Output |
|---|---|---|---|
| `vyn`, `vyn-pair` | vynkor | tag `vX.Y.Z` | GitHub Release: `vyn-<target>.tar.gz`, `install.sh`, `SHA256SUMS` + provenance attestations |
| `vynm` | vynkor-manager | tag `vX.Y.Z` | GitHub Release: `vynm-<target>.tar.gz`, `SHA256SUMS` + attestations |
| plugins | vynkor-plugins | tag `<slug>-vX.Y.Z` | signed zip → `dist/` + `registry.json` commit + GitHub Release (INF-01, see that repo's `release.yml`) |
| `vynkor-wire` | vynkor-wire | manual `cargo publish` | crates.io — steps in its README, "Releasing a new version" |
| `vynkor-sdk` (Rust) | vynkor-sdk | manual `cargo publish` | crates.io — no written procedure yet |
| `vynkor-sdk` (Python) | vynkor-sdk-python | manual | PyPI — no written procedure yet |
| website + `/api` | vynkor-web | `npm run deploy` in `worker/` (not live yet) | Cloudflare Worker + D1 |

Targets for both binaries: `x86_64-unknown-linux-musl`,
`aarch64-unknown-linux-musl` — static, stripped, built on native GitHub
runners (`ubuntu-latest`, `ubuntu-24.04-arm`), no cross/qemu.

Asset names carry **no version** on purpose:
`https://github.com/vynkor-core/<repo>/releases/latest/download/<asset>`
then always resolves without an API call (no rate limit for `install.sh`).

## The installer

`install.sh` lives in the kernel repo root and is **published as a release
asset**. Every public entry point resolves to that asset, never to a branch:

- README / site: `curl -fsSL https://github.com/vynkor-core/vynkor/releases/latest/download/install.sh | bash`
- `https://vynkor.dev/install.sh` → 302 to the same URL (vynkor-web worker +
  `public/_redirects`)

Why: `curl | bash` must only execute tagged, reviewed code. Pointing it at
`develop` would ship every push straight into users' shells.

The script: detects arch → downloads kernel + vynm archives → verifies each
against its release's `SHA256SUMS` (refuses to install on mismatch or missing
entry) → installs into `~/.local/bin` → seeds `~/.config/vyn/config.yaml`
with `vynm init`, then appends `port` + a random `jwt_secret` (mode 600,
never overwrites). No sudo, no self-update: re-running is the update. All
logic sits in `main` called on the last line, so a truncated download runs
nothing.

Verify a download by hand:

```bash
sha256sum -c SHA256SUMS --ignore-missing
gh attestation verify vyn-x86_64-unknown-linux-musl.tar.gz -R vynkor-core/vynkor
```

## Cutting a release (kernel or vynm)

Order matters when both change: **vynm first**, then the kernel — the
installer fetches both, and the kernel release is what users hit.

1. **Bump** `version` in `Cargo.toml` on a branch, PR into `develop`, merge.
   The tag must equal this version or `check-version` fails the run.
2. **Dry run** (optional but cheap — do it whenever the workflow, toolchain,
   a C dependency or `Cargo.lock` changed):
   ```bash
   gh workflow run release.yml -R vynkor-core/vynkor --ref <branch>
   ```
   Builds and smoke-tests both targets, publishes nothing.
3. **Tag** the merge commit on `develop` and push:
   ```bash
   git checkout develop && git pull --ff-only
   git tag -a v0.1.4 -m "vynkor 0.1.4" && git push origin v0.1.4
   ```
   `vX.Y.Z-rc.N` publishes a **pre-release**, which `/releases/latest` skips —
   use it to test the installer with `--version v0.1.4-rc.1`.
4. **Watch** `gh run watch -R vynkor-core/vynkor`. Jobs: `check-version` →
   `build` ×2 (static check + `--version` smoke test) → `publish`
   (checksums, attestations, `gh release create --generate-notes`).
5. **Bump site copy**: `vynkor-web` `src/components/Header.tsx` badge and the
   `--version` example in `GettingStarted.tsx` (hardcoded for now).
6. **Verify** on a clean machine or container:
   ```bash
   docker run --rm ubuntu:24.04 bash -c 'apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null &&
     curl -fsSL https://github.com/vynkor-core/vynkor/releases/latest/download/install.sh | bash &&
     ~/.local/bin/vyn --version && ~/.local/bin/vynm --version'
   ```

## When a release run fails

- **Failed before `publish`** (nothing on the Releases page): fix on a branch,
  dry-run it, merge, then move the tag to the fixed commit:
  ```bash
  git push origin :refs/tags/v0.1.3 && git tag -d v0.1.3
  git tag -a v0.1.3 -m "vynkor 0.1.3" && git push origin v0.1.3
  ```
  Only acceptable because no one could have downloaded anything.
- **Failed after a release was published**, or a bad binary shipped: never
  move or reuse the tag. Mark the release as pre-release or delete its
  assets (`gh release edit --prerelease`), bump the patch version and cut a
  new tag. `/latest` then moves on.

## Lessons already paid for

| Symptom | Cause | Fix |
|---|---|---|
| `cannot create the lock file … because --locked was passed` | `Cargo.lock` gitignored in vynkor-manager | Binaries commit their lockfile (`fix(release)`, vynkor-manager#38) |
| `unresolved import libc::SYS_kexec_file_load` on aarch64 only | libc's musl/aarch64 syscall table is incomplete | local `const` for that target in `src/plugins/seccomp.rs` (#105). Any new `libc::SYS_*` in the seccomp list must build for **both** musl targets — dry-run it |
| Could not find `protoc` | `vynkor-wire` runs prost-build at compile time | workflow installs `protobuf-compiler`; local cross builds need it too |
| x86_64 job "cancelled" hides its real status | `fail-fast` cancelled it when arm failed | `fail-fast: false` |

## Follow-ups

- `actions/upload-artifact@v4`, `download-artifact@v4`,
  `attest-build-provenance@v2` target Node 20 (runner warns, forces Node 24).
  Bump to current majors and dry-run + rc-tag to exercise the publish job.
- AUR `PKGBUILD` (V-19.3) — should consume the release archives, not rebuild.
- macOS builds: the kernel's sandbox is Linux-only; decide whether a
  no-sandbox macOS `vyn` is worth shipping before adding a target.
- Scheduled installer smoke test (weekly, clean container, public one-liner)
  so a broken `latest` is noticed without waiting for a user report.
- Write release procedures for `vynkor-sdk` (crates.io) and
  `vynkor-sdk-python` (PyPI); publish `vynkor-manager` to crates.io.
- Curated release notes: `--generate-notes` only lists PR titles.
- Site version strings (`vynkor-web` Header badge, GettingStarted example)
  are hardcoded — part of the release checklist until they are generated.

Tracked in `ROADMAP.md` → "Distribution — REL-01" and "Ecosystem backlog".
