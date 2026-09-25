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

Specs: `docs/tasks/CD-*.md` (source:
`CLIENT_DRIVEN_KERNEL_TASKS.md`). Decisions recorded 2026-09-24; each spec
carries a "Decision" + "Status" block. Dumb core holds: anything that knows an
action name (`chat_completion`, stt/tts, quotas) lives in `vynkor-plugins`.

| Item | Decision (short) | Where the work lives |
|------|------------------|----------------------|
| CD-00 | `ai` `list_models`/`list_agents` + `plugin.json` output schema is the contract; no proto change | vynkor-plugins (`ai`) |
| CD-01 | HTTP `POST /devices/pair` + unauth rate-limited `POST /devices/consume`; hashed `tickets.json`; WS untouched | kernel |
| CD-02 | kernel DONE; symmetric per-device secret (Ed25519 → future v3) | SDKs + client |
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
- [x] CD-02 — per-device keys, **kernel side**. Remaining: sdk-cpp
      `resolve_jwt_secret` → device-secret naming; Rust/Python SDK
      `device_secret`; client `HostProfile.jwtSecret` → `deviceSecret`.
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
