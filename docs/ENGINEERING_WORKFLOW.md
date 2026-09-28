# Engineering workflow

How work moves from idea to merged, released code across `vynkor-core` —
for humans and coding agents alike. Rules of the codebase itself (dumb core,
proto, comment style) are in `CONTRIBUTING.md` and `CLAUDE.md`; releases are
in `docs/RELEASING.md`. This page is the *process*.

---

## 1. One change = one branch = one PR

- Branch from the repo's default branch — `develop`: vynkor, vynkor-manager,
  vynkor-plugins, vynkor-wire; `main`: vynkor-web, vynkor-sdk,
  vynkor-sdk-cpp, vynkor-sdk-python, vynkor-client-android. Never commit to
  it directly.
- Names: `feat/…`, `fix/…`, `docs/…`, `ci/…`, `chore/…` — same prefix as the
  commit type.
- A change that spans repos is one PR **per repo**, each linking the others
  ("Companion PRs: …"). Merge in dependency order (§5).
- Keep PRs reviewable: a site bug-fix and a new backend are two PRs even if
  written the same day. Stack them when one needs the other (§5.3).
- Several independent branches in one repo at once: use `git worktree add`
  instead of stashing (`../.wt-<topic>`), remove it after the push.

## 2. Commits

Conventional commits, body explains **why** — the diff already shows what.

```
fix(release): build on aarch64-musl; add a dry-run trigger

The v0.1.3 release failed on aarch64-unknown-linux-musl: libc's musl
aarch64 syscall table has no SYS_kexec_file_load, so … Define it locally …
```

- Subject ≤ ~70 chars, imperative, scope = area (`ipc`, `supervisor`,
  `release`, `site`, `marketplace`).
- Say what was verified and how in the body when it is not obvious from tests.
- Agent-authored commits end with the `Co-Authored-By:` trailer.

## 3. PR description template

```markdown
## Why
The user-visible problem or the gap. Concrete: what fails, for whom.

## What
Bullets per file/area. Call out decisions and deviations from a plan doc
(and update that doc in the same PR).

## Verification
What was actually run, with results. Links to CI/dry-run runs.
- **Not verified:** what could not be checked here and why
  (no arm runner, no OAuth app, needs a deploy). Never omit this line
  to look cleaner — it is what the reviewer needs most.

## After merge
Ordered steps if merging is not the end (tags, deploys, companion PRs).
```

## 4. Verification bar — evidence before claims

"Done" means someone ran it. Pick the strongest check the change allows:

| Change | Minimum evidence |
|---|---|
| Rust (any repo) | `cargo test --all --all-features`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo fmt --check` |
| Release workflow / toolchain / C deps / `Cargo.lock` | `gh workflow run release.yml --ref <branch>` dry run, both targets green |
| Shell scripts | `shellcheck`, `bash -n`, and an end-to-end run in a clean `ubuntu:24.04` container — including the failure path (tampered checksum, bad args) |
| Worker / API | vitest inside workerd against migrated D1 (`worker/`), plus a local `wrangler dev` run with the real `registry.json` |
| UI | typecheck + build, then drive it in a real browser (headless Chrome via `playwright-core`); screenshot the states you changed; zero console errors |
| Security checks (auth, CSRF, signatures, sandbox) | **mutation check**: disable the check, confirm a test fails, restore. A security test that still passes with the check removed is not a test |

Also:

- Check every external link a page adds (`curl -sL -o /dev/null -w '%{http_code}'`).
- Numbers in copy (binary size, versions) are measured, not remembered.
- If you could not verify something, say so in the PR (§3) — do not round
  "probably works" up to "works".

## 5. Merging

### 5.1 Style per repo (keep history consistent)

| Repo | Method |
|---|---|
| vynkor | merge commit (`gh pr merge N --merge --delete-branch`) |
| vynkor-manager, vynkor-web, SDKs | squash (`--squash`), subject gets `(#N)` |

vynkor refuses a merge when the branch is behind `develop` ("head branch is
not up to date" — an org ruleset): `gh pr update-branch N`, wait for CI
again, then merge. Auto-merge is off in every repo — wait for green checks,
then merge by hand.

### 5.2 Order across repos

Dependency first: `vynkor-wire` → SDKs → `vynkor-manager` → `vynkor` →
`vynkor-plugins` → `vynkor-web`. For releases: vynm before kernel
(`docs/RELEASING.md`).

### 5.3 Stacked PRs

Open the second PR with `--base <first-branch>`. When the first is
**squash**-merged, the second still carries its original commits:

```bash
git fetch origin
git rebase --onto origin/main <first-branch> <second-branch>
git push --force-with-lease origin <second-branch>
gh pr edit <N> --base main
git push origin --delete <first-branch>
```

Changing a PR's base does not fire `pull_request` CI — close and reopen the
PR (or push) to get checks on the new base.

## 6. Security posture for every change

- **Nothing fake on public surfaces.** No invented reviews, users, prices,
  download counts or "verified" badges, even as placeholders — a visitor
  cannot tell a mock from a claim. Hide the feature until it is real.
- **Executable content comes from tags.** Anything users pipe into a shell
  or install resolves to a release asset, never a branch head.
- **Verify, then install.** Downloads are checked against published
  checksums/signatures before use; a mismatch aborts with nothing written.
- Secrets never in repo files; `.dev.vars`, `wrangler secret`, GitHub
  secrets. Store hashes of session tokens, not tokens.
- Workflows: interpolate only trusted values (`matrix.*`, fixed strings)
  into `run:`; pass refs and user-controlled data through `env:`.
- A background security review finding is handled before moving on:
  fix it or explain in the PR why it does not apply.

## 7. Docs move with the code

- Operator-visible behavior → README in the same PR.
- A roadmap item started or finished → its tracker (`ROADMAP.md`,
  `docs/*_ROADMAP.md`, `vynkor-web/ROADMAP.md`) in the same PR, with the date.
- Deviating from a plan doc (e.g. `MARKETPLACE_PLAN.md`) → record the
  deviation and the reason in that doc.
- A process lesson (a release broke, a CI trap) → `docs/RELEASING.md`
  "Lessons" or this page.
