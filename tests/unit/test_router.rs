use prost::Message;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;
use veyron::events::bus::EventBus;
use veyron::ipc::framing::{target_as_str, Frame};
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

fn ipc_manifest() -> PluginManifest {
    PluginManifest {
        permissions: vec!["PERMISSION_IPC_SEND".to_string()],
        ..Default::default()
    }
}

fn make_write_pair() -> (mpsc::Sender<Frame>, mpsc::Receiver<Frame>) {
    mpsc::channel::<Frame>(16)
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
    }
}

fn kernel_frame(payload: Envelope) -> Frame {
    make_frame("kernel", encode_envelope(payload))
}

fn plug_frame(target: &str, payload: Vec<u8>) -> Frame {
    make_frame(target, payload)
}

fn incoming(conn_id: u64, frame: Frame, write_tx: mpsc::Sender<Frame>) -> IncomingMessage {
    IncomingMessage {
        conn_id,
        frame,
        write_tx,
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

async fn recv_frame(rx: &mut mpsc::Receiver<Frame>) -> Frame {
    timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("recv timed out")
        .expect("channel closed")
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

    // plugin A (conn_id=1) holds PERMISSION_IPC_SEND so router allows forwarding
    let (a_write_tx, _a_write_rx) = make_write_pair();
    reg.register("plugin_a".to_string(), 1, ipc_manifest(), a_write_tx.clone())
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
    assert_eq!(target_as_str(&received), "plugin_b");
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
    reg.register("plugin_a".to_string(), 1, dummy_manifest(), a_write_tx.clone())
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
    assert!(matches!(
        env.payload,
        Some(envelope::Payload::Error(_))
    ));
    assert!(b_write_rx.try_recv().is_err());
}

#[tokio::test]
async fn router_broadcasts_star_to_all_except_sender() {
    let reg = Arc::new(PluginRegistry::new());
    let bus = Arc::new(EventBus::new());

    let (a_tx, mut a_rx) = make_write_pair();
    let (b_tx, mut b_rx) = make_write_pair();
    let (c_tx, mut c_rx) = make_write_pair();

    reg.register("plugin_a".to_string(), 1, dummy_manifest(), a_tx.clone())
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
