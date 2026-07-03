# veyron-sdk

Rust SDK for writing [Veyron](https://github.com/mrsolusdev/veyron) plugins.

A Veyron plugin is a separate OS process supervised by the Veyron kernel. It
talks to the kernel over a Unix domain socket using the Veyron wire protocol:
44-byte framed messages carrying Protobuf envelopes, with optional zstd
compression, HMAC-SHA256 frame authentication, and fragmentation.

## Quick start

```rust
use veyron_sdk::{Plugin, VeyronClient, VeyronError};
use veyron_sdk::proto::{envelope, ActionResponse, ActionStatus, Envelope, PluginManifest};

struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn id(&self) -> &str {
        "echo"
    }

    fn manifest(&self) -> PluginManifest {
        PluginManifest::default()
    }

    async fn on_message(&mut self, envelope: Envelope) -> Result<Option<Envelope>, VeyronError> {
        match envelope.payload {
            Some(envelope::Payload::ActionRequest(req)) => Ok(Some(Envelope {
                payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                    action_id: req.action_id,
                    status: ActionStatus::ActionOk as i32,
                    data_json: req.params_json,
                    error: String::new(),
                })),
                ..Default::default()
            })),
            _ => Ok(None),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), VeyronError> {
    EchoPlugin.run().await
}
```

`Plugin::run` connects, registers, and serves until the kernel asks the plugin
to shut down. The SDK answers `Ping` automatically, acknowledges delivered
events after `on_event` succeeds, and exits the loop on `PluginShutdown`.

## Environment

| Variable             | Meaning                                                        |
|----------------------|----------------------------------------------------------------|
| `VEYRON_SOCKET_PATH` | Kernel UDS path. Default: `XDG_RUNTIME_DIR` → `/run/user/<uid>` → `~/.veyron/run` (never shared `/tmp`). |
| `VEYRON_JWT_TOKEN`   | JWT presented at registration (required on secured kernels).   |
| `VEYRON_JWT_SECRET`  | Shared secret; enables per-frame HMAC-SHA256 tags after registration. |

## Protocol coverage

The SDK re-exports the kernel framing layer (`veyron_sdk::framing`), so the
wire format cannot drift between the two sides. All flag bits from
`docs/FRAMING.md` are handled:

| Flag               | Send                                             | Receive                                    |
|--------------------|--------------------------------------------------|--------------------------------------------|
| `FLAG_MAC_PRESENT` | automatic after secured registration             | verified; untagged frames rejected         |
| `FLAG_COMPRESSED`  | automatic for payloads ≥ 64 KiB                  | decompressed + normalized by `read_frame`  |
| `FLAG_FRAGMENTED`  | `VeyronClient::send_fragmented`                  | reassembled by `recv`/`recv_frame` (64 streams, 1 MiB, 30 s bounds) |
| `FLAG_RAW_BINARY`  | `VeyronClient::send_raw_audio`                   | returned raw by `recv_frame`               |

## Client API

For lower-level control, use `VeyronClient` directly:

```rust,ignore
let mut client = VeyronClient::connect_with_secret(&socket, secret).await?;
let ack = client.register_with_token("weather", manifest, &jwt).await?;

client.subscribe(vec!["alarm.fired".into()]).await?;
let resp = client.send_action("get_weather", br#"{"city":"Berlin"}"#, 5_000).await?;
let latency = client.ping().await?;
```

Requests and responses are matched on a single connection; drive
request/response traffic from one task, or use the `Plugin` trait's serve
loop.

## License

MIT
