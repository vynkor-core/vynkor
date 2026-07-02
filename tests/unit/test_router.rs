use prost::Message;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use veyron::events::bus::EventBus;
use veyron::ipc::connection::{out_frame, Outbound};
use veyron::ipc::framing::{target_as_str, Frame, FLAG_MAC_PRESENT};
use veyron::ipc::messages::IncomingMessage;
use veyron::ipc::protocol::MessageRouter;
use veyron::plugins::registry::PluginRegistry;
use veyron::proto::veyron::{
    envelope, Envelope, Ping, PluginManifest, PluginRegister, PluginRegisterAck,
};

// ── helpers ─────────────────────────────────────────────────────────────────

fn dummy_manifest() -> PluginManifest {
    PluginManifest::default()
}

fn ipc_manifest_with_targets(targets: Vec<&str>) -> PluginManifest {
    PluginManifest {
        permissions: vec!["PERMISSION_IPC_SEND".to_string()],
        ipc_targets: targets.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn make_write_pair() -> (mpsc::Sender<Outbound>, mpsc::Receiver<Outbound>) {
    mpsc::channel::<Outbound>(16)
}

fn encode_envelope(env: Envelope) -> Vec<u8> {
    let mut buf = Vec::new();
    env.encode(&mut buf).unwrap();
    buf
}

fn make_frame(target: &str, payload: Vec<u8>) -> Frame {
    let crc = crc32fast::hash(&payload);
    let mut t = [0u8; 32];
    let b = target.as_bytes();
    t[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
    Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: t,
        crc32: crc,
        payload,
        mac: None,
    }
}

fn kernel_frame(payload: Envelope) -> Frame {
    make_frame("kernel", encode_envelope(payload))
}

fn plug_frame(target: &str, payload: Vec<u8>) -> Frame {
    make_frame(target, payload)
}

fn incoming(conn_id: u64, frame: Frame, write_tx: mpsc::Sender<Outbound>) -> IncomingMessage {
    IncomingMessage {
        conn_id,
        frame,
        write_tx,
        session_key: Arc::new(Mutex::new(None)),
    }
}

fn spawn_router(
    registry: Arc<PluginRegistry>,
    event_bus: Arc<EventBus>,
) -> mpsc::Sender<IncomingMessage> {
    let (tx, rx) = mpsc::channel::<IncomingMessage>(64);
    tokio::spawn(MessageRouter::run(rx, registry, event_bus, None));
    tx
}

async fn recv_frame(rx: &mut mpsc::Receiver<Outbound>) -> Frame {
    let item = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("recv timed out")
        .expect("channel closed");
    match item {
        Outbound::Frame(f) => *f,
        _ => panic!("expected Outbound::Frame"),
    }
}

fn decode_envelope(frame: &Frame) -> Envelope {
    Envelope::decode(frame.payload.as_slice()).expect("failed to decode envelope")
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn router_handles_plugin_register_and_sends_ack() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let (write_tx, mut write_rx) = make_write_pair();

    let env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "weather".to_string(),
            version: "1.0.0".to_string(),
            manifest: Some(dummy_manifest()),
            ..Default::default()
        })),
        ..Default::default()
    };

    router_tx
        .send(incoming(1, kernel_frame(env), write_tx))
        .await
        .unwrap();

    let frame = recv_frame(&mut write_rx).await;
    let ack_env = decode_envelope(&frame);

    match ack_env.payload {
        Some(envelope::Payload::PluginRegisterAck(PluginRegisterAck { accepted, .. })) => {
            assert!(accepted, "ack must be accepted=true");
        }
        other => panic!("expected PluginRegisterAck, got {:?}", other),
    }

    assert!(
        reg.is_registered(1),
        "conn_id 1 must be in registry after registration"
    );
}

#[tokio::test]
async fn router_forwards_frame_to_target_plugin() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    // pre-register plugin B in registry
    let (b_write_tx, mut b_write_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_write_tx)
        .unwrap();

    // plugin A (conn_id=1): ipc_send + plugin_b in allowlist
    let (a_write_tx, _a_write_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b"]),
        a_write_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let raw_payload = b"arbitrary bytes".to_vec();
    let frame = plug_frame("plugin_b", raw_payload.clone());

    router_tx
        .send(incoming(1, frame, a_write_tx))
        .await
        .unwrap();

    let received = recv_frame(&mut b_write_rx).await;
    assert_eq!(received.payload, raw_payload);
    assert_eq!(target_as_str(&received), Some("plugin_b"));
}

#[tokio::test]
async fn slow_target_does_not_stall_router() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    // slow target: capacity-1 channel, pre-filled and never drained. Keep the
    // receiver alive so the channel stays open and full (send blocks → times out).
    let (slow_tx, _slow_rx) = mpsc::channel::<Outbound>(1);
    slow_tx
        .send(out_frame(make_frame("x", b"prefill".to_vec())))
        .await
        .unwrap();
    reg.register("slow".to_string(), 2, dummy_manifest(), slow_tx)
        .unwrap();

    // fast target: drained normally
    let (fast_tx, mut fast_rx) = make_write_pair();
    reg.register("fast".to_string(), 3, dummy_manifest(), fast_tx)
        .unwrap();

    // sender: ipc_send + both targets in allowlist
    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "sender".to_string(),
        1,
        ipc_manifest_with_targets(vec!["slow", "fast"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    // frame to the stuck target (router must give up after the send timeout)...
    router_tx
        .send(incoming(
            1,
            plug_frame("slow", b"to-slow".to_vec()),
            a_tx.clone(),
        ))
        .await
        .unwrap();
    // ...then a frame to a healthy target.
    router_tx
        .send(incoming(
            1,
            plug_frame("fast", b"to-fast".to_vec()),
            a_tx.clone(),
        ))
        .await
        .unwrap();

    // With an unbounded send the router would block on "slow" forever and "fast"
    // would never arrive. The bounded send must let it through promptly.
    let f = recv_frame(&mut fast_rx).await;
    assert_eq!(f.payload, b"to-fast");
    assert_eq!(target_as_str(&f), Some("fast"));
}

#[tokio::test]
async fn router_denies_forward_without_ipc_permission() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    // target plugin B registered
    let (b_write_tx, mut b_write_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_write_tx)
        .unwrap();

    // sender plugin A lacks PERMISSION_IPC_SEND (default-deny)
    let (a_write_tx, mut a_write_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        dummy_manifest(),
        a_write_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let frame = plug_frame("plugin_b", b"blocked".to_vec());
    router_tx
        .send(incoming(1, frame, a_write_tx))
        .await
        .unwrap();

    // sender gets an error frame; target receives nothing
    let err = recv_frame(&mut a_write_rx).await;
    let env = Envelope::decode(err.payload.as_slice()).unwrap();
    assert!(matches!(env.payload, Some(envelope::Payload::Error(_))));
    assert!(b_write_rx.try_recv().is_err());
}

#[tokio::test]
async fn router_broadcasts_star_to_all_except_sender() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (a_tx, mut a_rx) = make_write_pair();
    let (b_tx, mut b_rx) = make_write_pair();
    let (c_tx, mut c_rx) = make_write_pair();

    // sender holds PERMISSION_IPC_SEND and lists both recipients in ipc_targets
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b", "plugin_c"]),
        a_tx.clone(),
    )
    .unwrap();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();
    reg.register("plugin_c".to_string(), 3, dummy_manifest(), c_tx)
        .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let payload = b"broadcast payload".to_vec();
    let frame = make_frame("*", payload.clone());

    // sender is plugin_a (conn_id=1)
    router_tx.send(incoming(1, frame, a_tx)).await.unwrap();

    // B and C must receive it
    let fb = recv_frame(&mut b_rx).await;
    let fc = recv_frame(&mut c_rx).await;
    assert_eq!(fb.payload, payload);
    assert_eq!(fc.payload, payload);

    // A (sender) must NOT receive it
    let a_result = timeout(Duration::from_millis(100), a_rx.recv()).await;
    assert!(
        a_result.is_err(),
        "sender must not receive its own broadcast"
    );
}

#[tokio::test]
async fn router_denies_broadcast_without_ipc_permission() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (a_tx, mut a_rx) = make_write_pair();
    let (b_tx, mut b_rx) = make_write_pair();

    // sender plugin_a lacks PERMISSION_IPC_SEND (default-deny)
    reg.register("plugin_a".to_string(), 1, dummy_manifest(), a_tx.clone())
        .unwrap();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let frame = make_frame("*", b"blocked".to_vec());
    router_tx.send(incoming(1, frame, a_tx)).await.unwrap();

    // sender gets an error; no peer receives the broadcast
    let err = recv_frame(&mut a_rx).await;
    let env = Envelope::decode(err.payload.as_slice()).unwrap();
    assert!(matches!(env.payload, Some(envelope::Payload::Error(_))));
    assert!(b_rx.try_recv().is_err());
}

#[tokio::test]
async fn router_throttles_connection_after_error_burst() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    let router_tx = spawn_router(Arc::clone(&reg), bus);

    // wide channel so all error responses buffer without blocking the router
    let (tx, mut rx) = mpsc::channel::<Outbound>(64);

    // 16 malformed kernel frames (invalid protobuf) -> 16 error responses
    for _ in 0..16 {
        let frame = make_frame("kernel", vec![0xff, 0xff, 0xff, 0xff]);
        router_tx
            .send(incoming(1, frame, tx.clone()))
            .await
            .unwrap();
    }
    for _ in 0..16 {
        let f = recv_frame(&mut rx).await;
        let env = Envelope::decode(f.payload.as_slice()).unwrap();
        assert!(matches!(env.payload, Some(envelope::Payload::Error(_))));
    }

    // 17th crosses the budget -> dropped, no response
    let frame = make_frame("kernel", vec![0xff, 0xff, 0xff, 0xff]);
    router_tx
        .send(incoming(1, frame, tx.clone()))
        .await
        .unwrap();
    let throttled = timeout(Duration::from_millis(200), rx.recv()).await;
    assert!(
        throttled.is_err(),
        "throttled connection must receive no further responses"
    );
}

#[tokio::test]
async fn router_resets_error_budget_on_success() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    reg.register(
        "pinger".to_string(),
        1,
        dummy_manifest(),
        make_write_pair().0,
    )
    .unwrap();
    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let (tx, mut rx) = mpsc::channel::<Outbound>(64);

    // alternate: malformed (error) then valid ping (success resets budget) x many.
    // never accrues to the throttle threshold, so every ping still gets a pong.
    for _ in 0..30 {
        let bad = make_frame("kernel", vec![0xff, 0xff, 0xff, 0xff]);
        router_tx.send(incoming(1, bad, tx.clone())).await.unwrap();
        let _err = recv_frame(&mut rx).await;

        let ping = kernel_frame(Envelope {
            payload: Some(envelope::Payload::Ping(Ping { timestamp: 1 })),
            ..Default::default()
        });
        router_tx.send(incoming(1, ping, tx.clone())).await.unwrap();
        let pong = recv_frame(&mut rx).await;
        let env = Envelope::decode(pong.payload.as_slice()).unwrap();
        assert!(matches!(env.payload, Some(envelope::Payload::Pong(_))));
    }
}

#[tokio::test]
async fn router_responds_to_ping_with_pong() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (write_tx, mut write_rx) = make_write_pair();
    reg.register("pinger".to_string(), 5, dummy_manifest(), write_tx.clone())
        .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let ping_env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 999 })),
        ..Default::default()
    };

    router_tx
        .send(incoming(5, kernel_frame(ping_env), write_tx))
        .await
        .unwrap();

    let frame = recv_frame(&mut write_rx).await;
    let env = decode_envelope(&frame);

    assert!(
        matches!(env.payload, Some(envelope::Payload::Pong(_))),
        "expected Pong, got {:?}",
        env.payload
    );
}

#[tokio::test]
async fn router_rejects_non_register_from_unregistered_conn() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let (write_tx, mut write_rx) = make_write_pair();

    // send arbitrary frame targeting "kernel" without registering first
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 1 })),
        ..Default::default()
    };

    router_tx
        .send(incoming(99, kernel_frame(env), write_tx))
        .await
        .unwrap();

    let frame = recv_frame(&mut write_rx).await;
    let env = decode_envelope(&frame);

    assert!(
        matches!(env.payload, Some(envelope::Payload::Error(_))),
        "expected ErrorMessage, got {:?}",
        env.payload
    );
}

#[tokio::test]
async fn router_sends_error_for_unknown_target_plugin() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (write_tx, mut write_rx) = make_write_pair();
    reg.register("sender".to_string(), 7, dummy_manifest(), write_tx.clone())
        .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let frame = plug_frame("nonexistent_plugin", b"data".to_vec());
    router_tx.send(incoming(7, frame, write_tx)).await.unwrap();

    let response = recv_frame(&mut write_rx).await;
    let env = decode_envelope(&response);

    assert!(
        matches!(env.payload, Some(envelope::Payload::Error(_))),
        "expected ErrorMessage for unknown target, got {:?}",
        env.payload
    );
}

// ── T-04: per-plugin IPC allowlist ───────────────────────────────────────────

#[tokio::test]
async fn router_denies_forward_with_empty_ipc_targets() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    // sender: ipc_send granted but ipc_targets empty → deny-all
    let (a_tx, mut a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec![]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);
    router_tx
        .send(incoming(
            1,
            plug_frame("plugin_b", b"blocked".to_vec()),
            a_tx,
        ))
        .await
        .unwrap();

    let err = recv_frame(&mut a_rx).await;
    let env = Envelope::decode(err.payload.as_slice()).unwrap();
    assert!(matches!(env.payload, Some(envelope::Payload::Error(_))));
    assert!(b_rx.try_recv().is_err());
}

#[tokio::test]
async fn router_allows_forward_to_listed_ipc_target() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    // sender: ipc_send + plugin_b in allowlist
    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);
    let payload = b"hello".to_vec();
    router_tx
        .send(incoming(1, plug_frame("plugin_b", payload.clone()), a_tx))
        .await
        .unwrap();

    let received = recv_frame(&mut b_rx).await;
    assert_eq!(received.payload, payload);
}

#[tokio::test]
async fn router_denies_forward_to_unlisted_ipc_target() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();
    let (c_tx, mut c_rx) = make_write_pair();
    reg.register("plugin_c".to_string(), 3, dummy_manifest(), c_tx)
        .unwrap();

    // sender: ipc_send + only plugin_b allowed, not plugin_c
    let (a_tx, mut a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);
    router_tx
        .send(incoming(
            1,
            plug_frame("plugin_c", b"blocked".to_vec()),
            a_tx,
        ))
        .await
        .unwrap();

    let err = recv_frame(&mut a_rx).await;
    let env = Envelope::decode(err.payload.as_slice()).unwrap();
    assert!(matches!(env.payload, Some(envelope::Payload::Error(_))));
    assert!(b_rx.try_recv().is_err());
    assert!(c_rx.try_recv().is_err());
}

// ── T-19: mutex poison hardening (VULN-013) ─────────────────────────────────

/// VULN-013: if the SessionKeyCell mutex is poisoned before registration,
/// `if let Ok(...)` silently skips key installation — MAC verification is
/// never activated and the connection proceeds unprotected.
/// After the fix, the key must be installed despite the prior poison.
#[tokio::test]
async fn poisoned_session_key_cell_still_installs_mac_key() {
    use veyron::ipc::messages::IncomingMessage;

    // Poison the mutex on a dedicated OS thread, then join to confirm.
    let cell: Arc<Mutex<Option<[u8; 32]>>> = Arc::new(Mutex::new(None));
    let cell_for_poison = Arc::clone(&cell);
    let _ = std::thread::spawn(move || {
        let _guard = cell_for_poison.lock().unwrap();
        panic!("intentionally poison the mutex");
    })
    .join();
    assert!(cell.lock().is_err(), "precondition: mutex must be poisoned");

    // Router with a mac_secret so it will derive + install a session key.
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    let mac_secret = Arc::new(b"t19-test-secret-key-32bytesXXXXX".to_vec());

    let (router_tx, rx) = mpsc::channel::<IncomingMessage>(16);
    tokio::spawn(MessageRouter::run_with_context(
        rx,
        Arc::clone(&reg),
        Arc::clone(&bus),
        None,
        std::time::Instant::now(),
        None,
        None,
        Some(Arc::clone(&mac_secret)),
        None,
    ));

    let (write_tx, mut write_rx) = make_write_pair();
    let env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "poison-test-plugin".to_string(),
            manifest: Some(dummy_manifest()),
            ..Default::default()
        })),
        ..Default::default()
    };

    router_tx
        .send(IncomingMessage {
            conn_id: 42,
            frame: kernel_frame(env),
            write_tx: write_tx.clone(),
            session_key: Arc::clone(&cell),
        })
        .await
        .unwrap();

    // Ack confirms registration was accepted.
    let frame = recv_frame(&mut write_rx).await;
    let ack_env = decode_envelope(&frame);
    match ack_env.payload {
        Some(envelope::Payload::PluginRegisterAck(PluginRegisterAck {
            accepted,
            session_nonce,
            ..
        })) => {
            assert!(accepted, "registration must be accepted");
            assert!(
                !session_nonce.is_empty(),
                "nonce must be present when mac_secret set"
            );
        }
        other => panic!("expected PluginRegisterAck, got {:?}", other),
    }

    // EnableMac is enqueued after the ack (VULN-020 fix). In production the
    // write_loop processes it. Here we simulate that: drain it and install the key.
    let enable_item = timeout(Duration::from_secs(2), write_rx.recv())
        .await
        .expect("EnableMac timed out")
        .expect("channel closed");
    match enable_item {
        Outbound::EnableMac(k, c) => {
            *c.lock().unwrap_or_else(|p| p.into_inner()) = Some(k);
        }
        _ => panic!("expected Outbound::EnableMac after ack"),
    }

    // Key must be installed despite the earlier poison — recover with unwrap_or_else.
    let installed = cell.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        installed.is_some(),
        "session key must be installed even when mutex was previously poisoned"
    );
}

// ── T-18: broadcast security (VULN-012, VULN-015) ───────────────────────────

/// VULN-012: sender with PERMISSION_IPC_SEND but empty ipc_targets must not
/// deliver a broadcast to any recipient — the per-target allowlist check must
/// apply inside the broadcast send loop, same as in forward().
#[tokio::test]
async fn broadcast_denied_with_empty_ipc_targets() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    // sender: ipc_send granted but ipc_targets empty → deny-all
    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec![]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);
    let frame = make_frame("*", b"secret".to_vec());
    router_tx.send(incoming(1, frame, a_tx)).await.unwrap();

    // plugin_b must receive nothing
    let result = timeout(Duration::from_millis(150), b_rx.recv()).await;
    assert!(
        result.is_err(),
        "broadcast from plugin with empty ipc_targets must not deliver to any recipient"
    );
}

/// VULN-015: broadcast must strip FLAG_MAC_PRESENT from the cloned frame's
/// flags so that the recipient's write_loop re-tags it under its own session
/// key rather than forwarding a stale/wrong tag.
#[tokio::test]
async fn broadcast_strips_flag_mac_present() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    // sender: ipc_send + plugin_b in allowlist
    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    // send a frame that has FLAG_MAC_PRESENT set in flags but mac: None
    let mut frame = make_frame("*", b"data".to_vec());
    frame.flags |= FLAG_MAC_PRESENT;

    router_tx.send(incoming(1, frame, a_tx)).await.unwrap();

    let received = recv_frame(&mut b_rx).await;
    assert_eq!(
        received.flags & FLAG_MAC_PRESENT,
        0,
        "broadcast must strip FLAG_MAC_PRESENT from cloned frame"
    );
}

/// AUDIT M-04: forward() (unicast) must strip FLAG_MAC_PRESENT the same way
/// broadcast() does — otherwise the recipient's write_loop sees a stale tag
/// flag from the sender's own session key.
#[tokio::test]
async fn forward_strips_flag_mac_present() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (b_tx, mut b_rx) = make_write_pair();
    reg.register("plugin_b".to_string(), 2, dummy_manifest(), b_tx)
        .unwrap();

    let (a_tx, _a_rx) = make_write_pair();
    reg.register(
        "plugin_a".to_string(),
        1,
        ipc_manifest_with_targets(vec!["plugin_b"]),
        a_tx.clone(),
    )
    .unwrap();

    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let mut frame = plug_frame("plugin_b", b"data".to_vec());
    frame.flags |= FLAG_MAC_PRESENT;

    router_tx.send(incoming(1, frame, a_tx)).await.unwrap();

    let received = recv_frame(&mut b_rx).await;
    assert_eq!(
        received.flags & FLAG_MAC_PRESENT,
        0,
        "forward must strip FLAG_MAC_PRESENT from the sender's frame"
    );
}

#[tokio::test]
async fn router_rejects_duplicate_plugin_id_registration() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());
    let router_tx = spawn_router(Arc::clone(&reg), bus);

    let (tx1, mut rx1) = make_write_pair();
    let (tx2, mut rx2) = make_write_pair();

    let reg_env = |id: &str| Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: id.to_string(),
            manifest: Some(dummy_manifest()),
            ..Default::default()
        })),
        ..Default::default()
    };

    router_tx
        .send(incoming(10, kernel_frame(reg_env("clash")), tx1))
        .await
        .unwrap();
    let f1 = recv_frame(&mut rx1).await;
    let e1 = decode_envelope(&f1);
    assert!(matches!(
        e1.payload,
        Some(envelope::Payload::PluginRegisterAck(PluginRegisterAck {
            accepted: true,
            ..
        }))
    ));

    router_tx
        .send(incoming(11, kernel_frame(reg_env("clash")), tx2))
        .await
        .unwrap();
    let f2 = recv_frame(&mut rx2).await;
    let e2 = decode_envelope(&f2);
    match e2.payload {
        Some(envelope::Payload::PluginRegisterAck(PluginRegisterAck { accepted, .. })) => {
            assert!(!accepted, "duplicate registration must be rejected");
        }
        other => panic!(
            "expected PluginRegisterAck(accepted=false), got {:?}",
            other
        ),
    }
}
