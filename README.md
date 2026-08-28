# Vynkor

**Your personal cloud. One kernel, any language, any device.**

> A tiny Rust daemon that turns a laptop into a private cloud. Plugins — AI, storage, automations — run as isolated processes and talk through Vynkor. Your phone becomes a remote device. No vendor, no cloud account.

[![Kernel 0.1.0](https://img.shields.io/badge/kernel-0.1.0-blue)](https://github.com/vynkor-core/vynkor/blob/main/ROADMAP.md) [![Proto v1.7](https://img.shields.io/badge/proto-v1.7-green)](https://github.com/vynkor-core/vynkor-wire/blob/main/proto/vynkor_protocol.proto) [![License MIT/Apache](https://img.shields.io/badge/license-MIT%2FApache--2.0-orange)](https://github.com/vynkor-core/vynkor/blob/main/LICENSE-MIT) [![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-red)](https://github.com/vynkor-core/vynkor/blob/main/Cargo.toml)

---

## Why Vynkor?

**You keep control.** AI, files and automations run on your host. No data leaves unless a plugin you allowed sends it. Offline works.

**Any language you already know.** Same `Plugin` trait in Rust, Python and C++ — one protocol, one `proto` file. Prototype in Python, ship in Rust.

**Your phone is a plugin.** Android device-agent exposes `battery`, `geo`, `clipboard`, `notifications`, `mic`/`speaker` as `my-phone.geo`, `my-phone.mic` — call them like any local plugin.

**One command to extend.** `vynm install ai` fetches a signed archive, verifies it, and drops a `plugins.d/ai.yaml`. No kernel rebuild, no manual wiring.

**Built to be boring (in a good way).** The kernel only routes 44-byte frames, supervises processes and checks permissions. If a plugin crashes, only that plugin restarts. Zero-parse routing, HMAC-signed frames, default-deny permissions, Landlock + seccomp sandbox on Linux.

---

## How it works

```
Browser ──┐
Phone ────┼─ wss (TLS + per-device JWT) ──→  Vynkor (vyn)  ── UDS (0o600) ──→  Plugins
Laptop ───┘                                  routing + auth     44-byte frame: Magic|Flags|Length|Target|CRC
                                                                  ai · network · stt/tts · database · secrets · …
```

1. Plugin registers over UDS: `PluginRegister { plugin_id, manifest }` → `PluginRegisterAck { session_nonce }`.
2. Kernel derives a per-session HMAC key and enforces `ipc_targets` + permissions.
3. Clients talk over `wss://host:port/ws` (`Sec-WebSocket-Protocol: vynkor, <jwt>`). Phone pairs by scanning a `vynkor://pair` QR.

The kernel never inspects the payload — it reads 32 bytes of `target` and routes.

---

## Ecosystem — one org, many repos

All repos live in `vynkor-core`. The wire protocol in [`vynkor-wire`](https://github.com/vynkor-core/vynkor-wire) is the single source of truth.

| Repo | You need it when… |
|---|---|
| [**`vynkor`**](https://github.com/vynkor-core/vynkor) — kernel `vyn` | You run a host. `vyn start / status / logs`, `vyn device connect` (QR). |
| [**`vynkor-wire`**](https://github.com/vynkor-core/vynkor-wire) `0.0.3` | You build anything — framing, MAC, protobuf types. Everyone depends on it. |
| [**`vynkor-manager`**](https://github.com/vynkor-core/vynkor-manager) — `vynm` | You install plugins. `vynm search/install/remove/update`, signed registry at `~/.local/lib/vyn/plugins/`. |
| [**`vynkor-sdk`**](https://github.com/vynkor-core/vynkor-sdk) (Rust) · [**`vynkor-sdk-cpp`**](https://github.com/vynkor-core/vynkor-sdk-cpp) · [**`vynkor-sdk-python`**](https://github.com/vynkor-core/vynkor-sdk-python) | You write a plugin. `impl Plugin { on_init, on_message, on_shutdown }`, `VynkorClient::send_action / publish_event`. |
| [**`vynkor-plugins`**](https://github.com/vynkor-core/vynkor-plugins) | You want ready-made power. `ai` (multi-provider chat), `network` (guarded HTTP), `stt`/`tts`, `database` (KV/SQL), `secrets`, `scheduler`, `media`… |
| [**`vynkor-web`**](https://github.com/vynkor-core/vynkor-web) | You want a UI. Marketplace + device list + plugin control (Vite + Tailwind). |
| [**`vynkor-client-android`**](https://github.com/vynkor-core/vynkor-client-android) | You want your phone as a device. Pairs by QR, streams mic/speaker, exposes phone capabilities. |

> One config `~/.config/vyn/config.yaml` for both `vyn` and `vynm`. Plugins live in `~/.local/lib/vyn/plugins/`, state in `~/.local/share/vyn/`, socket at `$XDG_RUNTIME_DIR/vyn.sock`. No legacy paths — fresh install uses only `vyn` paths (see [`docs/VYN_PRODUCT_LAYOUT.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/VYN_PRODUCT_LAYOUT.md)).

---

## What you can do today — `0.1.0`

- **Run plugins in isolation.** Supervised, auto-restart, resource limits (`512 MiB` / `1024` pids, cgroup `pids.max` + PID-namespace per plugin on Linux). One crash never takes the host down.
- **Use any language.** `cargo add vynkor-sdk` / `pip install vynkor-sdk` — streaming actions (`ActionRequestChunk`/`ResponseChunk`), `publish_event`, `SessionClose` — same in Rust/Python/C++.
- **Connect a phone in 10 seconds.** `vyn device connect --name my-phone` → QR with TLS cert pinning + encrypted `device_secret` (`AES-256-GCM`). `vyn devices`, `vyn device revoke <id>`, per-device JWT (`aud`/`jti`/`exp`).
- **Install from a signed marketplace.** `vynm` fetches `registry.json`, verifies Ed25519 + sha256, checks kernel compat, writes `plugins.d/<slug>.yaml` drop-ins. Offline cache, no kernel marketplace code.
- **Stay local by default, remote when you want.** `role: host` + Tailscale/headscale overlay — zero kernel change — or `role: client` bridge that mirrors local plugins as `device.<cap>`.

---

## Get started in 60 seconds

```bash
git clone https://github.com/vynkor-core/vynkor && cd vynkor
cargo build --release                 # → target/release/vyn
./target/release/vyn start --foreground --debug  # foreground blocks; use another shell for next commands
# in another shell:
./target/release/vyn status
# vynm is the plugin manager (separate repo):
git clone https://github.com/vynkor-core/vynkor-manager && cd vynkor-manager
cargo build --release                 # → target/release/vynm
./target/release/vynm search ai
./target/release/vynm install ai network database
./target/release/vyn device connect --name my-phone    # → QR, scan with Android app
```

**Next:** open [`vynkor-web`](https://github.com/vynkor-core/vynkor-web) or `vyn device list`, then from any plugin:

```rust
client.send_action("ai", "chat_completion", json!({ "prompt": "hello" })).await
```

---

## Write a plugin

```rust
use vynkor_sdk::{Plugin, VynkorClient, VynkorError};
use vynkor_wire::proto::vynkor::{Envelope, PluginManifest};

struct MyPlugin;

impl Plugin for MyPlugin {
    fn id(&self) -> &str { "my-plugin" }
    fn manifest(&self) -> PluginManifest { PluginManifest::default() }

    async fn on_init(&mut self, _c: &mut VynkorClient) -> Result<(), VynkorError> { Ok(()) }
    async fn on_message(&mut self, _env: Envelope) -> Result<Option<Envelope>, VynkorError> {
        Ok(None) // return Some(envelope) to reply
    }
    async fn on_shutdown(&mut self) -> Result<(), VynkorError> { Ok(()) }
}

// MyPlugin.run().await — reads $VYN_SOCKET_PATH, registers, serves
```

SDKs: [`vynkor-sdk`](https://github.com/vynkor-core/vynkor-sdk) · [`vynkor-sdk-cpp`](https://github.com/vynkor-core/vynkor-sdk-cpp) · [`vynkor-sdk-python`](https://github.com/vynkor-core/vynkor-sdk-python) — all support streaming actions and events.

| Package | Registry |
|---|---|
| `vynkor-sdk` | [crates.io](https://crates.io/crates/vynkor-sdk) (Rust) |
| `vynkor-wire` | [crates.io](https://crates.io/crates/vynkor-wire) (wire types) |
| `vynkor-sdk` (Python, module `vynkor`) | [PyPI](https://pypi.org/project/vynkor-sdk/) |

---

## Security in one paragraph

TLS on by default (auto self-signed `vyn-tls/`), `Sec-WebSocket-Protocol: vynkor, <jwt>` auth, per-device credentials instead of sharing the master `jwt_secret`, HMAC-SHA256 per frame after registration, default-deny `ipc_targets`, same-user IPC, sandboxed processes. Details: [`docs/THREAT_MODEL.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/THREAT_MODEL.md), [`docs/FRAMING.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/FRAMING.md) (flag table), [`AUDIT.md`](https://github.com/vynkor-core/vynkor/blob/main/AUDIT.md).

---

## What is coming next

Built for humans, not CLIs — the next wave is **zero-terminal onboarding and real-time UX**:

- **One-tap pairing.** Single-use ticket QR (`v:2 {ws, ticket}`, 5-min TTL) — friend scans, kernel mints their JWT. Old QR still works.
- **Real streaming.** Token-by-token `ai` stream + `cancel` stops billing, not just the typewriter.
- **Announced models & agents.** `ai` lists `models[]`/`agents[]`, phone caches per profile — honest `unavailable` instead of guessing.
- **Hands-free.** Wake-word on phone → assistant session <300 ms, `partial_transcript` → `turn_end` → `tts_interrupt`, mic → STT → capability or chat → TTS without opening chat.
- **Trust you can see.** `capability_used {cap, ts, origin}` back to your phone.
- **Hygiene.** Version negotiation, honest `device offline`, cert fingerprint in QR, per-device quota on `ai.chat`.

Details and repo split per task: [`docs/CLIENT_DRIVEN_SPLIT.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/CLIENT_DRIVEN_SPLIT.md) + `docs/tasks/CD-00..CD-09`.

---

## Project layout

```
src/
  kernel/  orchestrator + shutdown
  api/     REST + WebSocket gateway
  auth/    JWT + HMAC + permissions
  ipc/     UDS framing + routing
  plugins/ loader + registry + supervisor + sandbox
  events/  bus + at-least-once store
  bridge/  role: client WS bridge
  cli/     vyn
  utils/   config, logging, tls
docs/
  VYN_PRODUCT_LAYOUT.md  paths & config discovery (authoritative)
  FRAMING.md             flag bits + MAC interaction
  PLUGIN_REGISTRY_SCHEMA.md  registry.json / plugin.json
  CLIENT_DRIVEN_SPLIT.md client wave split by repo
```

---

## References

- Frame format: [`docs/FRAMING.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/FRAMING.md)
- Registry schema: [`docs/PLUGIN_REGISTRY_SCHEMA.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/PLUGIN_REGISTRY_SCHEMA.md)
- Product layout: [`docs/VYN_PRODUCT_LAYOUT.md`](https://github.com/vynkor-core/vynkor/blob/main/docs/VYN_PRODUCT_LAYOUT.md)
- Protocol: [`vynkor-wire/proto/vynkor_protocol.proto`](https://github.com/vynkor-core/vynkor-wire/blob/main/proto/vynkor_protocol.proto) (single source of truth)

### Unpublished crates

Between releases `Cargo.toml` may carry a `patch.crates-io` override to pull `vynkor-wire`/`vynkor-sdk` from a local path (`../vynkor-wire`, `../vynkor-sdk`). The version requirement stays; the patch just swaps the source. Drop it once published.

---

## License

MIT or Apache-2.0, at your option — `LICENSE-MIT` / `LICENSE-APACHE`.
