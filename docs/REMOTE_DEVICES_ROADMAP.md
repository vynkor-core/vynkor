# Veyron ROADMAP — Remote Devices (Phase 12)

**Baseline:** 2026-08-14 · Kernel `0.1.0` · Proto `v1.5`
**Branch:** `develop`
**Source:** design + decisions in `docs/REMOTE_DEVICES_PLAN.md`; this file is the task breakdown.
**Previous phases:** Phase 8–11 in `ROADMAP.md`.

> ID prefix `D-` = Device/remote. Related changes are grouped into one task
> (e.g. all additive proto fields are one bump — D-01 — so the six vendored
> copies are synced exactly once).

---

## Non-negotiables (carried from the manifesto)

- **Dumb core stays dumb.** The kernel is a byte router + process supervisor +
  metadata registry + one coarse protocol guard. No business logic, no AI, no
  tool-schema interpretation. Everything "smart" lives in plugins.
- **Additive-only protocol.** Every proto change is additive (`reserved`
  preserved); breaking changes are a separate major version (D-01 / D-03).
- **Local-first is the default.** Remote devices are an additive deployment on
  top of the existing single-machine kernel, never a migration.

## Priority legend

- **P0** — foundation; blocks everything (identity + versioning + discovery).
- **P1** — client kernel + transport (a device can actually connect).
- **P2** — AI integration, voice, sync, companions (product capability).
- **P3** — deferred (explicit non-goals until real demand).

---

## P0 — Foundation (proto, identity, versioning, discovery)

- [x] **D-01 — Proto v1.6: device identity + versioning + `user_id` + tool schema (one additive bump).**
  Single coordinated proto change; re-sync all vendored copies byte-identical
  (P11-02's six copies, R8-05 drift guard) and bump `PROTOCOL_VERSION` in the
  same commit as the header + `Cargo.toml`.
  - `PluginRegister` += `device_id`, `os` (enum), `arch`, `os_version`,
    `capabilities[]`, `protocol_version` (semver string), `user_id`.
  - `PluginManifest` += `platforms[]` (install filtering) and `action_specs[]`
    (`ActionSpec { name, description, params_schema (JSON Schema), risk,
    requires_confirmation }`); keep `actions[]` for the router.
  - New `DeviceInfo { device_id, os, arch, os_version, capabilities,
    last_seen, state }`.
  - Files: `../veyron-wire/proto/veyron_protocol.proto`, `veyron_wire::PROTOCOL_VERSION`,
    vendored copies, `tests/unit/test_proto_sync.rs`.
  - Acceptance: regen compiles; six copies byte-identical; drift test green;
    `PROTOCOL_VERSION`/header/`Cargo.toml` bumped in one commit.
  - **Status (2026-08-14): SHIPPED** — proto v1.6 landed in `veyron-wire`
    (`657b528`, merged via PR veyron-core/veyron-wire#4) as **0.2.3** (patch —
    additive per the release rule); **published to crates.io 2026-08-14**.
    `PROTOCOL_VERSION` 1.6 + header + `Cargo.toml` in **one commit**. Vendored
    copies re-synced byte-identical in `veyron-sdk-python#3` (+ regenerated
    `veyron_protocol_pb2.py`) and `veyron-sdk-cpp#3`; both SDK READMEs bumped
    to v1.6. Kernel drift guard extended and shipped in **PR #22**: R8-05
    staleness check now asserts the v1.6 symbols (verbatim + escaped-tail
    markers) and a new `proto_header_matches_wire_protocol_version` pairing
    test enforces the one-commit bump convention. Full kernel suite green
    (444 tests, `clippy -D warnings`, `fmt --check`). Kernel still consumes
    published `veyron-wire 0.2.2` — D-01 is proto-only; D-03 wires the new
    fields up and bumps the dependency to 0.2.3 (resolves from crates.io, no
    `[patch.crates-io]` needed — 0.2.3 is published).

- [x] **D-02 — Registry: device/user identity + `devices` map + `last_seen`.**
  - `PluginEntry` += `device_id`, `user_id` (defaults `"local"` / `"default"`).
  - New `devices: DashMap<device_id, DeviceInfo>`; update `last_seen` on
    Ping/Pong (reuse existing `pong_times`).
  - Files: `src/plugins/registry.rs`.
  - Acceptance: registration stores device/user; `devices` populated;
    `last_seen` advances on ping/pong.
  - **Status (2026-08-14): SHIPPED** — merged via PR veyron-core/veyron#23
    (`543c758`). `PluginEntry` gains `device_id`/`user_id` (empty →
    `"local"`/`"default"`); `devices: DashMap<device_id, DeviceInfo>` is
    populated at registration and `last_seen` advances on ping/pong (reuses
    `pong_times`); a device flips `Offline` once its last plugin
    unregisters; `get_device`/`list_devices` exposed for D-04. `DeviceInfo`
    is a kernel-local record shape-compatible with proto v1.6 — the kernel
    still runs `veyron-wire` 0.2.2 (proto v1.5), so **D-03** bumps the dep
    to 0.2.3 and swaps in the wire type (mechanical). The router currently
    passes `""`/`""` (host plugins → default device); D-03 parses
    `device_id`/`user_id`/`os`/`capabilities`/`protocol_version` off the
    wire. Full suite green (450 tests, `clippy -D warnings`,
    `fmt --check`).

- [x] **D-03 — Registration: parse new fields + device/user clamp + major-version reject + same-user IPC.**
  - Parse `device_id`/`user_id`/`os`/`capabilities`/`protocol_version` on
    `PluginRegister`; clamp permissions by device (JWT claims already override
    the manifest — extend the clamp to key on device).
  - Reject on `protocol_version` **major** mismatch with `ERR_PROTOCOL_MISMATCH`
    (one comparison; minor/patch accepted) — finally wires up the currently
    unused error code.
  - "Same-user only" IPC: one comparison in `check_ipc_target` / `check_ipc_send`.
  - Files: `src/ipc/protocol.rs`, `src/auth/permissions.rs`.
  - Acceptance: v1.x registers with device metadata; major mismatch rejected
    with both versions in the message; cross-user IPC denied, same-user allowed.
  - **Status (2026-08-14): SHIPPED** — this PR (`feat/d-03-registration-device-fields`).
    Kernel bumps to `veyron-wire 0.2.3` (proto v1.6, crates.io); registry now
    stores the wire `DeviceInfo`/`DeviceState`/`DeviceOs` and gains
    `DeviceMeta` + `register_with_device` (empty identity → `"local"`/
    `"default"`, device metadata refreshes on re-register). Router parses
    device/user/os/arch/os_version/capabilities off the wire, accepts a
    device-scoped JWT (`sub == device_id`) as a device ceiling (claims
    override the manifest), and rejects a `protocol_version` **major**
    mismatch with `ERR_PROTOCOL_MISMATCH` carrying both versions (empty =
    v1.5 host plugin, accepted). `check_ipc_target` enforces same-user IPC
    (cross-user denied, single-user "default" unaffected). `ActionLookup`
    boxes `PluginEntry` (wire v1.6 grew the manifest past clippy's
    large-variant threshold). Full suite green (281 unit + 86 integration,
    `clippy -D warnings`, `fmt --check`).

- [x] **D-04 — Discovery surface: enriched events + `list_devices` + `/devices` + `vyn devices`.**
  - Enrich `system.plugin_joined` / `system.plugin_left` payloads with
    `device_id`/`os`/`capabilities`.
  - Admin action `list_devices` (or extend `list_plugins`) returning `DeviceInfo`.
  - REST `GET /devices`; extend `GET /plugins` with `device_id` + `last_seen`;
    CLI `vyn devices`.
  - Files: `src/ipc/protocol.rs`, `src/events/bus.rs`, `src/api/routes.rs`,
    `src/cli/`.
  - Acceptance: joined event carries device fields; `/devices` returns the map
    with `last_seen`/`state`; `vyn devices` prints it.
  - **Status (2026-08-14): SHIPPED** — this PR
    (`feat/d-04-discovery-surface`). `system.plugin_joined`/`plugin_left`
    payloads now carry `device_id`/`os`/`capabilities`, built via
    `plugin_lifecycle_payload` (`src/events/bus.rs`) with `serde_json` (never
    `format!`) because device fields arrive off the wire unvalidated — a raw
    splice would be a JSON-injection vector. New IPC admin command
    `list_devices` (`src/kernel/commands.rs`, `PERMISSION_KERNEL_ADMIN`)
    returns the device map; REST gains `GET /devices` and `GET /plugins`
    entries now include `device_id` + `last_seen` (the owning device's).
    `vyn devices` prints the table. Full suite green (470 tests, `clippy -D
    warnings`, `fmt --check`).

---

## P1 — Client kernel + transport

- [ ] **D-05 — WS transport in `veyron-sdk-rust`.**
  Mirror the UDS client semantics (register, MAC enable, reconnect) over a WS
  backend. Respect the WS-gateway limits (no `FLAG_COMPRESSED`/`FLAG_FRAGMENTED`
  inbound; `FLAG_RAW_BINARY` passes).
  - Files: `../veyron-sdk-rust/`.
  - Acceptance: an SDK plugin connects to the WS endpoint, registers, and
    round-trips actions; integration test against the kernel WS gateway.

- [ ] **D-06 — `role: client` + bridge/mirror component + local-first routing.**
  - Config: `role: host|client` + `bridge:` section (host URL, token, mirror list).
  - Bridge: one WS connection per mirrored capability, registering as
    `device.<cap>` on the host, shuttling frames both ways.
  - Local router: resolve local `target` first, else forward via bridge
    (local-to-local must not round-trip the host).
  - Files: `src/utils/config.rs`, new bridge module, `src/kernel/orchestrator.rs`,
    `src/ipc/protocol.rs`.
  - Acceptance: `vyn --client` runs, hosts a local plugin, that plugin appears
    on the host as `device.<cap>`; local-to-local traffic stays local.

- [ ] **D-07 — Auth: per-device JWT minting + TLS by default + close the WS no-MAC gap.**
  - `vyn token mint --device …` → per-device JWT (`sub=device_id`, restricted
    claims, `aud`/nonce/short `exp`).
  - TLS (rustls) enabled by default for the network path; bind beyond
    `127.0.0.1` when `role: host`.
  - Document/close the "WS client that never registers has no frame-MAC" gap.
  - Files: `src/auth/jwt.rs`, `src/api/server.rs`, `src/api/websocket.rs`,
    `src/cli/`.
  - Acceptance: per-device token works end-to-end; TLS on by default;
    `aud`/`exp`/nonce validated.

---

## P2 — AI integration, voice, sync, companions

- [ ] **D-08 — Tool-calling surface (consumes D-01 `action_specs`).**
  Expose `action_specs` to the AI: enrich the joined event with `action_specs`
  and add a `get_manifest` admin action. Kernel only serves registry data —
  no interpretation.
  - Files: `src/ipc/protocol.rs`, `src/events/bus.rs`.
  - Acceptance: the AI can enumerate actions with
    name/description/params_schema/risk.

- [ ] **D-09 — Confirmation gate (SDK helper + reference impl).**
  Permission-separation pattern: high-risk actions split into `request_*` (the
  AI's JWT can call) and `confirm_*` (only the user's device can call). SDK
  one-liner + a reference high-risk plugin demonstrating `requires_confirmation`.
  - Files: `../veyron-sdk-rust/`, `../veyron-plugins/`.
  - Acceptance: the AI cannot call `confirm_*`; the user confirm path works.

- [ ] **D-10 — Observability: `message_id` propagation + log discipline.**
  Preserve `Envelope.message_id` across forward/broadcast (the event bus
  currently builds fresh envelopes without it) and log
  `message_id`/`sender_id`/`target`/`hop` at each hop.
  - Files: `src/ipc/protocol.rs`, `src/events/bus.rs`.
  - Acceptance: one action is traceable device → bridge → kernel → plugin via
    `message_id`.

- [ ] **D-11 — Threat model doc.**
  Assets / actors (external attacker, compromised plugin, compromised device,
  malicious prompt) / controls (TLS, JWT, frame-MAC, per-device permissions,
  overlay, confirmation gate, least-privilege AI) — consolidating §10/§19/§21.
  - Files: `docs/THREAT_MODEL.md` (new).
  - Acceptance: doc exists and covers the four actors.

- [ ] **D-12 — Voice pipeline: STT local (client) + TTS host → Opus.**
  Local STT plugin (whisper.cpp/vosk) emits text to the host; host TTS streams
  Opus to the client speaker via the existing `FLAG_RAW_BINARY` +
  `AudioStreamChunk` path. Audio never leaves the device for STT.
  - Files: `../veyron-plugins/`.
  - Acceptance: local STT → text event; host TTS → Opus → client speaker.

- [ ] **D-13 — Sync: heartbeat + snapshot + deltas + pull-on-reconnect.**
  Client plugin with `PERMISSION_SCHEDULER` publishes heartbeat/state on a
  timer; host `get_snapshot` action + subscribe to delta events; on reconnect,
  pull snapshot then subscribe (event bus is at-least-once to connected
  subscribers only).
  - Files: `../veyron-plugins/` (sync/database plugin), client scheduler.
  - Acceptance: offline client catches up on reconnect; state push works.

- [ ] **D-14 — Android device-agent app.**
  Single app exposing fixed capabilities (geo, battery, notifications,
  clipboard, contacts) that register on the host as `device.*`; persistent WS +
  foreground service.
  - Files: new `../veyron-client-android` (or under `veyron-client`).
  - Acceptance: phone appears on the host; `device.geo`/`device.battery` callable.

- [ ] **D-15 — Web companion (wss chat/control) + Web Push.**
  Browser/PWA client speaking the frame protocol over wss (TS), chat with the
  AI, control panel; Web Push (VAPID) for notifications.
  - Files: `../veyron-web` or new.
  - Acceptance: browser chats with the agent + controls the host + receives push.

- [ ] **D-16 — Android distribution.**
  Signed APKs in GitHub Releases + F-Droid metadata. Google Play later, and
  only a reduced "safe" subset (automation features are what review flags).
  - Files: release config, F-Droid metadata.
  - Acceptance: signed APK in releases + F-Droid listing.

---

## P3 — Deferred (explicit non-goals until demand)

- [ ] **D-17 — QUIC listener** (0-RTT reconnect, stream multiplexing) as a
  third `IncomingMessage` feeder.
- [ ] **D-18 — ed25519 device enrollment + revocation** (replaces the shared
  HS256 secret for device identity).
- [ ] **D-19 — WASM phone plugin runtime** (Rust → `wasm32-unknown-unknown`,
  `wasm3`/`wasmtime`; capability bridge = imports; the marketplace for `.wasm`).
- [ ] **D-20 — Bridge multi-registration per connection** (registry
  `by_conn_id` 1:1 → 1:N; replaces one-WS-per-capability).
- [ ] **D-21 — headscale plugin** (self-hosted Tailscale coordination on the
  host or a cheap VPS).
- [ ] **D-22 — FCM / UnifiedPush notification backend** (replace/augment
  persistent WS when battery or Play demands it).
- [ ] **D-23 — Multi-user enforcement** (event-bus user-scoping, per-user AI
  sessions, onboarding/UI) — the `user_id` seam (D-01/D-02/D-03) is already in
  place so this is "flip on enforcement", not a schema rewrite.

---

## Build order & estimate

1. **D-01 → D-02 → D-03 → D-04** — the foundation (identity + versioning +
   discovery), ~2 weeks, one proto bump.
2. **D-05 → D-06 → D-07** — client kernel + transport, ~3–4 weeks.
3. **D-08 → D-09** — AI tool-calling + safety, ~2 weeks (rides D-01's fields).
4. **D-10, D-11, D-12, D-13** — observability, threat model, voice, sync,
   ~3 weeks, parallelizable.
5. **D-14, D-15, D-16** — companions, ~5–9 weeks, independent track.
6. **D-17 … D-23** — deferred; pull forward only on real demand.

**MVP slice** (host + one device agent + a few capabilities + Kairo tool-calling
+ chat) ≈ **D-01…D-09 + D-12 + D-14**, roughly 8–12 weeks for one strong Rust dev
(excluding companion polish).
