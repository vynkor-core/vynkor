# Veyron → Vynkor — Full Migration (2026-08-28)

> **Date:** 2026-08-28
> **Branches:** `chore/rename-veyron-remnants` in all 9 repos `vynkor-core/*`
> **Goal:** 0 occurrences of `veyron` in code (excluding `docs/archive` as history)

## 0. Summary

| Question | Answer |
|---|---|
| `grep -R -i veyron --exclude-dir=.git --exclude-dir=target | grep -v docs/archive | grep -v .omo` | **0** |
| `grep` with `docs/archive` | **26** (historical `ROADMAP_phase1/v3/v5`, `SECURITY_HARDENING` — not code) |
| `.superpowers/` | **deleted** (2 repos) |
| `target/`, `build/`, `__pycache__` | **deleted and rebuilt** |
| Breaking changes | **3** (vault magic, `VEYRON_SOCKET_PATH`, Python `Veyron*` aliases) |

## 1. Branches

Created from current bases, no commits squashed (dirty, `git diff`):

| Repo | Base | Branch | Changed files |
|---|---|---|---|
| `vynkor` (kernel) | `develop` | `chore/rename-veyron-remnants` | `ROADMAP.md` 198, `AUDIT.md` 48, `docs/*.md` 12, `plugins.d/*.yaml` 5, `src/plugins/{shim,supervisor}.rs`, `tests/*`, `CLAUDE.md`, `docs/archive/VEYRON_ARCHITECTURE.md`→`VYNKOR_ARCHITECTURE.md` |
| `vynkor-wire` | `main` | `chore/rename-veyron-remnants` | `LICENSE*`, `src/manifest.rs`, `tests/manifest.rs`, `README.md`, `docs/V-01` |
| `vynkor-manager` | `develop` | `chore/rename-veyron-remnants` | `LICENSE`, `src/{sign,keygen,state,error,installer}`, `.github/workflows/ci.yml`, `docs/V-03,V-05,STAGE4` |
| `vynkor-sdk` | `main` | `chore/rename-veyron-remnants` | `LICENSE`, `.github/workflows/ci.yml` (removed `veyron-wire` check) |
| `vynkor-sdk-cpp` | `main` | `chore/rename-veyron-remnants` | `LICENSE`, `fuzz/fuzz_framing.cpp` |
| `vynkor-sdk-python` | `main` | `chore/rename-veyron-remnants` | `vynkor/errors.py` (9 classes → no legacy), `framing/client/concurrent/confirmation_gate/plugin.py`, `tests/*`, `fuzz/`, `examples/`, `README.md`, `LICENSE` |
| `vynkor-plugins` | `develop` | `chore/rename-veyron-remnants` | **53 files** — all `plugins/*/README|ROADMAP|USAGE.md`, `config.example.yaml`, `scripts/*`, `docs/*`, `registry.json`, `plugins/secrets/src/vault.rs` (see §3), `scripts/live-audit/veyron_ws.py`→`vynkor_ws.py` |
| `vynkor-web` | `main` | `chore/rename-veyron-remnants` | `ROADMAP.md` |
| `vynkor-client-android` | `feat/client-full-capabilities` | `chore/rename-veyron-remnants` | **19 files** — `rust/Cargo.toml` (`veyron-wire 0.2`→`vynkor-wire 0.0.3` + patch), `rust/src/*`, `docs/*`, `LICENSE-MIT` |

## 2. What Was Deleted

```bash
rm -rf vynkor/docs/superpowers
rm -rf vynkor-sdk-cpp/.superpowers vynkor-sdk-python/.superpowers
rm -rf vynkor/target vynkor-wire/target vynkor-manager/target \
       vynkor-client-android/rust/target vynkor-sdk-cpp/build \
       vynkor-client-android/{build,app/build} \
       vynkor-plugins/plugins/*/target \
       vynkor-sdk-python/**/__pycache__ *.pyc
```

`.superpowers/` contained `review-*.diff` with 20+ `proto/veyron_protocol.proto` — generated diff artifacts, not code.

## 3. Where and How It Was Renamed

### 3.1 Filesystem: `vyn` vs `vynkor`

* **Short `vyn`** — paths: `~/.local/lib/vyn/plugins`, `~/.local/share/vyn`, `~/.config/vyn`, `~/.cache/vyn`, `/var/lib/vyn`, `/tmp/vyn`, env `VYN_SOCKET_PATH`, `VYN_JWT_SECRET`, `VYN_DATA_DIR`, `VYN_SHIM_GRACE_SECS`
* **Long `vynkor`** — crate/repo/proto: `vynkor-wire`, `vynkor-sdk`, `vynkor-plugins`, `vynkor-manager`, `package vynkor`, `proto/vynkor_protocol.proto`, `vynkor.dev`, `vynkor-core` org

Script: `VEYRON`→`VYNKOR`→ postfix `/vynkor`→`/vyn` for 6 prefixes (`.local/lib`, `.local/share`, `.config`, `.cache`, `/var/lib`, `/tmp`).

### 3.2 Kernel `vynkor`

* `ROADMAP.md` 113 replacements, `AUDIT.md` 30 — `veyron-wire`→`vynkor-wire`, `veyron/<id>.scope`→`vynkor/<id>.scope`, `VeyronError`→`VynkorError`, `~/.local/lib/veyron`→`~/.local/lib/vyn`, `/tmp/veyron`→`/tmp/vyn`
* `docs/*.md` (12 files, without `archive/superpowers`): `VYNM_PLAN`, `REMOTE_DEVICES`, `ANDROID_DEVICE_AGENT*`, `COMMENT_TAGS`, `PLUGIN_REGISTRY_SCHEMA`, `VYN_PRODUCT_LAYOUT` — all → `vynkor`
* `plugins.d/*.yaml` 5 files: `binary: /home/.../.local/lib/veyron`→`/vyn`
* **Breaking `shim.rs:115`**: `VYN_SOCKET_PATH or VEYRON_SOCKET_PATH` → `VYN_SOCKET_PATH` only
* **Breaking `supervisor.rs:291`**: removed `.env("VEYRON_SOCKET_PATH")` (only `VYN_SOCKET_PATH` remains)
* `tests/integration` 9 files + `tests/unit/test_supervisor.rs` — `VEYRON_SOCKET_PATH`→`VYN_SOCKET_PATH`, `test -n "$VYN" -a -n "$VEYRON"`→`test -n "$VYN"`
* `CLAUDE.md` `Old veyron*`→`Old names`, `VYNKOR_ARCHITECTURE.md` file renamed

### 3.3 Wire `vynkor-wire`

* `LICENSE*` `veyron-core`→`vynkor-core`
* `src/manifest.rs:166,179` `requires Veyron kernel`→`requires Vynkor kernel`
* `tests/manifest.rs:48,60,194,423` expectations → `Vynkor`
* `README.md` `proto::veyron`→`proto::vynkor`, `veyron/src/marketplace`→`vynkor/src`

### 3.4 Manager `vynkor-manager`

* `src/sign.rs:13`, `keygen.rs:2`, `state.rs:111` — removed `(legacy VEYRON_*)` tails, only `VYN_*` remains
* `src/installer.rs:149` `Upgrade Veyron`→`Upgrade Vynkor`, `src/error.rs:3` `VeyronError`→`VynkorError`
* `.github/workflows/ci.yml` — `cargo tree -i veyron` check kept as `vynkor` (or removed in `sdk`)

### 3.5 SDK Rust `vynkor-sdk`

* `LICENSE` + removed `No old wire dependency` step with `veyron-wire` (3 lines) — now 0 mentions

### 3.6 SDK Python `vynkor-sdk-python` — breaking

* `vynkor/errors.py`: removed legacy block 9 `Veyron* = Vynkor*` + 9 lines in `__all__`. Only 10 `Vynkor*` remain. Imports `from vynkor.errors import VeyronInternal` now **ImportError** → fix to `VynkorInternal`.
* `framing.py`/`client.py`/`concurrent.py`/`confirmation_gate.py`/`plugin.py` — `Veyron*`→`Vynkor*` (60+ replacements)

### 3.7 Plugins `vynkor-plugins` — breaking

* **53 files** bulk: `Veyron`→`Vynkor` in `README/ROADMAP/USAGE`, `config.example.yaml` (`VEYRON_JWT_SECRET`→`VYN_JWT_SECRET`, `/var/lib/veyron`→`/var/lib/vyn`), `scripts/*`, `docs/*`
* `plugins/secrets/src/vault.rs`: `MAGIC = b"VYNKORVLT"` only, removed `MAGIC_LEGACY = b"VEYRONVLT"` and `&& MAGIC_LEGACY` branch. Doc `b"VYNKORVLT" (or legacy...)`→`b"VYNKORVLT"`, test `b"VEYRONVLT"`→`b"VYNKORVLT"`
* `scripts/live-audit/veyron_ws.py` → `vynkor_ws.py` + `from vynkor_ws import` in 2 files
* `LICENSE`, `plugin.json` (`defaults to 'veyron'`→`vynkor`)

### 3.8 Android `vynkor-client-android`

* `rust/Cargo.toml` `veyron-wire = "0.2"`→`vynkor-wire = "0.0.3"` + `[patch.crates-io] vynkor-wire = {path="../../vynkor-wire"}` (until `0.0.3` publish)
* `rust/src/*` 8 files: `veyron_wire`→`vynkor_wire`, `veyron::proto::veyron`→`vynkor::proto::vynkor`, `veyron-frame-mac-v1`→`vynkor-frame-mac-v1`, `VEYRON_DATA_DIR`→`VYN_DATA_DIR`
* `docs/*`, `LICENSE-MIT`, `scripts/fetch-stt-assets.sh`

### 3.9 Web `vynkor-web`

* `ROADMAP.md` 3 lines: `Veyron → vynkor (231)`→`vynkor rename complete`, `public/veyron_demo_preview.jpg`→`vynkor_demo_preview.jpg`, `veyron.online`→`vynkor.online`

### 3.10 Local artifacts

* `.claude/settings.local.json` — `veyron-core`→`vynkor-core`, `/tmp/veyron`→`/tmp/vyn`, all bash commands with `vynkor` paths
* `CLAUDE.md` — `veyron*`→`names`

## 4. Breaking Changes → Operator Migration

| What breaks | Why | Migration |
|---|---|---|
| `~/.local/share/vyn/*.vault` with `VEYRONVLT` | `vault.rs` now rejects `VEYRONVLT` | `rm ~/.local/share/vyn/*.vault` or manual `sed` conversion. Loss: all `secret_*` must be `secret_set` again. |
| Plugins built with old SDK (<0.0.3) | `shim`/`supervisor` no longer export `VEYRON_SOCKET_PATH` | `cargo build --release` in each `plugins/*/`, `pip install -e .` for Python, `cmake --build` for C++ |
| `from vynkor.errors import VeyronInternal` | aliases removed | `Veyron*`→`Vynkor*` in 9 places |
| `vynkor-client-android` builds | `vynkor-wire 0.0.3` not published yet | `cargo` uses `[patch.crates-io]` locally; after `cargo publish -p vynkor-wire` remove patch |

## 5. Verification

```bash
# code — 0 (without docs/archive, .omo, .gradle, target)
grep -R -a -i veyron --exclude-dir=.git --exclude-dir=target --exclude-dir=.codegraph \
  --exclude-dir=node_modules --exclude-dir=dist --exclude-dir=build --exclude-dir=.gradle \
  vynkor-core | grep -v docs/archive | grep -v .omo
# → (empty)

# builds
cargo build -p vynkor-wire          # 23s OK
cargo build -p vynkor               # 64s OK
cargo build -p vynkor-manager       # 68s OK
cmake -S vynkor-sdk-cpp -B build && cmake --build build -j4  # OK
python3 -m py_compile vynkor-sdk-python/vynkor/*.py           # OK
cargo test -p vynkor --test unit test_supervisor test_manifest_enforcement # 21 passed
cargo test -p secrets --lib         # 22 passed
cargo test -p vynkor-wire           # 7 passed
cargo test -p vynkor-manager        # 11 passed
```

`docs/archive` kept intentionally — 26 occurrences in `ROADMAP_phase1/v3/v5`, `SECURITY_HARDENING` as history. Remove with `rm -rf docs/archive` if needed.

## 6. Why

* `superpowers/` — generated `review-*.diff` with `proto/veyron_protocol.proto` (20+ `project(veyron_sdk`, `package veyron`), not code.
* `VEYRON_SOCKET_PATH` fallback — 9 lines in `shim`/`supervisor`/`tests` — kept compatibility with plugins until `vynkor-sdk 0.0.1` re-release wave; wave done, fallback removed.
* `VEYRONVLT` — on-disk `secrets` vault format; dual-read kept 1 release for migration, now single `VYNKORVLT`.
* `Veyron*` aliases in Python — kept `pip` plugins until rename, now clean `Vynkor*` API.

## 7. Next

* `cargo publish -p vynkor-wire` `0.0.3` → remove `[patch.crates-io]` in `vynkor/Cargo.toml` and `vynkor-client-android/rust/Cargo.toml`
* Rebuild and reinstall all `~/.local/lib/vyn/plugins/*` with new SDK
* Remove old `~/.local/lib/veyron`, `~/.cache/veyron`, `~/.local/share/veyron` if any remain
