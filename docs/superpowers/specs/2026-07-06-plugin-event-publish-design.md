# Plugin → Event-Bus Publish Path Design

Date: 2026-07-06

## Goal

Let a plugin push an event into the kernel's event bus (`EventBus::publish`,
`src/events/bus.rs`), so other subscribed plugins receive it the same way
they receive kernel-originated events (e.g. `system.plugin_joined`).
Currently `EventBus::publish` is only called from kernel-internal code
(`src/ipc/protocol.rs`, `src/kernel/orchestrator.rs`,
`src/plugins/supervisor.rs`) — there's no wire path for a plugin to publish.

This is ROADMAP.md R6-01. Immediate driver: the `network` plugin
(`docs/superpowers/specs/2026-07-05-network-plugin-design.md`) wants to emit
`network.request_completed` (status, host, latency_ms, retry_count) as a real
event instead of stdout-only logging, so other plugins can subscribe to it.

## Scope

- v1: kernel wire protocol + Rust SDK (`sdk/rust`) helper method.
- Out of scope for v1: Python/C++ SDK helpers — noted as a follow-up, added
  once a Python or C++ plugin actually needs to publish. Nothing in this
  design blocks adding them later; the wire format is language-agnostic.

## Wire protocol changes (`wire/proto/veyron_protocol.proto`)

No new `Event` message — the existing `Event` message (already used
kernel→plugin) is now legal in the plugin→kernel direction too, the same way
`Subscribe`/`Unsubscribe` are plugin→kernel-only today. The kernel's inbound
envelope handler in `src/ipc/protocol.rs` gains a
`Some(envelope::Payload::Event(event))` arm (there currently is none, since
nothing sends `Event` inbound) alongside the existing `Subscribe`/
`Unsubscribe` arms.

New permission:

```protobuf
enum PermissionType {
  ...
  PERMISSION_EVENT_PUBLISH = 13; // publish events to the kernel event bus
}
```

New ack message and status, and a new `Envelope` oneof field:

```protobuf
message EventPublishAck {
  string             event_id = 1;
  EventPublishStatus status   = 2;
}

enum EventPublishStatus {
  EVENT_PUBLISH_OK     = 0;
  EVENT_PUBLISH_DENIED = 1; // missing PERMISSION_EVENT_PUBLISH, or system.* namespace
}
```

```protobuf
message Envelope {
  oneof payload {
    ...
    EventPublishAck event_publish_ack = 44;
  }
}
```

`Event.retry_count` remains kernel-owned (doc comment: "kernel fills this in
on redelivery") — the kernel zeroes it on any inbound publish before handing
the event to `EventBus::publish`, regardless of what the plugin sent.

## Kernel handling (`src/ipc/protocol.rs`)

New arm, positioned next to the existing `Subscribe`/`Unsubscribe` handling:

```rust
Some(envelope::Payload::Event(mut event)) => {
    let sender_id = registry
        .get_by_conn_id(msg.conn_id)
        .map(|e| e.plugin_id.clone())
        .unwrap_or_default();

    let status = if event.event_type.starts_with("system.") {
        // Kernel-owned namespace — never spoofable, permission or not.
        EventPublishStatus::EventPublishDenied
    } else if check_permission(registry, &sender_id, PermissionType::PermissionEventPublish).is_err() {
        EventPublishStatus::EventPublishDenied
    } else {
        event.retry_count = 0; // kernel-owned field, ignore whatever the plugin sent
        event_bus.publish(event.clone(), registry).await;
        EventPublishStatus::EventPublishOk
    };

    let ack = Envelope {
        payload: Some(envelope::Payload::EventPublishAck(EventPublishAck {
            event_id: event.event_id,
            status: status as i32,
        })),
        ..Default::default()
    };
    Self::send_envelope(&msg.write_tx, ack).await;
    false
}
```

The `system.*` check runs *before* the permission check and is not
bypassable by any grant — this matches the manifesto's "kernel = source of
truth for its own lifecycle events" stance and mirrors how `ACTION_PERMISSION_DENY`
already gates `ActionRequest` routing (`src/auth/permissions.rs`,
`required_permission_for_action`) without adding a second declaration
surface (no new `PluginManifest` field — a plugin either has
`PERMISSION_EVENT_PUBLISH` or it doesn't; it may publish any non-`system.*`
type once granted, same shape as `PERMISSION_NETWORK` gating `http_request`).

## Manifest

```rust
PluginManifest {
    permissions: vec!["PERMISSION_EVENT_PUBLISH".into()],
    ...
}
```

No new manifest field. A plugin requests the permission the same way it
requests `PERMISSION_NETWORK` etc. today; `PluginRegisterAck.granted_permissions`
already reports back what was actually granted.

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
    let event_id = next_request_id("evt");
    let env = Envelope {
        payload: Some(envelope::Payload::Event(Event {
            event_id: event_id.clone(),
            event_type: event_type.to_string(),
            payload_json: payload_json.to_vec(),
            retry_count: 0,
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
            Some(envelope::Payload::EventPublishAck(ack)) if ack.event_id == event_id => {
                return Ok(ack);
            }
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

## Error handling

- `EVENT_PUBLISH_DENIED` covers both denial reasons (missing permission,
  `system.*` namespace) — the ack doesn't need to distinguish them further
  for v1; a plugin author sees denial and checks its manifest/event name.
  (Matches `ACTION_PERMISSION_DENY`'s level of detail.)
- Malformed `event_type` (empty string) is also denied — treated the same
  as `system.*`-prefixed (falls under "not a valid publishable type").

## Testing

Integration tests in `tests/integration/test_kernel_commands.rs`, mirroring
`kernel_denies_action_when_provider_lacks_required_permission`:

1. Plugin without `PERMISSION_EVENT_PUBLISH` publishes any event →
   `EVENT_PUBLISH_DENIED`, no subscriber receives it.
2. Plugin *with* `PERMISSION_EVENT_PUBLISH` publishes `system.fake_event` →
   still `EVENT_PUBLISH_DENIED` (namespace block wins over grant).
3. Plugin with the permission publishes `network.request_completed` →
   `EVENT_PUBLISH_OK`, and a separate subscriber plugin (subscribed via
   `Subscribe{event_types: ["network.request_completed"]}`) receives the
   `Event` with matching `payload_json` and `retry_count == 0`.

## Non-goals / follow-ups

- Python/C++ SDK `publish_event()` helpers — follow-up once needed.
- Per-event-type authorization finer than the blanket permission (e.g. a
  declared `published_events` allowlist per plugin, mirroring the `actions`
  manifest field) — explicitly rejected for v1 as unnecessary surface; can
  be added later without a breaking wire change if a real need shows up.
- Rate limiting / quota on publish volume — separate from R6-03 (per-caller
  action quotas), not addressed here.
