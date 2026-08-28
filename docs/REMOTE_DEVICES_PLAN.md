# Remote Devices — Design & Plan

Status: **partially shipped** — D-01..D-14 shipped (2026-08-14..2026-08-16, proto v1.6), E-01 per-device credentials shipped as proto v1.7 (2026-08-22), D-15..D-23 deferred. Design decisions remain valid; kernel implementation lives in REMOTE_DEVICES_ROADMAP.md. Design decisions remain valid; kernel implementation lives in REMOTE_DEVICES_ROADMAP.md.

Goal: let a single Vynkor kernel (renamed from Veyron 2026-08-22, running on the user's own machine) act as a
**hub** that additional "device" clients (phone, second PC, laptop) connect to
over the network. Devices host their own capabilities, are addressed/routed
through the kernel, and are permission-limited per device.

## 1. Guiding decisions

1. **Local-first stays the default.** The kernel keeps working on the user's
   own machine over direct UDS, no network. "Host/cloud" is not a migration —
   it is an optional deployment. Most users keep everything on one machine.
2. **The kernel is the hub.** All device↔device and device↔plugin traffic
   routes through the kernel (star topology). No peer-to-peer device links.
3. **A "device" is a first-class (but additive) identity dimension in the
   kernel, not a plugin.** Identity / routing / permissions are the kernel's
   job per the manifesto. A plugin cannot do this — it never sees other
   plugins' frames, and a "device-hub plugin" would break zero-parse routing
   and duplicate the registry.
4. **Reuse, don't rewrite.** Transport, routing, permissions, events, frame-MAC
   are reused verbatim. New work is additive metadata + a client-side device
   agent.

## 2. Model

- **Main kernel** (user's PC): full Vynkor (formerly Veyron), unchanged core. Hosts the AI agent
  (Kairo), `database`, `secrets`, heavy compute.
- **Device agents**: phone / second PC / laptop. Each connects over WS (JWT)
  and registers its capabilities as namespaced plugins:
  `phone-abc.geo`, `phone-abc.battery`, `pc-mac.clipboard`.
- Every plugin has an owner device; local plugins have `device_id = "local"`.

Flow: `pc-plugin → kernel → Kairo → kernel → phone-plugin`
(existing `target` routing + `ipc_targets` allowlist).

## 3. Transport

- **Now: WebSocket** — reuse the existing gateway. A WS client can already
  register as a plugin; the router is transport-agnostic (`IncomingMessage`
  fed identically by UDS and WS). Mirror via one connection per device
  capability = **zero kernel change**.
- **Later: QUIC** as an additive listener (0-RTT reconnect, stream
  multiplexing, no head-of-line blocking) behind the same router.

## 4. Device identity & plugin namespace

- **Wire identity**: namespaced `plugin_id` (`<device_id>.<capability>`) —
  keeps zero-parse routing on the `target` field unchanged and guarantees
  global uniqueness across devices.
- **Explicit metadata**: a denormalized `device_id` field on the plugin entry,
  for permissions and discovery (cleaner than parsing the id prefix).

## 5. Kernel changes (additive, ~1–2 weeks)

1. **proto** — **DONE (D-01, 2026-08-14, proto v1.6; E-01 bumped to v1.7 2026-08-22):** `PluginRegister` +=
   `device_id`, `os` (enum), `arch`, `os_version`, `capabilities[]`,
   `protocol_version`, `user_id`. `PluginManifest` += `platforms[]`
   (install filtering) + `action_specs[]` (tool schema). New `DeviceInfo`.
2. **registry**: `PluginEntry.device_id` (default `"local"`); new
   `devices: DashMap<device_id, DeviceInfo { os, arch, os_version,
   capabilities, last_seen, state }>`. (SHIPPED D-02, see
   REMOTE_DEVICES_ROADMAP.md)
3. **protocol.rs**: parse new fields; device-scoped permission clamp (JWT
   `claims.permissions` already override manifest permissions at registration
   — extend to key on `device_id`). (SHIPPED D-03, see
   REMOTE_DEVICES_ROADMAP.md)
4. **events**: enrich `system.plugin_joined` / `system.plugin_left` payloads
   with `device_id` / `os` / `capabilities`. (SHIPPED D-04, see
   REMOTE_DEVICES_ROADMAP.md)
5. **API/CLI**: `GET /devices`; extend `GET /plugins` with `device_id` +
   `last_seen`; `vyn devices`. (SHIPPED D-04, see REMOTE_DEVICES_ROADMAP.md)
6. **liveness**: update `last_seen` on Ping/Pong (`pong_times` already tracked
   in the registry). (SHIPPED D-02, see REMOTE_DEVICES_ROADMAP.md)

## 6. Permissions

- **Device ceiling**: the device agent's JWT carries a restricted permission
  set; the kernel clamps at registration (existing `claims.permissions`
  override of the manifest).
- **Read/write granularity**: enforced in the `database` plugin via the
  kernel-stamped, unspoofable `ActionRequest.caller_plugin_id` (e.g.
  `phone-*` → read-only). This is plugin logic — the kernel stays dumb.

## 7. Discovery & liveness (so the AI sees topology)

- **Snapshot on start**: `list_devices` / `list_plugins` (admin action or REST).
- **Deltas**: `system.plugin_joined` / `plugin_left` events with device metadata.
- **Liveness**: `last_seen` from ping/pong (watchdog pings already exist).

## 8. Plugin taxonomy

- `run_on: host | client | any` in the manifest — deployment metadata, **not**
  a runtime placement scheduler.
- `platforms[]` in the manifest for install filtering (`android-14+`,
  `macos`, ...).
- `any` (portable): weather, search, calculator, rss.
- `host`: kairo (AI), database, secrets, integrations.
- `client` (device): geo, battery, clipboard, screen, notifications, mic.

## 9. Voice

- **STT local** on the client (privacy + latency); text reaches the host AI
  agent as ordinary events/actions. Audio never leaves the device.
- **TTS on the host** (bigger models) streamed to the client speaker as Opus
  over the existing `FLAG_RAW_BINARY` + `AudioStreamChunk` path.

## 10. Auth / channel security

- **Now**: per-device JWT (HS256), `sub = device_id`, long exp, restricted
  claims; reuse the whole nonce → frame-MAC flow. TLS (rustls) enabled on the
  network path; close the "WS client without registration has no frame-MAC"
  gap.
- **Later**: asymmetric device identity (ed25519 + enrollment + revocation).

## 11. Sync

- **State push**: a client plugin with `PERMISSION_SCHEDULER` publishes a
  heartbeat/snapshot event on a timer.
- **Receive**: client `Subscribe`s to event types; host publishes deltas.
- **Offline gap**: the event bus is at-least-once to *connected* subscribers
  only (offline client → drop-and-log). Reconnect = pull `get_snapshot` (host
  plugin) then subscribe to deltas. Cheap v1: online-only best-effort +
  manual resync on reconnect.

## 12. Local storage on the client

Nothing new to build: `EventStore` (SQLite, already optional in the binary),
the `database` plugin (`PERMISSION_STORAGE`) run locally, and `installed.json`
for plugin state.

## 13. Platform matrix

| Role | Linux | macOS | Windows | Android/iOS | Web |
|---|---|---|---|---|---|
| Main kernel (hub) | full | full (no sandbox) | host via WSL | — | — |
| Device agent (plugins) | minimal kernel | minimal kernel | skip (not "months") | single app, fixed capabilities | — |
| Companion (chat/control) | — | — | — | wss app | wss |

## 14. Phone plugins — decision

- **DO**: device capabilities (geo/battery/clipboard/notifications/mic)
  exposed as namespaced plugins by the device-agent app.
- **DEFER**: general "download arbitrary plugins and run them on the phone".
  The hard part is not the marketplace UI — it is a *safe runtime* for
  arbitrary code on iOS/Android (no exec, no code-loading; Google Play / App
  Store policy). The chosen path when needed is **Rust → WASM**: compile the
  plugin to `wasm32-unknown-unknown`, run it in an embedded `wasm3`/`wasmtime`
  interpreter. WASM is explicitly permitted by both Apple and Google (data +
  interpreter, not downloaded native code), is sandboxed by construction, and
  the capability bridge maps 1:1 onto the permission model (a plugin only gets
  the host functions it declared — `call`, `read_contacts`, ... are imports).
  This is a third SDK/runtime (deferred, weeks–months), not a quick win.
- **NOT via phone**: cross-app automation (e.g. a Telegram user-bot on the
  device). That is a long-running background concern that belongs on the host
  using TDLib/GramJS in a normal desktop plugin; the phone is just the UI.

## 15. Time estimate (one strong Rust dev)

| Step | Weeks |
|---|---|
| WS transport in `veyron-sdk-rust` | 1–2 |
| `role: client` + bridge + local-first routing + per-plugin JWT | 2–3 |
| TLS default + JWT (aud/nonce/exp, per-device mint) | 1 |
| STT local + TTS host→Opus | 1–2 |
| Sync (heartbeat + snapshot + deltas) | 1 |
| **MVP (client + bridge + voice + sync)** | **≈5–7** |
| Web companion | 2–4 (independent) |
| Android companion | 3–6 (independent) |
| Kernel device identity (proto + registry + events + `/devices`) | 1–2 |

## 16. Non-goals (deferred)

QUIC · ed25519 enrollment · CRDT/sync engine · full plugin host on Android/iOS
· native Windows client · runtime placement scheduler · separate client repo
(`veyron-client` stays empty; the client is a `role: client` config of `vyn`).

## 17. Open questions — status

1. **Host onboarding** — partially resolved: `curl -sSL
   https://core.veyron.online/install.sh | bash` ships a prebuilt binary. Still
   open: auto-start (systemd/launchd), self-update, and "persistent reachable
   host" is a deployment story, not just a binary install.
2. **Host reachability** — decided (§19): free Tailscale now, headscale-as-
   plugin / WireGuard later. Zero kernel change; deployment layer only.
3. **Phone notification delivery** — options in §20; MVP = persistent WS +
   foreground service.
4. **AI tool-calling + safety** — design in §21; needs manifest v3 (tool schema)
   - a confirmation gate.
5. **Kairo model** — resolved: the plugin already supports local models and API
   keys, user's choice. Remaining (memory/RAG, tool loop) is Kairo's own
   roadmap, out of kernel scope.

## 18. Android distribution — decision

Start with signed APKs in GitHub Releases + F-Droid (open-source store, no
policy fight, auto-updates). Google Play later, and only a reduced "safe"
feature subset (the automation features — `CALL_PHONE`, app-launch,
Accessibility — are exactly what Play review flags). The app is not
standalone: it needs a host, so distribution is gated on the host-onboarding
story (open question #1).

## 19. Host reachability — decision

- Physics: at least one endpoint must be reachable. In hub-and-spoke only the
  host needs it (clients dial out).
- **Now**: free Tailscale — zero code/ops, works through CGNAT, end-to-end
  encrypted (the coordination server sees device metadata, not Vynkor frames).
- **Later (self-hosted)**: headscale as a plugin on the host
  (protocol-compatible with Tailscale clients) — still needs one reachable
  address, so a cheap VPS if the home is behind CGNAT. Direct WireGuard works
  only with a real/forwarded IP.
- Zero kernel/protocol change — a deployment overlay under Vynkor (formerly Veyron).

## 20. Notification delivery (host → phone)

| Option | Code effort | Reliability | Battery | Cloud dep |
|---|---|---|---|---|
| Persistent WS + foreground service | low (WS exists) | medium (OEM killers) | high | none |
| FCM | medium | high | low | Google |
| UnifiedPush (ntfy/self-host) | medium | medium–high | low | optional |
| Web Push (VAPID) | low | high | n/a (PWA) | none |
| Telegram/ntfy channel | trivial | high | low | yes |
| Poll (WorkManager) | trivial | latency-bound | medium | none |

MVP: persistent WS + foreground service (user opts in; document OEM whitelist).
Web/PWA: Web Push. FCM later (E2E-encrypt the payload so Google sees only a
wake ping), or UnifiedPush for a FOSS-aligned architecture.

## 21. AI tool-calling & prompt-injection safety

Manifest v3: `actions[]` (strings) → structured `ActionSpec { name, description,
params_schema (JSON Schema), risk, requires_confirmation }`. This is *data* the
AI plugin reads; the kernel stays dumb and only enforces permissions on the
resulting ActionRequest.

Layered injection defense (structural, not prompt engineering):

1. **Least privilege for the model** — Kairo runs with a restricted JWT
   (permissions + `ipc_targets` allowlist), reusing existing machinery. A fully
   injected prompt can only reach allowlisted low-risk tools.
2. **Confirmation gate** — high-risk tools (`requires_confirmation`) emit a user
   approval request; the model cannot self-approve.
3. **Argument schema validation** — validate tool args against `params_schema`
   before execution.
4. **Data vs authority** — tool descriptions come from the trusted manifest;
   web/email content and tool arguments are untrusted, never instructions.
5. **Rate/spend caps** — existing `ACTION_QUOTA_EXCEEDED` + rate limiting.
6. **Audit log** — persist every AI-initiated action (caller + args + result).

## 22. Decisions — the remaining seven

1. **Manifest v3 (tool schema)** — additive: keep `actions[]` for the router,
   add `action_specs[] { name, description, params_schema, risk,
   requires_confirmation }` for the AI. Kernel does not parse the schema.
   High · low-med · ~1 wk. — **DONE (D-01, 2026-08-14, proto v1.6):**
   `ActionSpec`/`ActionRisk` landed in the proto; served from the registry in
   D-08.
2. **Confirmation gate** — plugin-level by permission separation (`request_*`
   vs `confirm_*`; the AI's JWT cannot call `confirm_*`), not a kernel gate
   (a kernel gate would violate dumb-core). SDK helper + the
   `requires_confirmation` flag. High · med · ~1 wk (after #1). — **DONE
   (D-09, 2026-08-15, see REMOTE_DEVICES_ROADMAP.md)**
3. **Multiuser** — add the identity seam now (§23), defer enforcement.
   Med · low (seam) · ~1 wk.
4. **Versioning** — `PluginRegister.protocol_version` (semver; the kernel
   rejects on major mismatch — one comparison) + `capabilities[]` (feature
   negotiation; the kernel stores/passes, never interprets). High · low-med ·
   ~1 wk (with #1). — **DONE (D-01, 2026-08-14, proto v1.6):** both fields
   landed in `PluginRegister`; the major-mismatch reject wires up in D-03.
5. **Kairo** — one plugin with internal modules for MVP; the kernel only serves
   the registry (enrich `system.plugin_joined` / add `get_manifest`).
   High (product) / low (kernel) · high (product) · months (separate repo).
6. **Observability** — reuse `Envelope.message_id` as the correlation id; fix
   propagation (the event bus builds fresh envelopes without `message_id`
   today) + log discipline. Med · low · ~2–3 days. — **DONE (D-10,
   2026-08-15, see REMOTE_DEVICES_ROADMAP.md)**
7. **Threat model** — one focused doc (assets / actors / controls),
   consolidating §10/§19/§21. Med · low · ~1 day. — **DONE (D-11,
   2026-08-15, see REMOTE_DEVICES_ROADMAP.md)**

Build order: **#1 + #4 together** (same proto change) → **#2** (rides #1's
flag) → **#6 + #3 + #7** in parallel (cheap) → **#5** (separate track).

## 23. Multiuser — cheap now vs premature

The schema/permission/DB part is genuinely cheap and should land now so it is
never a migration. The expensive part is *enforcement*, not identity.

**Now (~1 wk):**

- `user_id` in JWT claims + on registration/device.
- `database` plugin keys by `user_id` (even a constant "default" for v1).
- "same-user only" IPC rule (one comparison in `check_ipc_target` /
  `check_ipc_send`).
- registry carries `user_id` (like `device_id`).

**Defer (the real cost):**

- Event-bus user-scoping (subscribe/deliver by user, not just `event_type`) —
  a core-component change; today a co-user's plugin could subscribe to `*`.
- Per-user AI (agent sessions, memory, tool permissions per user).
- Multi-user onboarding/UI (invite, roles, per-user device management).
- Security hardening (asymmetric identity + per-user revocation) — a shared
  HS256 secret is amplified by co-users.
- Rate-limit keying by user.

The identity seam now means multiuser later is "flip on the enforcement", not
"rewrite the schema". Skipping the seam means retrofitting `user_id` into a
live single-user system later — that is the painful migration.

## 24. Ship readiness & expected churn

This is the *engineering* plan, not the *product*. "Ship" additionally needs an
MVP slice (host + device agent + one real capability + Kairo doing something
useful), a host onboarding a non-dev survives, and basic UX — none of which are
kernel items.

Settled (minimal redo): additive proto discipline, transport-agnostic router,
`user_id` seam, dumb core.

Will iterate (product-level, expected): tool-schema shape once the AI uses it,
confirmation UX, notification transport (WS ↔ FCM ↔ UnifiedPush), the
device-agent capability set, the bridge model (a → b multi-registration).
