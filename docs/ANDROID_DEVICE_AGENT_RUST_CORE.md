# vynkor Android Device-Agent — Rust core design

The Rust core is the **protocol engine**; Kotlin is the **device-I/O layer**;
UniFFI is the boundary between them. (Project renamed Veyron → vynkor.)

Scope: D-14, first release. See `docs/ANDROID_DEVICE_AGENT.md` for the
product decisions (repo, stack, capabilities).

## Principle: Rust = protocol, Kotlin = hardware

- Rust owns: framing, frame-MAC, protobuf, the WS client, registration,
  reconnect/backoff, Opus encode/decode, and per-capability routing.
- Kotlin owns: `AudioRecord`/`AudioTrack`, `BatteryManager`,
  `FusedLocationProvider`, `ClipboardManager`, `NotificationListenerService`,
  `ContactsContract`, foreground-service lifecycle, runtime permissions.
- No protocol logic in Kotlin; no Android API in Rust.

## Crate layout

```
vynkor-client-android/
├── rust/                       # the core crate (vynkor-agent-core)
│   ├── Cargo.toml              # crate-type ["cdylib"]; android targets
│   ├── uniffi.toml             # Kotlin package name + bindings config
│   └── src/
│       ├── lib.rs              # uniffi::setup_scaffolding!() + module tree
│       ├── ffi.rs              # the UniFFI surface (objects, records, traits)
│       ├── agent.rs            # Agent object: lifecycle, provider registry
│       ├── transport.rs        # WS client: connect, backoff, TLS pin, read/write loop
│       ├── protocol.rs         # register flow, frame build/parse, MAC
│       └── caps/
│           ├── mod.rs          # Capability trait + dispatch (target -> handler)
│           ├── battery.rs
│           ├── geo.rs
│           ├── notifications.rs
│           ├── clipboard.rs
│           ├── contacts.rs
│           ├── mic.rs          # PCM -> Opus, streaming
│           └── speaker.rs      # Opus -> PCM, streaming
├── app/                        # the Kotlin/Gradle Android app
│   └── src/main/kotlin/...     # Activity, FGS, capability providers, UniFFI consumers
└── build/                      # Gradle tasks: cargo ndk + uniffi-bindgen
```

Build: a Gradle task runs `cargo ndk` (targets `aarch64-linux-android`,
`armv7-linux-androideabi`, `x86_64-linux-android`) to produce `libvynkor_agent.so`,
then `uniffi-bindgen` to emit the Kotlin bindings; `app/` loads the `.so` via
`System.loadLibrary`.

## UniFFI boundary

Three kinds of surface:

1. **Records** (plain data, Kotlin data classes) — config.
2. **Object `Agent`** (Rust-owned; Kotlin calls its methods) — lifecycle + the
   Kotlin→Rust push path.
3. **Foreign traits** (`with_foreign`; Kotlin implements, Rust calls) — the
   capability backends Rust pulls from.

```rust
// ffi.rs — records
#[derive(uniffi::Record)]
pub struct AgentConfig {
    pub host_url: String,          // wss://host:port/ws
    pub jwt_token: String,         // device JWT (sub = device_id)
    pub jwt_secret: String,        // host's jwt_secret, for frame-MAC derivation
    pub device_id: String,         // stable per-install UUID
    pub capabilities: Vec<String>, // ["geo","battery","notifications","clipboard","contacts","mic","speaker"]
}

// ffi.rs — foreign traits (Kotlin implements, Rust calls)
#[uniffi::export(with_foreign)]
pub trait BatteryProvider: Send + Sync {
    fn level_percent(&self) -> u8;
    fn is_charging(&self) -> bool;
    fn temperature_c(&self) -> f32;
}

#[uniffi::export(with_foreign)]
pub trait LocationProvider: Send + Sync {
    fn last_known(&self) -> Option<Location>;
}

#[uniffi::export(with_foreign)]
pub trait ClipboardProvider: Send + Sync {
    fn read(&self) -> Option<String>;
    fn write(&self, text: String);
}

#[uniffi::export(with_foreign)]
pub trait ContactsProvider: Send + Sync {
    fn list(&self, query: String) -> Vec<Contact>;
}

#[uniffi::export(with_foreign)]
pub trait SpeakerSink: Send + Sync {
    fn play_pcm(&self, pcm: Vec<u8>);   // s16le mono, 16 kHz
}

#[derive(uniffi::Record)]
pub struct Location { pub lat: f64, pub lon: f64, pub accuracy_m: f32 }
#[derive(uniffi::Record)]
pub struct Contact { pub name: String, pub phones: Vec<String>, pub emails: Vec<String> }

// ffi.rs — the Agent object (Kotlin -> Rust)
#[uniffi::Object]
pub struct Agent { /* tokio runtime, transport, caps registry */ }

#[uniffi::export]
impl Agent {
    #[uniffi::constructor]
    pub fn new(config: AgentConfig) -> Self;

    pub fn start(&self);                 // spawn WS connect + register loop
    pub fn stop(&self);
    pub fn is_connected(&self) -> bool;

    // register the capability backends Kotlin provides
    pub fn set_battery(&self, p: Arc<dyn BatteryProvider>);
    pub fn set_location(&self, p: Arc<dyn LocationProvider>);
    pub fn set_clipboard(&self, p: Arc<dyn ClipboardProvider>);
    pub fn set_contacts(&self, p: Arc<dyn ContactsProvider>);
    pub fn set_speaker(&self, p: Arc<dyn SpeakerSink>);

    // Kotlin -> Rust push paths (event-driven capabilities)
    pub fn push_mic_pcm(&self, pcm: Vec<u8>);                 // AudioRecord loop
    pub fn on_notification(&self, app: String, title: String, body: String);
    pub fn on_clipboard_change(&self, text: String);
    pub fn push_geo_update(&self, loc: Location);
}
```

Flow summary:

| capability | direction | mechanism |
|---|---|---|
| `<device_id>.battery` | host→device request | `BatteryProvider` (foreign trait) |
| `<device_id>.geo` | host→device request + device→host push | `LocationProvider` + `push_geo_update` |
| `<device_id>.notifications` | device→host event | `on_notification` (Rust method) |
| `<device_id>.clipboard` | both | `ClipboardProvider` + `on_clipboard_change` |
| `<device_id>.contacts` | host→device request | `ContactsProvider` |
| `<device_id>.mic` | device→host stream | `push_mic_pcm` → Opus → raw frame |
| `<device_id>.speaker` | host→device stream | raw frame → Opus decode → `SpeakerSink.play_pcm` |

## Registration flow (mirrors D-06 bridge)

One WS connection per capability (v1; the registry is 1 plugin per connection
— D-20 defers multi-registration, so collapsing to one connection comes later).

Per capability `cap`:

1. Connect WS to `host_url` with `Sec-WebSocket-Protocol: veyron, <jwt_token>`.
2. Send `Envelope { PluginRegister { plugin_id: "<device_id>.<cap>",
   version, manifest, jwt_token, device_id, os: DEVICE_OS_ANDROID (4),
   arch, os_version, capabilities: [cap], protocol_version: "1.6",
   user_id } }`.
3. Read `PluginRegisterAck { accepted, session_nonce }`.
4. Derive the per-session MAC key:
   `derive_session_key(jwt_secret, session_nonce, "<device_id>.<cap>")`
   (`vynkor_wire::mac::derive_session_key`, HKDF-SHA256).
5. From then on every frame carries `FLAG_MAC_PRESENT` + `compute_tag(key,
   header, payload)`; inbound frames are MAC-verified with `verify_tag`.

Reconnect + backoff mirrors the bridge (`BRIDGE_MAX_BACKOFF`); TLS pins the
host's served certificate (same rule as the local `vyn` clients, D-07).

Reused verbatim from `vynkor-wire 0.0.2` (proto v1.7 — legacy `veyron-wire` 0.2.3 shim still present): `write_frame_raw`,
`read_frame`, `serialize_header`, `Frame`, the `FLAG_*` constants,
`derive_session_key`/`compute_tag`/`verify_tag`, and `proto::veyron::{Envelope,
PluginRegister, PluginRegisterAck, AudioStreamChunk, ActionRequest, …}`.

## Naming — decided: `<device_id>.<cap>`

`plugin_id = "<device_id>.<cap>"` (e.g. `phone-abc.geo`), per plan §4. **Decision
(2026-08-15):** this wins over the D-06 bridge's original literal `device.<cap>`
— the kernel bridge now registers `{device_id}.{cap}` (see
`REMOTE_DEVICES_ROADMAP.md` D-14). The host router only cares about the
`target` string, so this was a naming/allowlist decision, not a protocol change.
Follow-ups: D-09's confirm allowlist glob `device.*` must move to
`<device_id>.*` (operator-set; see the gated-write task under D-14), and the
`veyron-sdk-rust`/`tts` doc examples still show the old form.

## Audio (mic/speaker) detail

- **mic**: Kotlin `AudioRecord` (16 kHz mono s16le) → `push_mic_pcm` in ~20 ms
  chunks → Rust encodes Opus → `FLAG_RAW_BINARY` frame `target = "stt"` (host
  STT plugin). Audio never leaves the device as PCM for STT (plan §9).
- **speaker**: Rust receives `FLAG_RAW_BINARY` Opus frames (target
  `<device_id>.speaker`) → decode to PCM → `SpeakerSink.play_pcm`.
- Opus codec crate: `audiopus` (libopus FFI, cross-compiles for the NDK) or the
  pure-Rust `opus` crate — pick at implementation; encode/decode lives entirely
  in Rust, Kotlin sees PCM only.

## Key implementation notes

- **Runtime ownership**: the core owns a Tokio runtime on a dedicated thread
  (`std::thread` + `Runtime::new`, not `#[tokio::main]`); `start()` spawns the
  connect/read/write loops on it; `stop()` triggers a graceful shutdown.
- **Foreign-trait calls are synchronous**: the capability backends must return
  fast (read a cached value, not block on I/O) — long work (location fix) uses
  the Kotlin→Rust push path instead.
- **Threading**: the Rust read loop must not call back into Kotlin on the
  read-loop thread if a backend can block; delegate via the runtime + channels
  where needed.
- **UniFFI binding syntax is finalized at implementation** (proc-macros
  `#[uniffi::Object]` / `#[uniffi::export]` / `#[uniffi::export(with_foreign)]`
  + `uniffi::setup_scaffolding!()`); the shapes above are the contract, not the
  exact generated code.
