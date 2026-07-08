# Non-blocking IPC Router Sends (T-03) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the router's and event bus's blocking `timeout(50ms, tx.send(...)).await` fan-out sends with non-blocking `tx.try_send(...)`, so one stalled plugin can no longer delay delivery to every other plugin.

**Architecture:** `MessageRouter::forward`/`MessageRouter::broadcast` (`src/ipc/protocol.rs`) and `EventBus::deliver` (`src/events/bus.rs`) currently `.await` a bounded timeout on each per-target `mpsc::Sender<Outbound>::send`. Swap each to `mpsc::Sender::try_send`, which returns immediately with `Ok(())`, `Err(TrySendError::Full(_))`, or `Err(TrySendError::Closed(_))` — no `.await` point tied to the target draining. Same drop-and-count-a-metric behavior on failure, just detected synchronously instead of after up to 50ms.

**Tech Stack:** Rust, Tokio (`tokio::sync::mpsc`), existing `metrics`/`tracing` crates already in use.

## Global Constraints

- No proto changes, no SDK changes, no config changes (per spec Non-goals).
- No change to channel capacity (`mpsc::channel::<Outbound>(64)` in `src/ipc/connection.rs`) or to delivery-guarantee semantics — sends remain best-effort/fire-and-forget.
- No retry queue (per spec, rejected as unneeded complexity).
- `cargo test --all --all-features` must exit 0; `cargo clippy --all-targets --all-features -- -D warnings` and `cargo fmt --check` must be clean before the final commit.

---

### Task 1: `forward()` uses `try_send`

**Files:**
- Modify: `src/ipc/protocol.rs:697-707` (the `match registry.get(plugin_id)` `Some` arm's send call), `src/ipc/protocol.rs:25` (remove now-unused `use tokio::time::timeout;` — deferred to Task 4 once all three call sites are converted), `src/ipc/protocol.rs:34` (remove `WRITE_SEND_TIMEOUT` const — deferred to Task 4)
- Test: `tests/unit/test_router.rs`

**Interfaces:**
- Consumes: `PluginRegistry::get`, `PluginEntry.write_tx: mpsc::Sender<Outbound>` (existing), `out_frame(Frame) -> Outbound` (existing, `src/ipc/connection.rs`).
- Produces: no new public interface — internal behavior change only.

- [ ] **Step 1: Write the failing test**

Add to `tests/unit/test_router.rs` (after the existing `slow_target_does_not_stall_router` test). This times two successive forwards to the same full channel — if `forward()` still blocks on the 50ms timeout internally, the round trip takes >= 100ms:

```rust
#[tokio::test]
async fn forward_to_full_channel_returns_without_waiting() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    // stuck target: capacity-1 channel, pre-filled and never drained.
    let (stuck_tx, _stuck_rx) = mpsc::channel::<Outbound>(1);
    stuck_tx
        .send(out_frame(make_frame("x", b"prefill".to_vec())))
        .await
        .unwrap();
    reg.register("stuck".to_string(), 2, dummy_manifest(), stuck_tx)
        .unwrap();

    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "sender".to_string(),
        1,
        ipc_manifest_with_targets(vec!["stuck"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let start = std::time::Instant::now();
    router_tx
        .send(incoming(1, plug_frame("stuck", b"to-stuck".to_vec()), a_tx.clone()))
        .await
        .unwrap();

    // Sending a second message to the SAME router channel and waiting for the
    // router to accept it (mpsc send completing) proves the router's internal
    // loop iteration for "stuck" has finished — if forward() blocked 50ms
    // internally, this whole round trip takes >= 50ms.
    router_tx
        .send(incoming(1, plug_frame("stuck", b"to-stuck-2".to_vec()), a_tx))
        .await
        .unwrap();

    assert!(
        start.elapsed() < Duration::from_millis(20),
        "two forwards to a full channel must not block the router on the 50ms \
         send timeout, took {:?}",
        start.elapsed()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test unit forward_to_full_channel_returns_without_waiting -- --nocapture`
Expected: FAIL — elapsed is ~100ms (two 50ms blocking timeouts), assertion `< 20ms` fails.

- [ ] **Step 3: Implement `try_send` in `forward()`**

In `src/ipc/protocol.rs`, replace (around line 697-707):

```rust
                // Bounded send: a slow target must not block the router. Dropping
                // one frame for a non-draining plugin is not the sender's fault, so
                // this is not counted against the sender's error budget.
                if timeout(WRITE_SEND_TIMEOUT, entry.write_tx.send(out_frame(frame)))
                    .await
                    .is_err()
                {
                    warn!(target = %plugin_id, "forward timeout: slow target, frame dropped");
                    counter!("ipc_forward_timeouts_total").increment(1);
                }
```

with:

```rust
                // Non-blocking send: a slow/full target must not block the router.
                // Dropping one frame for a non-draining plugin is not the sender's
                // fault, so this is not counted against the sender's error budget.
                if entry.write_tx.try_send(out_frame(frame)).is_err() {
                    warn!(target = %plugin_id, "forward: target channel full, frame dropped");
                    counter!("ipc_forward_timeouts_total").increment(1);
                }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test unit forward_to_full_channel_returns_without_waiting -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the full existing router test suite to check for regressions**

Run: `cargo test --test unit --lib router`
Expected: all pass, including `slow_target_does_not_stall_router`, `forward_strips_flag_mac_present`.

- [ ] **Step 6: Commit**

```bash
git add src/ipc/protocol.rs tests/unit/test_router.rs
git commit -m "fix: forward() uses non-blocking try_send instead of 50ms timeout (T-03)"
```

---

### Task 2: `broadcast()` uses `try_send`

**Files:**
- Modify: `src/ipc/protocol.rs:776-785` (the `match timeout(...)` block inside the broadcast loop)
- Test: `tests/unit/test_router.rs`

**Interfaces:**
- Consumes: same as Task 1.
- Produces: no new public interface.

- [ ] **Step 1: Write the failing test**

Add to `tests/unit/test_router.rs`:

```rust
#[tokio::test]
async fn broadcast_to_many_stuck_targets_does_not_multiply_delay() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    // Five stuck targets: each capacity-1, pre-filled, never drained.
    for i in 0..5u64 {
        let (stuck_tx, _stuck_rx) = mpsc::channel::<Outbound>(1);
        stuck_tx
            .send(out_frame(make_frame("x", b"prefill".to_vec())))
            .await
            .unwrap();
        reg.register(format!("stuck{i}"), 100 + i, dummy_manifest(), stuck_tx)
            .unwrap();
    }

    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "sender".to_string(),
        1,
        PluginManifest {
            permissions: vec!["PERMISSION_IPC_SEND".to_string()],
            ipc_targets: (0..5u64).map(|i| format!("stuck{i}")).collect(),
            ..Default::default()
        },
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let start = std::time::Instant::now();
    router_tx
        .send(incoming(1, make_frame("*", b"broadcast payload".to_vec()), a_tx))
        .await
        .unwrap();
    // Force a second round trip through the router so we know the broadcast
    // loop (which ran synchronously inside the first message's handling) has
    // fully completed by the time this second send is accepted.
    let (b_tx, _b_rx) = make_write_pair();
    reg.register("ping".to_string(), 200, dummy_manifest(), b_tx.clone())
        .unwrap();
    router_tx
        .send(incoming(200, plug_frame("kernel", vec![]), b_tx))
        .await
        .unwrap();

    assert!(
        start.elapsed() < Duration::from_millis(50),
        "broadcast to 5 stuck subscribers must not cost 5 * 50ms, took {:?}",
        start.elapsed()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test unit broadcast_to_many_stuck_targets_does_not_multiply_delay -- --nocapture`
Expected: FAIL — elapsed ~250ms (5 * 50ms), assertion `< 50ms` fails.

- [ ] **Step 3: Implement `try_send` in `broadcast()`**

In `src/ipc/protocol.rs`, replace (around line 776-785):

```rust
            match timeout(WRITE_SEND_TIMEOUT, entry.write_tx.send(out_frame(frame))).await {
                Ok(_) => {}
                Err(_) => {
                    warn!(
                        plugin_id = %entry.plugin_id,
                        "broadcast timeout: slow plugin skipped"
                    );
                    counter!("broadcast_timeouts_total").increment(1);
                }
            }
```

with:

```rust
            if entry.write_tx.try_send(out_frame(frame)).is_err() {
                warn!(
                    plugin_id = %entry.plugin_id,
                    "broadcast: target channel full, frame dropped"
                );
                counter!("broadcast_timeouts_total").increment(1);
            }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test unit broadcast_to_many_stuck_targets_does_not_multiply_delay -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the full router test suite**

Run: `cargo test --test unit --lib router`
Expected: all pass, including `router_broadcasts_star_to_all_except_sender`, `broadcast_denied_with_empty_ipc_targets`, `broadcast_strips_flag_mac_present`.

- [ ] **Step 6: Commit**

```bash
git add src/ipc/protocol.rs tests/unit/test_router.rs
git commit -m "fix: broadcast() uses non-blocking try_send instead of 50ms-per-subscriber timeout (T-03)"
```

---

### Task 3: `EventBus::deliver()` uses `try_send`

**Files:**
- Modify: `src/events/bus.rs:126-152` (the `match tokio::time::timeout(...)` block inside `deliver`'s subscriber loop)
- Test: `tests/unit/test_event_bus.rs`

**Interfaces:**
- Consumes: `PluginRegistry::get`, `PluginEntry.write_tx`, `out_frame` — same as Tasks 1-2, applied inside `EventBus::deliver`.
- Produces: no new public interface.

- [ ] **Step 1: Write the failing test**

Add to `tests/unit/test_event_bus.rs`:

```rust
#[tokio::test]
async fn publish_to_many_stuck_subscribers_does_not_multiply_delay() {
    let bus = EventBus::new();
    let registry = make_registry();

    for i in 0..5u64 {
        let (stuck_tx, _stuck_rx) =
            mpsc::channel::<veyron::ipc::connection::Outbound>(1);
        stuck_tx
            .send(veyron::ipc::connection::out_frame(empty_frame()))
            .await
            .unwrap();
        registry
            .register(format!("stuck{i}"), i, PluginManifest::default(), stuck_tx)
            .unwrap();
        bus.subscribe(&format!("stuck{i}"), vec!["e".to_string()]);
    }

    let start = std::time::Instant::now();
    bus.publish(make_event("e"), &registry).await;
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "publish to 5 stuck subscribers must not cost 5 * 50ms, took {:?}",
        start.elapsed()
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test unit publish_to_many_stuck_subscribers_does_not_multiply_delay -- --nocapture`
Expected: FAIL — elapsed ~250ms, assertion `< 50ms` fails.

- [ ] **Step 3: Implement `try_send` in `EventBus::deliver`**

In `src/events/bus.rs`, replace (around line 126-152):

```rust
                    // Bounded send: a slow subscriber must not stall the publisher.
                    match tokio::time::timeout(
                        EVENT_SEND_TIMEOUT,
                        entry.write_tx.send(out_frame(frame)),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            // receiver dropped — plugin is disconnecting
                            counter!("events_dropped_total", "reason" => "channel_closed")
                                .increment(1);
                        }
                        Err(_) => {
                            warn!(
                                plugin_id = %plugin_id,
                                event_type = %event_type,
                                "event dropped: subscriber write channel full"
                            );
                            counter!("events_dropped_total", "reason" => "slow_subscriber")
                                .increment(1);
                        }
                    }
```

with:

```rust
                    // Non-blocking send: a slow/full subscriber must not stall the
                    // publisher or any other subscriber in this fan-out loop.
                    match entry.write_tx.try_send(out_frame(frame)) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            // receiver dropped — plugin is disconnecting
                            counter!("events_dropped_total", "reason" => "channel_closed")
                                .increment(1);
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            warn!(
                                plugin_id = %plugin_id,
                                event_type = %event_type,
                                "event dropped: subscriber write channel full"
                            );
                            counter!("events_dropped_total", "reason" => "slow_subscriber")
                                .increment(1);
                        }
                    }
```

Add `use tokio::sync::mpsc;` to the top of `src/events/bus.rs` (currently the file has no top-level `mpsc` import — check with `grep -n "use tokio" src/events/bus.rs` first; if absent, add it next to the other `use` lines).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --test unit publish_to_many_stuck_subscribers_does_not_multiply_delay -- --nocapture`
Expected: PASS

- [ ] **Step 5: Run the full event bus test suite**

Run: `cargo test --test unit --lib event_bus`
Expected: all pass, including `slow_subscriber_does_not_block_publish_to_others`, `publish_delivers_event_to_subscriber`.

- [ ] **Step 6: Commit**

```bash
git add src/events/bus.rs tests/unit/test_event_bus.rs
git commit -m "fix: EventBus::deliver uses non-blocking try_send instead of 50ms-per-subscriber timeout (T-03)"
```

---

### Task 4: Remove now-unused timeout constants/imports and run full verification

**Files:**
- Modify: `src/ipc/protocol.rs` (remove `const WRITE_SEND_TIMEOUT` at line 34, remove `use tokio::time::timeout;` at line 25 if no longer referenced anywhere else in the file)
- Modify: `src/events/bus.rs` (remove `const EVENT_SEND_TIMEOUT` at line 17; keep `use std::time::Duration;` only if still used elsewhere in the file — check with `grep -n Duration src/events/bus.rs`)

- [ ] **Step 1: Remove `WRITE_SEND_TIMEOUT` and its import**

```bash
grep -n "timeout(" src/ipc/protocol.rs
```
Expected: no matches (both call sites were converted in Tasks 1-2). Then remove:

```rust
const WRITE_SEND_TIMEOUT: Duration = Duration::from_millis(50);
```
and
```rust
use tokio::time::timeout;
```
from `src/ipc/protocol.rs`. Check `Duration` is still used elsewhere in the file (it is — `ERROR_BUDGET_IDLE_TTL`, `Instant`, prune interval) so keep `use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};` as-is.

- [ ] **Step 2: Remove `EVENT_SEND_TIMEOUT`**

```bash
grep -n "EVENT_SEND_TIMEOUT\|Duration" src/events/bus.rs
```
Remove the `const EVENT_SEND_TIMEOUT: Duration = Duration::from_millis(50);` line. If `Duration` has no other reference left in the file, also remove `use std::time::Duration;`.

- [ ] **Step 3: Build to catch any leftover reference**

Run: `cargo build --all-features`
Expected: exits 0, no unused-import or unused-const warnings.

- [ ] **Step 4: Full test suite**

Run: `cargo test --all --all-features`
Expected: exits 0, all tests pass (baseline was 266 passing before this work; expect 266 + 3 new = 269).

- [ ] **Step 5: Clippy and fmt**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean, 0 warnings.

Run: `cargo fmt --check`
Expected: clean, no diff.

- [ ] **Step 6: Commit**

```bash
git add src/ipc/protocol.rs src/events/bus.rs
git commit -m "chore: remove unused send-timeout constants after T-03 try_send migration"
```

---

### Task 5: Update `ROADMAP.md` and close out T-03

**Files:**
- Modify: `ROADMAP.md` (T-03 entry, currently `ROADMAP.md:77-78`; Task Summary table row, `ROADMAP.md:161`; Ship gate line, `ROADMAP.md:163`)

- [ ] **Step 1: Mark T-03 done in the Audit Remediation section**

In `ROADMAP.md`, change:

```markdown
**T-03 — Single-threaded IPC router stalls kernel-wide on one slow plugin**
`src/ipc/protocol.rs:648-654` (`forward`), `:725-734` (`broadcast`), `src/events/bus.rs:127-153` (`deliver`). All fan-out sends `.await` a 50ms timeout inline on the shared router task; `broadcast` loops all plugins = `O(n)*50ms`. One non-draining plugin stalls routing for everyone. Fix: spawn per-target send tasks or use `try_send` + bounded retry queue instead of blocking the router loop. Needs design thought — own workstream.
```

to:

```markdown
**T-03 — Single-threaded IPC router stalls kernel-wide on one slow plugin** ✅ done
`src/ipc/protocol.rs` (`forward`, `broadcast`), `src/events/bus.rs` (`deliver`). All fan-out sends `.await`ed a 50ms timeout inline on the shared router task; `broadcast`/event delivery looped all subscribers = `O(n)*50ms`. One non-draining plugin stalled routing for everyone. Fix: design doc `docs/superpowers/specs/2026-07-08-ipc-router-nonblocking-send-design.md` picked non-blocking `try_send` over spawning a task per send — spawning would let concurrent sends to the same target race out of delivery order, a protocol correctness regression for per-connection frame sequencing. All three call sites (`forward`, `broadcast`, `EventBus::deliver`) now use `try_send`; a full/closed channel drops the frame immediately instead of after up to 50ms, with the same counters as before (`ipc_forward_timeouts_total`, `broadcast_timeouts_total`, `events_dropped_total`). Tests: `tests/unit/test_router.rs` (`forward_to_full_channel_returns_without_waiting`, `broadcast_to_many_stuck_targets_does_not_multiply_delay`), `tests/unit/test_event_bus.rs` (`publish_to_many_stuck_subscribers_does_not_multiply_delay`).
```

- [ ] **Step 2: Update the Task Summary table and ship gate line**

In the Task Summary table row for Audit remediation, update the "T-01,T-02,T-04..T-15,T-17,T-18,T-20 ✅ done" list to include T-03:

```markdown
| Audit remediation | T-01..20 | 2 Critical, 5 High, 11 Medium, 2 Low | T-01/T-05 fix together; rest are independent; T-01..T-15,T-17,T-18,T-20 ✅ done |
```

In the **Ship gate** paragraph, insert a clause for T-03 into the existing sentence listing landed items, in numeric position right after the T-02 clause and before the T-04 clause:

```markdown
T-03 (single-threaded IPC router/event-bus fan-out now uses non-blocking `try_send`, no more O(n)*50ms broadcast stall),
```

The sentence "Remaining open: T-16 (deferred to next protocol version bump)." already names only T-16 as open — leave it unchanged, since T-03 is being closed in this same edit and was never listed there.

- [ ] **Step 3: Commit**

```bash
git add ROADMAP.md
git commit -m "docs: mark T-03 done in ROADMAP.md"
```

## Self-Review Notes

- **Spec coverage:** spec's three call sites (forward, broadcast, deliver) each have a task (1, 2, 3); constant/import cleanup has Task 4; ROADMAP update has Task 5. Spec's "why not spawn a task" and "why not retry queue" are documentation-only, captured in the ROADMAP done-note, no separate task needed.
- **Placeholder scan:** none found — every step has literal code/commands.
- **Type consistency:** `entry.write_tx: mpsc::Sender<Outbound>` used consistently across all three tasks; `try_send` return type `Result<(), TrySendError<Outbound>>` matched correctly in Task 3's explicit match, and via `.is_err()` in Tasks 1-2 (equivalent, just don't need to distinguish Full vs Closed there since both already produced the same warn+counter behavior in the original code).
