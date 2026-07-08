# Non-blocking IPC router sends (T-03)

**Status:** approved 2026-07-08
**Roadmap item:** `ROADMAP.md` T-03 (AUDIT H-03)

## Problem

`MessageRouter::run_with_context` (`src/ipc/protocol.rs`) is a single loop pulling every `IncomingMessage` off one `mpsc::Receiver` and processing them one at a time — kernel commands, unicast forwards, broadcasts, all serialized through this one task.

Two send paths block that loop on a per-target write:

- `forward()` (`src/ipc/protocol.rs:699`): `timeout(WRITE_SEND_TIMEOUT, entry.write_tx.send(...)).await` — up to 50ms if the target's channel is full.
- `broadcast()` (`src/ipc/protocol.rs:776`): the same `timeout(...).await`, called once per subscriber in a loop — up to `N × 50ms` for N plugins.
- `EventBus::deliver()` (`src/events/bus.rs`): same pattern, once per event subscriber.

A single non-draining plugin (crashed, stalled, slow to read its socket) stalls routing for every other plugin on the kernel for the duration of the timeout, repeated on every message sent its way. `broadcast`/event fan-out multiplies this by subscriber count.

## Non-goals

- No change to per-connection channel capacity (`mpsc::channel::<Outbound>(64)` in `src/ipc/connection.rs`) — this fix doesn't touch backpressure sizing, only how the router reacts to a full channel.
- No change to delivery semantics beyond timing: sends were already best-effort/fire-and-forget (drop-and-log on failure, no delivery guarantee, no caller notified). This spec keeps that contract, it just changes how quickly a full channel is detected.
- No retry queue. Considered and rejected — see Alternatives.

## Design

Replace every blocking `timeout(WRITE_SEND_TIMEOUT, tx.send(...)).await` on the router/event-bus hot path with a non-blocking `tx.try_send(...)`:

- `Ok(())` — delivered, same as today's success path.
- `Err(TrySendError::Full(_))` — target's channel is saturated. Drop the frame, log + increment the existing `ipc_forward_timeouts_total` / `events_dropped_total{reason="slow_subscriber"}` counters (renamed in spirit, not necessarily in metric name — see Implementation notes) exactly as the current "timed out" branch does.
- `Err(TrySendError::Closed(_))` — receiver gone (plugin disconnecting). Same as today's `entry.write_tx.send(...)` `Err` branch — drop, log, increment `channel_closed` counter.

This removes `WRITE_SEND_TIMEOUT` and the `tokio::time::timeout` wrapper entirely from these three call sites. The router loop, and `EventBus::deliver`, now spend microseconds per target regardless of how full or stalled that target's channel is — a stalled plugin can no longer hold up delivery to anyone else, because there is no `.await` point that depends on the target draining.

### Why not spawn a task per send instead

The roadmap listed this as the other option. Rejected: `entry.write_tx` is a single bounded `mpsc::Sender` per connection (capacity 64, `src/ipc/connection.rs:135`). Today, all sends to one target are issued sequentially by the single router task, so delivery order into that channel matches the order the router decided to send them. If instead each send were handed to `tokio::spawn`, multiple in-flight tasks targeting the *same* connection would race to call `.send()` — the order frames land in the channel would depend on tokio's scheduler, not on the order the router processed them in. For a single connection's frame stream (sequencing/MAC state is per-connection and order-dependent, `docs/FRAMING.md`), out-of-order delivery is a protocol correctness bug, not a latency tradeoff. `try_send` avoids this because it's synchronous and still issued in loop order from the one router task.

### Why not a bounded retry queue

Considered: instead of dropping on `Full`, push to a small per-target overflow queue and retry on the next prune tick. Rejected as unnecessary complexity for this fix — delivery here has never been guaranteed (both `forward`'s and `broadcast`'s existing doc comments call this "not the sender's fault" / fire-and-forget), and a retry queue adds new state (queue caps, eviction policy, ordering vs. fresh sends arriving concurrently) to solve a problem the protocol doesn't require solving. If a future need for guaranteed delivery arises, that's an `EventStore`-style persistence layer (which events already have via `EventStore::persist`/retry worker), not a router-level queue.

## Call sites changed

- `src/ipc/protocol.rs` `forward()` — one `try_send` in place of `timeout(...).await`.
- `src/ipc/protocol.rs` `broadcast()` — one `try_send` per subscriber in place of `timeout(...).await`.
- `src/events/bus.rs` `deliver()` — one `try_send` per subscriber in place of `timeout(...).await`.
- `WRITE_SEND_TIMEOUT` (`src/ipc/protocol.rs:34`) and `EVENT_SEND_TIMEOUT` (`src/events/bus.rs:16`) constants removed — no longer referenced anywhere.

No proto changes, no SDK changes, no config changes.

## Testing

- Existing tests asserting drop-on-full-channel behavior (`tests/unit/test_router.rs`, event bus tests) should continue to pass with `try_send` — a full channel still results in a dropped frame and an incremented counter, just detected immediately instead of after 50ms.
- New test: fill a target's write channel to capacity (send without draining), then have another plugin `forward()`/`broadcast()` to it, assert the call returns near-instantly (no 50ms+ wall-clock delay) and the frame is dropped with the counter incremented.
- New test (broadcast specifically): with one saturated subscriber among several healthy ones, assert the healthy subscribers still receive their frame and the whole `broadcast()` call completes in on the order of microseconds, not `N × 50ms`.

## Definition of Done

- `cargo test --all --all-features` exits 0, including the two new tests above.
- `cargo clippy --all-targets --all-features -- -D warnings` clean.
- `ROADMAP.md` T-03 marked done with a summary line matching the style of the other landed items.
