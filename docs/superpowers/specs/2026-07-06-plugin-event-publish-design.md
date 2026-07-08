# Plugin → Event-Bus Publish Path Design

Date: 2026-07-06 (revised 2026-07-08)

> **Supersedes the original 2026-07-06 version of this file.** That version
> reused the `Event` message for both directions, denied `system.*` via a
> hardcoded string-prefix check in kernel code, and used `EVENT_PUBLISH_OK =
> 0`. This revision fixes three issues found on review, none of which were
> ever implemented (`git log` shows only the spec commit, `e137bdb`, no
> follow-up code):
> 1. `EVENT_PUBLISH_OK = 0` is the exact zero-value footgun called out as
>    T-16 in `ROADMAP.md` (`ACTION_OK = 0` / `COMMAND_OK = 0`) — a missed
>    `set_status()` would silently read as success. Fixed by giving every new
>    status enum a `*_UNKNOWN = 0` per the convention every other enum in the
>    proto except the two T-16 flags already follows.
> 2. A hardcoded `event_type.starts_with("system.")` check embeds a business
>    naming convention directly in `src/ipc/protocol.rs` — the manifesto
>    states kernel is a "dumb byte router... zero business logic." Fixed by
>    structural auto-namespacing (below): the kernel mechanically prepends
>    the publisher's own `plugin_id`, so a plugin cannot produce a
>    `system.*` event type at all — not because the kernel recognizes and
>    denies that string, but because the kernel never lets a plugin's output
>    land outside its own namespace. No domain knowledge, no denylist.
> 3. Reusing `Event` (today strictly kernel→plugin, `retry_count` is
>    kernel-owned per its own doc comment) for the plugin→kernel direction
>    too overloads one message with two different trust boundaries. A
>    dedicated `EventPublish` message keeps the existing `Event` message's
>    contract (kernel-authored, redelivery-tracked) intact and makes the new
>    inbound surface explicit in the `oneof`, the same way `ActionRequest`/
>    `ActionResponse` are separate messages rather than one `Action` reused
>    both ways.

## Goal

Let a plugin push an event into the kernel's event bus (`EventBus::publish`,
`src/events/bus.rs`), so other subscribed plugins receive it the same way
they receive kernel-originated events (e.g. `system.plugin_joined`).
Currently `EventBus::publish` is only called from kernel-internal code
(`src/ipc/protocol.rs`, `src/kernel/orchestrator.rs`,
`src/plugins/supervisor.rs`) — there's no wire path for a plugin to publish.

This is ROADMAP.md R6-01. Immediate driver: the `network` plugin
(`docs/superpowers/specs/2026-07-05-network-plugin-design.md`) wants to emit
a `request_completed` event (status, host, latency_ms, retry_count) as a real
event instead of stdout-only logging, so other plugins can subscribe to it.

## Scope

- v1: kernel wire protocol + Rust SDK (`sdk/rust`) helper method.
- Out of scope for v1: Python/C++ SDK helpers — follow-up once a Python or
  C++ plugin actually needs to publish. Wire format is language-agnostic, so
  nothing here blocks adding them later.

## Wire protocol changes (`wire/proto/veyron_protocol.proto`,
mirrored into `sdk/cpp/proto/` and `sdk/python/proto/` per T-17's drift check)

New messages, permission, and `Envelope` oneof fields:

```protobuf
message EventPublish {
  string event_type   = 1;  // plugin's own sub-namespace, e.g. "request_completed"
  bytes  payload_json  = 2;
}

message EventPublishAck {
  string             event_id = 1;  // kernel-assigned, "plugin.<sender_id>.<event_type>"-scoped
  EventPublishStatus status   = 2;
  string             error    = 3;
}

enum EventPublishStatus {
  EVENT_PUBLISH_UNKNOWN         = 0;  // never explicitly set; a missed set_status() shows up as this, not OK
  EVENT_PUBLISH_OK              = 1;
  EVENT_PUBLISH_ERROR           = 2;
  EVENT_PUBLISH_PERMISSION_DENY = 3;
}

enum PermissionType {
  ...
  PERMISSION_EVENT_PUBLISH = 13; // publish events to the kernel event bus
}
```

```protobuf
message Envelope {
  oneof payload {
    ...
    EventPublish      event_publish      = 44;
    EventPublishAck   event_publish_ack  = 45;
  }
}
```

No changes to the existing `Event`/`EventAck`/`Subscribe`/`Unsubscribe`
messages — `Event.retry_count` stays exclusively kernel-owned, exactly as
its current doc comment says.

## Kernel handling (`src/ipc/protocol.rs`)

New arm, positioned next to the existing `Subscribe`/`Unsubscribe` handling:

```rust
Some(envelope::Payload::EventPublish(req)) => {
    let sender_id = registry
        .get_by_conn_id(msg.conn_id)
        .map(|e| e.plugin_id.clone())
        .unwrap_or_default();

    let (status, event_id) = if check_permission(
        registry,
        &sender_id,
        PermissionType::PermissionEventPublish,
    )
    .is_err()
    {
        (EventPublishStatus::EventPublishPermissionDeny, String::new())
    } else {
        // Structural namespacing: the kernel does not inspect or recognize
        // any business prefix (no "system." denylist). A plugin's published
        // event always lands under its own plugin_id — it is mechanically
        // impossible to land outside that namespace, so kernel-owned event
        // types (e.g. "system.plugin_joined", published only via the
        // separate internal EventBus::publish call sites) can never collide
        // with or be spoofed by anything reachable from this handler.
        let event_id = format!("evt-{}-{}", sender_id, req_uuid());
        let event_type = format!("plugin.{sender_id}.{}", req.event_type);
        event_bus
            .publish(
                Event {
                    event_id: event_id.clone(),
                    event_type,
                    payload_json: req.payload_json,
                    retry_count: 0,
                },
                registry,
            )
            .await;
        (EventPublishStatus::EventPublishOk, event_id)
    };

    let ack = Envelope {
        payload: Some(envelope::Payload::EventPublishAck(EventPublishAck {
            event_id,
            status: status as i32,
            error: String::new(),
        })),
        ..Default::default()
    };
    Self::send_envelope(&msg.write_tx, ack).await;
    false
}
```

`EVENT_PUBLISH_ERROR` is reserved for future internal failure modes (e.g. if
`EventBus::publish` ever becomes fallible) — v1 has no path that produces it,
same as how `ACTION_ERROR` exists in `ActionStatus` without every action
handler using it.

## Manifest

```rust
PluginManifest {
    permissions: vec!["PERMISSION_EVENT_PUBLISH".into()],
    ...
}
```

No new manifest field. A plugin requests the permission the same way it
requests `PERMISSION_NETWORK` etc. today; `PluginRegisterAck.granted_permissions`
already reports back what was actually granted. No per-event-type manifest
declaration is needed — structural namespacing already bounds what a plugin
can publish to its own `plugin.<id>.*` space, so there is nothing further to
declare or authorize per event type.

## Rust SDK (`sdk/rust/src/client.rs`)

New method mirroring the existing `send_action` (`sdk/rust/src/client.rs:463`),
reusing its `next_request_id` + manual deadline-loop pattern rather than
introducing a new correlation mechanism:

```rust
pub async fn publish_event(
    &mut self,
    event_type: &str,
    payload_json: &[u8],
    timeout_ms: u32,
) -> Result<EventPublishAck, VeyronError> {
    let env = Envelope {
        payload: Some(envelope::Payload::EventPublish(EventPublish {
            event_type: event_type.to_string(),
            payload_json: payload_json.to_vec(),
        })),
        ..Default::default()
    };
    self.send("kernel", env).await?;

    let timeout = if timeout_ms == 0 {
        DEFAULT_REQUEST_TIMEOUT
    } else {
        Duration::from_millis(timeout_ms as u64)
    };
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(VeyronError::Timeout);
        }
        let response = self.recv_timeout(remaining).await?;
        match response.payload {
            Some(envelope::Payload::EventPublishAck(ack)) => return Ok(ack),
            Some(envelope::Payload::Error(err)) => {
                return Err(VeyronError::Internal(format!(
                    "kernel error: {} ({})",
                    err.message, err.details
                )));
            }
            _ => continue, // unrelated traffic while waiting
        }
    }
}
```

Unlike `send_action`, there's no client-generated `event_id` to correlate
against — the kernel assigns it. A connection publishing concurrently before
this SDK method returns would need request pipelining to disambiguate acks;
out of scope for v1 (same limitation `send_action` already has today).

## Error handling

- `EVENT_PUBLISH_PERMISSION_DENY` — plugin lacks `PERMISSION_EVENT_PUBLISH`.
  This is the only denial path in v1; there is no namespace-rejection status
  because there is no namespace check — structural prepending makes
  rejection unnecessary rather than adding a second denial reason to
  distinguish from the first.
- Empty `event_type` from the plugin is allowed through as-is (becomes
  `plugin.<id>.`) — not worth a dedicated validation error for v1; a
  malformed but harmless event type only confuses the publishing plugin's
  own subscribers, not the kernel.

## Testing

Integration tests in `tests/integration/test_kernel_commands.rs`, mirroring
`kernel_denies_action_when_provider_lacks_required_permission`:

1. Plugin without `PERMISSION_EVENT_PUBLISH` publishes any event →
   `EVENT_PUBLISH_PERMISSION_DENY`, no subscriber receives it.
2. Plugin with the permission publishes `event_type: "request_completed"`
   from plugin id `network` → `EVENT_PUBLISH_OK`, and a separate subscriber
   plugin subscribed to `Subscribe{event_types: ["plugin.network.request_completed"]}`
   receives the `Event` with matching `payload_json` and `retry_count == 0`.
3. Two different plugins (`network`, `weather`) both publish
   `event_type: "request_completed"` → land on distinct wire event types
   (`plugin.network.request_completed` vs `plugin.weather.request_completed`),
   each only reaching subscribers of its own namespaced type (or `"*"`).
4. A plugin cannot produce a `system.*`-prefixed wire event type through
   `EventPublish` under any permission grant — assert this by construction
   (the handler always prepends `plugin.<sender_id>.`, so there's no input
   that reaches `system.*`) rather than as a runtime denial test.

## Non-goals / follow-ups

- Python/C++ SDK `publish_event()` helpers — follow-up once needed.
- Per-event-type authorization finer than the blanket permission — not
  applicable in this design; structural namespacing already scopes a plugin
  to its own `plugin.<id>.*` space without a separate declaration surface.
- Rate limiting / quota on publish volume — separate from R6-03 (per-caller
  action quotas), not addressed here.
