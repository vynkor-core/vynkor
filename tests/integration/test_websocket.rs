use super::helpers::{start_kernel, start_kernel_secured};
use crate::jwt_helper::create_test_token;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::Message as WsMsg;
use veyron::auth::frame_mac::{compute_tag, derive_session_key, MAC_TAG_LEN};
use veyron::proto::veyron::{envelope, Envelope, Ping, PluginManifest, PluginRegister};

/// Build a frame with HMAC-SHA256 MAC (FLAG_MAC_PRESENT = 0x0001 set, 32-byte tag appended).
fn build_mac_frame(target: &str, payload: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let flags: u16 = 0x0001; // FLAG_MAC_PRESENT
    let mut header = [0u8; 44];
    header[0..2].copy_from_slice(&0x5652u16.to_be_bytes());
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut tgt = [0u8; 32];
    let n = target.len().min(32);
    tgt[..n].copy_from_slice(&target.as_bytes()[..n]);
    header[8..40].copy_from_slice(&tgt);
    header[40..44].copy_from_slice(&crc32fast::hash(payload).to_be_bytes());
    let tag = compute_tag(key, &header, payload);
    let mut out = Vec::with_capacity(44 + payload.len() + MAC_TAG_LEN);
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    out.extend_from_slice(&tag);
    out
}

/// Connect to a secured WS kernel with JWT in Sec-WebSocket-Protocol header.
async fn ws_connect_with_jwt(
    port: u16,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    use tokio_tungstenite::tungstenite::http::Request;
    for attempt in 0..20 {
        let req = Request::builder()
            .method("GET")
            .uri(format!("ws://127.0.0.1:{port}/ws"))
            .header("Host", format!("127.0.0.1:{port}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", generate_key())
            .header("sec-websocket-protocol", format!("veyron, {token}"))
            .body(())
            .unwrap();
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => return ws,
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => panic!("WS connect failed: {e}"),
        }
    }
    unreachable!()
}

/// Connect to an unsecured WS kernel, retrying while the HTTP listener spins up
/// (under parallel test load a fixed sleep is not always enough).
async fn ws_connect_retry(
    port: u16,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://127.0.0.1:{port}/ws");
    for attempt in 0..20 {
        match tokio_tungstenite::connect_async(&url).await {
            Ok((ws, _)) => return ws,
            Err(_) if attempt < 19 => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(e) => panic!("WS connect failed: {e}"),
        }
    }
    unreachable!()
}

/// Parse raw binary WS data as a frame, returning (payload_bytes, had_mac_flag).
fn parse_ws_frame(data: &[u8]) -> (Vec<u8>, bool) {
    assert!(data.len() >= 44, "frame too short");
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let length = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let payload = data[44..44 + length].to_vec();
    let has_mac = flags & 0x0001 != 0;
    (payload, has_mac)
}

fn build_frame(target: &str, payload: &[u8]) -> Vec<u8> {
    build_frame_with_flags(target, payload, 0)
}

fn build_frame_with_flags(target: &str, payload: &[u8], flags: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + payload.len());
    out.extend_from_slice(&0x5652u16.to_be_bytes());
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    let mut tgt = [0u8; 32];
    let n = target.len().min(32);
    tgt[..n].copy_from_slice(&target.as_bytes()[..n]);
    out.extend_from_slice(&tgt);
    out.extend_from_slice(&crc32fast::hash(payload).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[tokio::test]
async fn ws_client_registers_and_receives_ack() {
    let (_shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_ws.sock", 19300).await;

    let mut ws = ws_connect_retry(19300).await;

    // send PluginRegister
    let reg_env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "ws_test_plugin".to_string(),
            version: String::new(),
            description: String::new(),
            manifest: Some(PluginManifest::default()),
            jwt_token: String::new(),
        })),
        ..Default::default()
    };
    let mut buf = Vec::new();
    reg_env.encode(&mut buf).unwrap();
    let frame = build_frame("kernel", &buf);

    ws.send(WsMsg::Binary(frame)).await.expect("WS send failed");

    let reply = timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("WS recv timed out")
        .expect("stream ended")
        .expect("WS error");

    let data = match reply {
        WsMsg::Binary(b) => b,
        other => panic!("expected binary, got: {:?}", other),
    };

    // parse response frame: skip 44-byte header
    assert!(data.len() > 44, "response too short");
    let payload = &data[44..];
    let env = Envelope::decode(payload).expect("decode failed");
    match env.payload {
        Some(envelope::Payload::PluginRegisterAck(ack)) => {
            assert!(ack.accepted, "WS plugin registration must be accepted");
        }
        other => panic!("expected PluginRegisterAck, got: {:?}", other),
    }

    let _ = _shutdown_tx.send(());
}

/// R5-03: the WS gateway does not normalize compressed/fragmented inbound
/// frames before MAC verification/routing, so it must reject rather than
/// mishandle them. A single bad frame should not close the connection
/// (parse-error budget is 16) or corrupt subsequent parsing.
#[tokio::test]
async fn ws_rejects_compressed_and_fragmented_inbound_frames() {
    let (_shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_ws_compressed.sock", 19322).await;

    let mut ws = ws_connect_retry(19322).await;

    const FLAG_COMPRESSED: u16 = 0x0002;
    const FLAG_FRAGMENTED: u16 = 0x0004;

    ws.send(WsMsg::Binary(build_frame_with_flags(
        "kernel",
        b"not actually compressed",
        FLAG_COMPRESSED,
    )))
    .await
    .expect("WS send failed");
    ws.send(WsMsg::Binary(build_frame_with_flags(
        "kernel",
        b"not actually a fragment",
        FLAG_FRAGMENTED,
    )))
    .await
    .expect("WS send failed");

    // Connection must survive both rejected frames and keep working normally.
    let reg_env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "ws_test_plugin_compressed".to_string(),
            version: String::new(),
            description: String::new(),
            manifest: Some(PluginManifest::default()),
            jwt_token: String::new(),
        })),
        ..Default::default()
    };
    let mut buf = Vec::new();
    reg_env.encode(&mut buf).unwrap();
    ws.send(WsMsg::Binary(build_frame("kernel", &buf)))
        .await
        .expect("WS send failed");

    let reply = timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("WS recv timed out")
        .expect("stream ended")
        .expect("WS error");

    let data = match reply {
        WsMsg::Binary(b) => b,
        other => panic!("expected binary, got: {:?}", other),
    };
    assert!(data.len() > 44, "response too short");
    let payload = &data[44..];
    let env = Envelope::decode(payload).expect("decode failed");
    match env.payload {
        Some(envelope::Payload::PluginRegisterAck(ack)) => {
            assert!(
                ack.accepted,
                "registration after rejected frames must still succeed"
            );
        }
        other => panic!("expected PluginRegisterAck, got: {:?}", other),
    }

    let _ = _shutdown_tx.send(());
}

#[tokio::test]
async fn ws_mac_tagged_frames_accepted_on_secured_kernel() {
    let secret = "ws-mac-test-secret";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/veyron_ws_mac_ok.sock", 19310, secret).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let token = create_test_token("ws-mac-plugin", vec![], secret.as_bytes(), 3600);
    let mut ws = ws_connect_with_jwt(19310, &token).await;

    // Send PluginRegister with JWT
    let reg_env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "ws-mac-plugin".to_string(),
            version: String::new(),
            description: String::new(),
            manifest: Some(PluginManifest::default()),
            jwt_token: token.clone(),
        })),
        ..Default::default()
    };
    let mut reg_buf = Vec::new();
    reg_env.encode(&mut reg_buf).unwrap();
    ws.send(WsMsg::Binary(build_frame("kernel", &reg_buf)))
        .await
        .unwrap();

    // Receive PluginRegisterAck (no MAC, sent before MAC is enabled)
    let ack_data = match timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error")
    {
        WsMsg::Binary(b) => b,
        other => panic!("expected binary ack, got: {:?}", other),
    };

    let (ack_payload, _) = parse_ws_frame(&ack_data);
    let ack_env = Envelope::decode(ack_payload.as_ref()).expect("decode ack");
    let session_nonce = match ack_env.payload {
        Some(envelope::Payload::PluginRegisterAck(ack)) => {
            assert!(ack.accepted, "registration must succeed");
            assert_eq!(ack.session_nonce.len(), 16, "must get 16-byte nonce");
            ack.session_nonce
        }
        other => panic!("expected PluginRegisterAck, got {:?}", other),
    };

    // Derive session key same way the kernel does
    let key = derive_session_key(secret.as_bytes(), &session_nonce, "ws-mac-plugin");

    // Send a MAC-tagged Ping
    let ping_env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 42 })),
        ..Default::default()
    };
    let mut ping_buf = Vec::new();
    ping_env.encode(&mut ping_buf).unwrap();
    ws.send(WsMsg::Binary(build_mac_frame("kernel", &ping_buf, &key)))
        .await
        .unwrap();

    // Receive Pong — kernel must also tag its outbound frames
    let pong_data = match timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("recv timed out")
        .expect("stream ended")
        .expect("ws error")
    {
        WsMsg::Binary(b) => b,
        other => panic!("expected binary pong, got: {:?}", other),
    };

    let (pong_payload, had_mac) = parse_ws_frame(&pong_data);
    assert!(
        had_mac,
        "kernel must MAC-tag outbound WS frames on secured kernel"
    );
    let pong_env = Envelope::decode(pong_payload.as_ref()).expect("decode pong");
    assert!(
        matches!(pong_env.payload, Some(envelope::Payload::Pong(_))),
        "expected Pong, got {:?}",
        pong_env.payload
    );

    let _ = _shutdown.send(());
}

#[tokio::test]
async fn ws_untagged_frames_rejected_on_secured_kernel() {
    let secret = "ws-mac-reject-secret";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/veyron_ws_mac_reject.sock", 19311, secret).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let token = create_test_token("ws-plain-plugin", vec![], secret.as_bytes(), 3600);
    let mut ws = ws_connect_with_jwt(19311, &token).await;

    // Register (untagged — ack is sent before MAC kicks in)
    let reg_env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: "ws-plain-plugin".to_string(),
            version: String::new(),
            description: String::new(),
            manifest: Some(PluginManifest::default()),
            jwt_token: token.clone(),
        })),
        ..Default::default()
    };
    let mut reg_buf = Vec::new();
    reg_env.encode(&mut reg_buf).unwrap();
    ws.send(WsMsg::Binary(build_frame("kernel", &reg_buf)))
        .await
        .unwrap();

    // Drain the ack
    let _ = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ack timed out");

    // Send an untagged Ping — kernel must detect missing MAC and drop connection
    let ping_env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 1 })),
        ..Default::default()
    };
    let mut ping_buf = Vec::new();
    ping_env.encode(&mut ping_buf).unwrap();
    let _ = ws
        .send(WsMsg::Binary(build_frame("kernel", &ping_buf)))
        .await;

    // Expect no Pong: connection is closed or times out
    let got = timeout(Duration::from_secs(2), ws.next()).await;
    let dropped = match got {
        Err(_) => true,
        Ok(None) => true,
        Ok(Some(Err(_))) => true,
        Ok(Some(Ok(WsMsg::Close(_)))) => true,
        Ok(Some(Ok(_))) => false,
    };
    assert!(
        dropped,
        "kernel must drop WS connection that sends untagged frames after MAC is enabled"
    );

    let _ = _shutdown.send(());
}

#[tokio::test]
async fn ws_closed_after_max_parse_errors() {
    let (_shutdown, _reg, _bus) = start_kernel("/tmp/veyron_ws_parse_err.sock", 19321).await;

    let mut ws = ws_connect_retry(19321).await;

    // 44 bytes with bad magic — parse_frame returns Err on each
    let bad_frame = vec![0xFFu8; 44];
    for _ in 0..16 {
        if ws.send(WsMsg::Binary(bad_frame.clone())).await.is_err() {
            // connection already closed before we sent all 16 — budget works
            let _ = _shutdown.send(());
            return;
        }
        // small yield so the server processes each frame before the next arrives
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    // After 16 bad frames the server must close the connection
    let result = timeout(Duration::from_secs(2), ws.next()).await;
    let closed = match result {
        Err(_) => false, // still open — test fails
        Ok(None) => true,
        Ok(Some(Err(_))) => true,
        Ok(Some(Ok(WsMsg::Close(_)))) => true,
        Ok(Some(Ok(_))) => false,
    };
    assert!(closed, "WS connection must close after 16 parse errors");
    let _ = _shutdown.send(());
}
