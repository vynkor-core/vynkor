use super::helpers::start_kernel;
use prost::Message;
use std::time::Duration;
use tokio::time::timeout;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::protocol::Message as WsMsg;
use veyron::proto::veyron::{envelope, Envelope, PluginManifest, PluginRegister};

fn build_frame(target: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + payload.len());
    out.extend_from_slice(&0x5652u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
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
    let (_shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_ws.sock", 19300).await;

    // give the HTTP server a moment to start
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut ws, _) =
        tokio_tungstenite::connect_async("ws://127.0.0.1:19300/ws")
            .await
            .expect("WS connect failed");

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

    ws.send(WsMsg::Binary(frame))
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
