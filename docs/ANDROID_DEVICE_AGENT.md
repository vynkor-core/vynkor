# vynkor Android Device-Agent (D-14)

Design + decisions for the Android companion app that turns a phone into a
remote device for a host kernel. (Project renamed **Veyron → vynkor**; see
`CLAUDE.md`.)

Sources: `docs/REMOTE_DEVICES_ROADMAP.md` (D-14, D-16),
`docs/REMOTE_DEVICES_PLAN.md` (§13–14, §18, §20).

Status: **draft** — repo, stack, voice, capabilities, rename, SDK floor agreed;
foreground-service architecture deferred with detail below. Rust core + UniFFI
boundary: `docs/ANDROID_DEVICE_AGENT_RUST_CORE.md`.

## Repo

New repo: `../vynkor-client-android` (created under the current `veyron-core`
org until the org rename lands).

Not under `vynkor-client` (stays empty by design — plan §16: the desktop
client is a `role: client` config of `vyn`, not a separate repo). Own CI,
release signing (D-16), F-Droid metadata.

## Stack: Kotlin + Rust (UniFFI)

- **Rust core** owns everything protocol/backend. Reuses **`vynkor-wire`
  (currently `veyron-wire`) types only** — 44-byte frame framing,
  HMAC-SHA256 frame-MAC, protobuf `Envelope`. **Do NOT pull `vynkor-sdk`
  (currently `veyron-sdk-rust`)**: the agent is a device bridge (like the D-06
  bridge), not a `Plugin` — the SDK's `Plugin` trait brings nothing we need.
  The core is a thin WS client: register `<device_id>.<cap>`, relay frames,
  reconnect/backoff, JWT. No Android APIs in Rust.
- **Kotlin** owns only the Android surface: Activity + foreground-service
  lifecycle, runtime permissions, `NotificationListenerService`, media/sensor
  APIs. Binds to Rust via **UniFFI** (generated Kotlin bindings).
- Boundary: UniFFI interface only — no protocol logic in Kotlin, no Android
  logic in Rust.

## Voice (D-12) — in the first release

- **STT — on the client (Android), local.** Audio never leaves the device for
  STT (privacy + latency, plan §9). Engine: sherpa-onnx (already used by the
  host `stt` plugin) or vosk/whisper.cpp.
- **TTS — both:**
  - **host** (strong models — Kokoro/ElevenLabs) streamed to the client
    speaker as Opus (`FLAG_RAW_BINARY` + `AudioStreamChunk`, D-12).
  - **client** (weak local model, sherpa-onnx) when offline or host
    unreachable.
- `<device_id>.mic` (capture → Opus) and `<device_id>.speaker` (playback) are
  Tier 1 capabilities to support this.

## Transport

Persistent WS + foreground service (MVP, plan §20). JWT in
`Sec-WebSocket-Protocol` header. Reconnect with exponential backoff (mirror
the D-06 bridge). TLS: pin the host's served cert (as the local `vyn` clients
do in D-07).

## Identity

Stable `device_id` per install (Android has no `$HOSTNAME`). Random UUID on
first run, persisted app-private. Registers as `<device_id>.<cap>`; JWT `sub =
device_id` + restricted claims (kernel clamps at registration, D-03).

## Capabilities

Each = one `<device_id>.<cap>` plugin on the host, backed by a restricted JWT
permission set.

### Tier 1 — MVP (agreed)

| cap | what | why |
|---|---|---|
| `<device_id>.geo` | location one-shot + subscribe | find-my-phone, automation |
| `<device_id>.battery` | level/charging/temp/is_low | trivial, useful |
| `<device_id>.notifications` | read incoming (app/title/body) | AI-assistant input |
| `<device_id>.clipboard` | read + write | low risk, high value |
| `<device_id>.contacts` | read contacts | "call/message X" |
| `<device_id>.mic` | capture → Opus | D-12 STT (client) |
| `<device_id>.speaker` | playback | D-12 TTS (host + client) |

### Tier 2 — near-term

`<device_id>.camera` (photo/QR) · `<device_id>.sms` (read inbox; send deferred) ·
`<device_id>.screen` (on/off/lock; screenshot needs a privacy flag) ·
`<device_id>.sensors` (accel/gyro/light/proximity) · `<device_id>.media` (playback
control) · `<device_id>.wifi` (SSID/connectivity) · `<device_id>.bluetooth` (paired
devices) · `<device_id>.calendar` (read events) · `<device_id>.files` (media store +
app-scoped) · `<device_id>.torch` (flashlight) · `<device_id>.callstate` ·
`<device_id>.apps` (list installed; launch deferred).

### Tier 3 — defer (Play review flags / needs design)

`<device_id>.automation` (accessibility UI automation) · `<device_id>.dial` /
`<device_id>.sendsms` (direct dial/send) · `<device_id>.screencast` (remote control) ·
`<device_id>.keychain` (secrets store).

## Notification onboarding (agreed)

`NotificationListenerService` is not grantable as a normal runtime permission
— the user must enable it in system settings. The app ships an onboarding
screen explaining this + a deep link to the settings page.

## Deferred — foreground/background service architecture

Decision: **do nothing extra for now** — MVP runs a single foreground service
holding the WS. Revisit the multi-type architecture when we actually hit the
limits.

Why it's non-trivial (context for the later decision):

- **Android 14 (API 34+):** every foreground service must declare a
  `foregroundServiceType` in the manifest and hold the matching
  `FOREGROUND_SERVICE_*` permission. Types include `connectedDevice`,
  `dataSync`, `location`, `microphone`, `mediaPlayback`, …
- **One type per need:** the WS link wants `connectedDevice`/`dataSync`;
  `<device_id>.geo` wants `location`; `<device_id>.mic` wants `microphone`. A single
  service can declare several types, but each type adds its own runtime
  start restrictions and notification requirements.
- **Android 13+:** `POST_NOTIFICATIONS` runtime permission required before the
  FGS notification shows.
- **Android 15 (API 35):** 6-hour limit on `dataSync`-type services, stricter
  background-start rules — relevant to "stay connected all day".
- **OEM killers** (Samsung/Xiaomi) kill background services regardless; plan
  §20 notes users must whitelist the app.

Open: which types to declare, one service vs several, and whether to move to
FCM/UnifiedPush later (plan §20) instead of a permanent FGS.

## min/target SDK (confirmed)

- **`minSdk 26`** (Android 8.0) — the floor where foreground services and
  `NotificationListenerService` work cleanly; ~98% of devices.
- **`targetSdk 35`** (Android 15) — Google Play requires new apps to target a
  recent API level.

(`targetSdk` = the API level the app is compiled against / declares
compatibility with — it gates which OS runtime restrictions apply; `minSdk` =
oldest Android the app installs on.)

## Decisions (previously open)

- **Client STT engine: sherpa-onnx** — same engine as the host `stt` plugin,
  supports streaming (online) ASR, ships Android bindings; keeps local models
  consistent with the host.
- **WS client crate: tokio-tungstenite** — mirrors the D-06 bridge exactly
  (already the kernel's WS stack); TLS via rustls with cert pinning.
- **`device_id` source: app-generated UUID**, persisted app-private on first
  run. `Settings.Secure.ANDROID_ID` is per-signing-key and resets on factory
  reset — not stable enough for device identity.

Rust core + UniFFI boundary: see `docs/ANDROID_DEVICE_AGENT_RUST_CORE.md`.
