//! E-01 end-to-end over the real WS gateway: pair → connect → MAC'd ping →
//! revoke → rejected → expired → rejected.
//!
//! The store writes here go through the same `DeviceStore` the CLI uses, onto
//! the kernel's data_dir — and because the kernel re-reads the store on every
//! upgrade/registration, revocation takes effect on the running process with
//! no restart.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::protocol::Message as WsMsg;

use vynkor::auth::device_store::DeviceStore;
use vynkor::auth::frame_mac::{compute_tag, derive_session_key};
use vynkor::proto::vynkor::{envelope, Envelope, Ping, PluginManifest, PluginRegister};

use crate::helpers::start_kernel_secured_with_data_dir;

const MASTER: &str = "e01-ws-integration-secret-32-bytes!!";

async fn ws_connect(port: u16, token: &str) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    use tokio_tungstenite::tungstenite::handshake::client::generate_key;
    use tokio_tungstenite::tungstenite::http::Request;
    let req = Request::builder()
        .method("GET")
        .uri(format!("ws://127.0.0.1:{port}/ws"))
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .header("sec-websocket-protocol", format!("vynkor, {token}"))
        .body(())
        .unwrap();
    tokio_tungstenite::connect_async(req)
        .await
        .map(|(ws, _)| ws)
}

fn register_env(plugin_id: &str, device_id: &str, token: &str) -> Vec<u8> {
    let env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: plugin_id.to_string(),
            manifest: Some(PluginManifest::default()),
            jwt_token: token.to_string(),
            device_id: device_id.to_string(),
            os: vynkor::proto::vynkor::DeviceOs::Android as i32,
            protocol_version: "1.7".to_string(),
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut buf = Vec::new();
    env.encode(&mut buf).unwrap();
    buf
}

/// 44-byte header + payload (+ 32-byte tag when key given).
fn build_frame(target: &str, payload: &[u8], key: Option<&[u8; 32]>) -> Vec<u8> {
    use vynkor::auth::frame_mac::MAC_TAG_LEN;
    let mut header = [0u8; 44];
    header[0..2].copy_from_slice(&0x5652u16.to_be_bytes());
    let flags: u16 = if key.is_some() { 0x0001 } else { 0 };
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    let n = target.len().min(32);
    header[8..8 + n].copy_from_slice(&target.as_bytes()[..n]);
    header[40..44].copy_from_slice(&crc32fast::hash(payload).to_be_bytes());
    let mut out = Vec::with_capacity(44 + payload.len() + MAC_TAG_LEN);
    out.extend_from_slice(&header);
    out.extend_from_slice(payload);
    if let Some(k) = key {
        out.extend_from_slice(&compute_tag(k, &header, payload));
    }
    out
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn send_frame(ws: &mut Ws, bytes: Vec<u8>) {
    ws.send(WsMsg::Binary(bytes)).await.unwrap();
}

fn mint(device_id: &str) -> String {
    vynkor::auth::jwt::mint_device_token(
        MASTER.as_bytes(),
        device_id,
        vec!["PERMISSION_IPC_SEND".into()],
        vec![],
        600,
        "vynkor",
    )
    .unwrap()
}

/// The full happy path then the revoked path.
#[tokio::test]
async fn paired_device_connects_macs_then_revocation_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let (_shutdown, _reg, _bus) =
        start_kernel_secured_with_data_dir("/tmp/vynkor_e01.sock", 19610, MASTER, dir.path()).await;

    // ── pair (what `vyn device connect` does server-side)
    let store = DeviceStore::new(dir.path(), MASTER);
    let device_secret = store.issue("behzod-phone", "behzod-phone", 3600).unwrap();

    // ── fresh CLI-process view sees the row immediately
    let cli_store = DeviceStore::new(dir.path(), MASTER);
    assert!(cli_store.active_secret("behzod-phone").unwrap().is_some());

    // ── connect over WS with the paired credential
    let token = mint("behzod-phone");
    let mut ws = timeout(Duration::from_secs(2), ws_connect(19610, &token))
        .await
        .expect("upgrade within 2s")
        .expect("upgrade must succeed for a paired device");

    send_frame(
        &mut ws,
        build_frame(
            "kernel",
            &register_env("behzod-phone.geo", "behzod-phone", &token),
            None,
        ),
    )
    .await;

    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ack = Envelope::decode(parse_payload(&msg).as_slice()).unwrap();
    let vynkor::proto::vynkor::envelope::Payload::PluginRegisterAck(ack) = ack.payload.unwrap()
    else {
        panic!("expected ack");
    };
    assert!(ack.accepted, "{}", ack.reject_reason);

    // ── MAC'd ping keyed by the DEVICE secret round-trips
    let key = derive_session_key(
        device_secret.as_bytes(),
        &ack.session_nonce,
        "behzod-phone.geo",
    );
    let ping = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 42 })),
        ..Default::default()
    };
    let mut payload = Vec::new();
    ping.encode(&mut payload).unwrap();
    send_frame(&mut ws, build_frame("kernel", &payload, Some(&key))).await;
    let reply = timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let env = Envelope::decode(parse_payload(&reply).as_slice()).unwrap();
    assert!(matches!(env.payload, Some(envelope::Payload::Pong(_))));

    drop(ws);

    // ── revoke from a second process view; SAME valid token now dies at upgrade
    cli_store.set_revoked("behzod-phone", true).unwrap();
    let err = timeout(Duration::from_secs(2), ws_connect(19610, &token))
        .await
        .expect("connect attempt resolves")
        .expect_err("revoked device must be rejected at WS upgrade");
    assert!(
        err.to_string().contains("401"),
        "expected HTTP 401 rejection, got: {err}"
    );

    // ── registration-level gate also fires if some proxy let it through:
    //    a DIFFERENT sub whose row is revoked is checked again post-validate.
    cli_store.set_revoked("behzod-phone", false).unwrap();
    cli_store.remove("behzod-phone").unwrap();
    let mut ws2 = timeout(Duration::from_secs(2), ws_connect(19610, &token))
        .await
        .expect("upgrade resolves")
        .expect("unknown sub passes the upgrade gate");

    send_frame(
        &mut ws2,
        build_frame(
            "kernel",
            &register_env("behzod-phone.battery", "behzod-phone", &token),
            None,
        ),
    )
    .await;
    let msg = timeout(Duration::from_secs(2), ws2.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let env = Envelope::decode(parse_payload(&msg).as_slice()).unwrap();
    match env.payload {
        Some(envelope::Payload::PluginRegisterAck(ack)) => {
            assert!(!ack.accepted);
            assert!(
                ack.reject_reason.contains("unknown device"),
                "{}",
                ack.reject_reason
            );
        }
        other => panic!("expected register reject, got {other:?}"),
    }
}

#[tokio::test]
async fn expired_device_is_rejected_at_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let (_shutdown, _reg, _bus) =
        start_kernel_secured_with_data_dir("/tmp/vynkor_e01b.sock", 19611, MASTER, dir.path())
            .await;

    let store = DeviceStore::new(dir.path(), MASTER);
    store.issue("old-phone", "old", 1).unwrap();
    tokio::time::sleep(Duration::from_millis(1300)).await;

    let err = timeout(
        Duration::from_secs(2),
        ws_connect(19611, &mint("old-phone")),
    )
    .await
    .expect("connect attempt resolves")
    .expect_err("expired device must be rejected at WS upgrade");
    assert!(err.to_string().contains("401"), "got: {err}");
}

fn parse_payload(msg: &WsMsg) -> Vec<u8> {
    let data = match msg {
        WsMsg::Binary(d) => d.clone(),
        other => panic!("expected binary frame, got {other:?}"),
    };
    // frame layout: magic(2) flags(2) len(4) target(32) crc(4) payload [mac]
    let len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    data[44..44 + len].to_vec()
}
