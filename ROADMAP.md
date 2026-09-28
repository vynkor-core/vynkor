# Vynkor ROADMAP — Phase 15+

**Baseline:** 2026-09-25 · Kernel `0.1.3`
**Branch:** `develop`
**Previous phases:** `docs/archive/` (Phase 1–2: `ROADMAP_phase1.md`/`ROADMAP_v2.md`/`ROADMAP_v3.md` · Phase 3–4: `ROADMAP_v4.md` · Phase 5: `ROADMAP_v5.md` · Phase 6: `ROADMAP_v6.md` · Phase 7: `ROADMAP_v7.md` · Phases 8–14 (permission sync, hard isolation, marketplace state, proto v1.4/v1.5, remote-devices foundation, kernel audits): `ROADMAP_v8.md`)

**Other live trackers:** `docs/REMOTE_DEVICES_ROADMAP.md` (D-15…D-20 open) ·
`docs/VYNM_ROADMAP.md` (V-19 partial) · `vynkor-plugins/ROADMAP.md`.

---

## Manifesto (non-negotiable)

- Kernel = dumb byte router + process supervisor. Zero business logic. Zero AI. Zero databases for application state (the event-delivery outbox is the explicit exception, see DC-5/F6).
- Intra-host IPC = UDS only. No TCP, no Redis, no queues.
- Protocol = single `.proto` file. Changes propagate to all SDKs.
- Plugin = isolated OS process. Cannot bypass kernel. Speaks only UDS.
- External access = WebSocket/HTTP gateway only (Axum).

---

## Distribution — REL-01 (2026-09-28)

- [x] **REL-01 — tag-driven binary releases + installer.** Kernel `v0.1.3`
  and vynm `v0.1.0` are the first tagged releases: static musl archives
  (x86_64/aarch64), `SHA256SUMS`, provenance attestations, `install.sh` as a
  release asset (#103, #105; vynkor-manager #37, #38). Runbook and lessons:
  `docs/RELEASING.md`. Process: `docs/ENGINEERING_WORKFLOW.md`.
- [ ] Bump release actions off Node 20 (upload/download-artifact,
  attest-build-provenance) — exercise publish with an `-rc` tag.
- [ ] AUR `PKGBUILD` from the release archives (V-19.3).
- [ ] **Scheduled installer smoke test** — weekly workflow: clean
  `ubuntu:24.04` → public one-liner → `vyn start` → `vyn status` →
  `vynm search`. Catches a broken latest release, registry or installer
  between tags (today this is only checked by hand, `docs/RELEASING.md` §5).
- [ ] **macOS decision** — sandbox is Linux-only; ship a no-sandbox `vyn`
  for macOS or keep "build from source"? Decide before adding a target.
- [ ] **Curated release notes / CHANGELOG** — releases use
  `--generate-notes` (PR titles only); only vynkor-wire keeps a CHANGELOG.

## Ecosystem backlog (recorded 2026-09-28)

Open work outside this repo's phases, so it is not lost between sessions.
Owner repo in bold.

**Distribution**
- [ ] **vynkor-manager**: bump `vynkor-wire` 0.0.2 → 0.0.4 (kernel is on
  proto 1.7 / wire 0.0.4; manager still parses manifests with the old crate).
- [ ] **vynkor-manager**: publish 0.1.x to crates.io (README no longer
  promises it; `cargo install --git` is the documented fallback).
- [ ] **vynkor-sdk / vynkor-sdk-python**: write down the release procedure
  (none exists; wire has one in its README), ideally tag-driven
  (`cargo publish` / PyPI trusted publishing).
- [ ] **vynkor-client-android**: signed APK in GitHub Releases + F-Droid
  metadata — tracked as D-16 in `docs/REMOTE_DEVICES_ROADMAP.md`.

**Product (plugins; kernel: none)** — CD-00, CD-03 (token streaming +
cancel), CD-04 (assistant session), CD-09 (quotas) in Phase 15 above. These
make the phone/AI demo feel alive; do them before announcing.

**Web (vynkor-web)** — deploy checklist (domain, D1, GitHub OAuth app,
privacy/terms), /docs pages, og-image, ratings on cards, permissions from the
registry, review abuse controls, publish pipeline. Full list:
`../vynkor-web/ROADMAP.md`.

**Launch**
- [ ] 60-second demo video/GIF: install → `vyn start` → pair phone (QR) →
  ask the AI something answered via `my-phone.*` — for README, site, posts.
- [ ] Announcement (HN / Reddit / Rust forums) — only after the site is
  deployed and the demo exists.

**Community hygiene**
- [ ] `CONTRIBUTING.md` in the other repos — only the kernel has one on its
  default branch (vynkor-plugins has one on `main`, not on `develop`). A short
  file per repo linking `docs/ENGINEERING_WORKFLOW.md` is enough.
- [ ] Issue templates missing in vynkor-sdk-cpp, vynkor-sdk-python,
  vynkor-web, vynkor-client-android (present in the other five).
- [ ] PR template (`.github/pull_request_template.md`) with the
  Why / What / Verification / Not verified sections from
  `docs/ENGINEERING_WORKFLOW.md` §3 — no repo has one.

**Correctness / security (before announcing)** — recorded 2026-09-28
- [x] **vynkor**: revocation reaches live sessions — an open device WS
  re-checks its row every `ws_device_recheck_secs` and closes on
  revoke/remove/expiry (#107). The ed25519 half of D-18 stays open.
- [ ] **vynkor**: duplicate action names across plugins — `stt`, `tts` and
  `speech` declare the same actions; registry behavior is unverified. Decide
  (reject at registration vs. last-wins), add a test; mark `stt`/`tts`
  deprecated in the registry.
- [ ] **vynkor-plugins**: `speech` not verified under `sandbox: true`;
  `email` runs `sandbox: false`.

**Testing across repos**
- [x] **vynkor-client-android**: CI (rust core + JVM unit tests + debug APK),
  vynkor-client-android #7.
- [ ] **vynkor-wire**: golden frames (bytes + expected decode) shared by all
  three SDKs' CI — checks codec behavior, not just byte-identical `.proto`.
- [ ] End-to-end: released kernel → `vynm install` → plugin → action call
  (extend the scheduled installer smoke test).

**Plugin developer experience**
- [ ] `vynm new <lang>` / `cargo generate` template (plugin.json, CI, test).
- [ ] SDK ↔ kernel protocol compatibility matrix in one place.

## Carried over — Phase 14

- [ ] **K-06 — Decouple API drain window from `default_grace_seconds`.**
  K-04 shipped the ordered shutdown reusing `default_grace_seconds` as the
  Axum `Handle::graceful_shutdown` drain bound (same budget as plugin
  teardown). Untested assumption: HTTP/WS drain and plugin-kill grace may
  need different tuning in practice (e.g. long-lived WS sessions want a
  longer drain than a hung plugin should get before SIGKILL).
  - Files: `src/kernel/orchestrator/{mod,shutdown}.rs`, `config.yaml`,
    `src/utils/config.rs`.
  - Fix: add optional `api_grace_seconds` config key, default to
    `default_grace_seconds` when unset (no behavior change out of the box).
  - Acceptance: unit test — setting `api_grace_seconds` distinct from
    `default_grace_seconds` changes only the API drain timeout, not the
    plugin grace window.
  - Not scheduled — raise before picking up, low urgency until a real
    workload needs the split.

## Phase 15 — Client-driven tasks (CD) (2026-09-24)

Specs: `docs/tasks/CD-*.md`, shipped ones in `docs/archive/tasks/` (source:
`CLIENT_DRIVEN_KERNEL_TASKS.md`). Decisions recorded 2026-09-24; each spec
carries a "Decision" + "Status" block. Dumb core holds: anything that knows an
action name (`chat_completion`, stt/tts, quotas) lives in `vynkor-plugins`.

| Item | Decision (short) | Where the work lives |
|------|------------------|----------------------|
| CD-00 | `ai` `list_models`/`list_agents` + `plugin.json` output schema is the contract; no proto change | vynkor-plugins (`ai`) |
| CD-01 | HTTP `POST /devices/pair` + unauth rate-limited `POST /devices/consume`; hashed `tickets.json`; WS untouched | kernel |
| CD-02 | DONE; symmetric per-device secret (Ed25519 → future v3) | kernel + SDKs + client |
| CD-03 | reuse `ActionResponseChunk` streaming; `ChatDelta` dropped | vynkor-plugins (`ai`, `network`) |
| CD-04 | owner = `agent` plugin; downlink via `{device_id}.*` actions | vynkor-plugins (`agent`, `stt`) + client |
| CD-05 | recipient = provider device; router already stamps `caller_plugin_id` | kernel (regression test) + client |
| CD-06 | min 1.5, major.minor range check; ack fields deferred to next wire | kernel |
| CD-07 | "device offline" message + fail in-flight on provider disconnect | kernel |
| CD-08 | `docs/TLS.md`, README, SHA-256 fingerprint in pair output + `vyn tls status` | kernel |
| CD-09 | per-caller quota inside `ai`, keyed by `caller_plugin_id` | vynkor-plugins (`ai`) |

- [ ] CD-00 — strip `api_key_env` from `list_models` output, optional
      `display_name`, contract test (**vynkor-plugins**; kernel: none).
- [x] CD-01 — pairing ticket (**kernel**). SHIPPED 2026-09-25 (d677a02):
      `POST /devices/pair` + `/devices/consume`, `vyn device pair`.
- [x] CD-02 — per-device keys. **DONE** 2026-09-25 across kernel, SDKs and
      client: `connect_ws_device` + `VYN_DEVICE_ID`/`VYN_DEVICE_SECRET` with
      one strict env policy in Rust (vynkor-sdk #16, **0.0.4** on crates.io),
      C++ (vynkor-sdk-cpp #9) and Python (vynkor-sdk-python #10); Android
      client already on `deviceSecret`.
- [ ] CD-03 — token streaming + cancel (**vynkor-plugins**; kernel: none).
- [ ] CD-04 — assistant session in `agent`; stt partial transcripts
      (**vynkor-plugins** + client; kernel: none).
- [x] CD-05 — `caller_plugin_id` stamping regression test for device targets
      (**kernel**, 19ef1c9). Remaining: client-side audit log (android).
- [x] CD-06 — protocol range `[1.5, PROTOCOL_VERSION]` (**kernel**, 272e48b).
      `PluginRegisterAck` negotiated/min fields deferred to next wire release.
- [x] CD-07 — offline message + fail in-flight on disconnect (**kernel**,
      7e0a4a1).
- [x] CD-08 — TLS docs + fingerprint (**kernel**, 30dc367): `docs/TLS.md`,
      `vyn tls status`; explicit `--host wss://` survives `tls: false`.
- [ ] CD-09 — per-caller `chat_completion` quota (**vynkor-plugins**;
      kernel: none — existing generic `action_caller_*` limits stay).

## Definition of Done

- `cargo test --all --all-features` exits 0; new behavior has regression tests.
- `cargo clippy --all-targets --all-features -- -D warnings` clean; `cargo fmt --check` clean.
- C++: existing CMake test targets stay green; new tests follow the
  `sdk/cpp/tests/test_*.cpp` naming/registration pattern in `CMakeLists.txt`.
- Python: new tests follow the `tests/test_*.py` pattern in the
  `vynkor-sdk-python` repo (unit tests live in the SDK, not the kernel;
  kernel-side cross-SDK integration tests stay in `tests/integration/`).
- Docs updated in the same PR (README for operator-visible changes; no
  `docs/FRAMING.md` changes expected since the wire format doesn't change).
