use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use metrics::counter;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::auth::frame_mac::{compute_tag, verify_tag};
use crate::auth::jwt::JwtValidator;
use crate::ipc::connection::{Outbound, SessionKeyCell};
use crate::ipc::framing::{serialize_header, Frame, FLAG_MAC_PRESENT, MAX_PAYLOAD_SIZE};
use crate::ipc::messages::IncomingMessage;

const FRAME_HEADER_SIZE: usize = 44;
pub const WS_CONN_ID_BASE: u64 = 1_000_000_000;

pub struct WsGateway {
    pub router_tx: mpsc::Sender<IncomingMessage>,
    pub disconnect_tx: mpsc::Sender<u64>,
    pub conn_counter: Arc<AtomicU64>,
    pub jwt_validator: Option<Arc<JwtValidator>>,
}

/// Extract JWT from `Sec-WebSocket-Protocol: veyron, <jwt>`.
/// Token is the first comma-separated entry that isn't "veyron".
/// Credentials stay in a request header, never in the URL.
fn extract_ws_token(headers: &HeaderMap) -> &str {
    headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').map(str::trim).find(|p| *p != "veyron"))
        .unwrap_or("")
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<Arc<WsGateway>>,
) -> Response {
    if let Some(validator) = &state.jwt_validator {
        let token = extract_ws_token(&headers);
        if let Err(e) = validator.validate(token) {
            warn!("WS: JWT rejected");
            let _ = e; // don't log token contents
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }

    let conn_id = state.conn_counter.fetch_add(1, Ordering::Relaxed) + WS_CONN_ID_BASE;
    let router_tx = state.router_tx.clone();
    let disconnect_tx = state.disconnect_tx.clone();

    ws.protocols(["veyron"])
        .on_upgrade(move |socket| handle_socket(socket, conn_id, router_tx, disconnect_tx))
}

async fn handle_socket(
    mut socket: WebSocket,
    conn_id: u64,
    router_tx: mpsc::Sender<IncomingMessage>,
    disconnect_tx: mpsc::Sender<u64>,
) {
    info!(conn_id = conn_id, "WS client connected");

    let (write_tx, mut write_rx) = mpsc::channel::<Outbound>(64);
    let session_key: SessionKeyCell = std::sync::Arc::new(std::sync::Mutex::new(None));
    // Key stored locally for tagging outbound frames; populated via Outbound::EnableMac.
    let mut outbound_key: Option<[u8; 32]> = None;

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match parse_frame(&data) {
                            Ok(frame) => {
                                // Verify MAC on inbound frames once session key is active.
                                let key = *session_key.lock().unwrap();
                                if let Some(k) = key {
                                    let valid = frame.flags & FLAG_MAC_PRESENT != 0
                                        && match &frame.mac {
                                            Some(tag) => {
                                                let header = serialize_header(&frame);
                                                verify_tag(&k, &header, &frame.payload, tag)
                                            }
                                            None => false,
                                        };
                                    if !valid {
                                        warn!(conn_id, "WS: frame MAC invalid — dropping connection");
                                        counter!("ipc_frame_errors_total", "error" => "mac").increment(1);
                                        break;
                                    }
                                }
                                let incoming = IncomingMessage {
                                    conn_id,
                                    frame,
                                    write_tx: write_tx.clone(),
                                    session_key: session_key.clone(),
                                };
                                if router_tx.send(incoming).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => warn!(conn_id = conn_id, "WS: bad frame: {e}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Err(e)) => {
                        warn!(conn_id = conn_id, "WS recv error: {e}");
                        break;
                    }
                    Some(Ok(_)) => {} // ping/pong/text ignored
                }
            }
            item = write_rx.recv() => {
                match item {
                    Some(Outbound::Frame(f)) => {
                        let mut frame = *f;
                        if let Some(k) = &outbound_key {
                            frame.flags |= FLAG_MAC_PRESENT;
                            let header = serialize_header(&frame);
                            frame.mac = Some(compute_tag(k, &header, &frame.payload));
                        }
                        if socket.send(Message::Binary(frame_to_bytes(&frame))).await.is_err() {
                            break;
                        }
                    }
                    Some(Outbound::EnableMac(k)) => {
                        outbound_key = Some(k);
                    }
                    None => break,
                }
            }
        }
    }

    info!(conn_id = conn_id, "WS client disconnected");
    let _ = disconnect_tx.send(conn_id).await;
}

fn parse_frame(data: &[u8]) -> Result<Frame, &'static str> {
    if data.len() < FRAME_HEADER_SIZE {
        return Err("frame too short");
    }
    let magic = u16::from_be_bytes([data[0], data[1]]);
    if magic != 0x5652 {
        return Err("bad magic");
    }
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let length = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
    if length > MAX_PAYLOAD_SIZE {
        return Err("payload too large");
    }
    let payload_end = FRAME_HEADER_SIZE + length;
    if data.len() < payload_end {
        return Err("truncated payload");
    }
    let mut target = [0u8; 32];
    target.copy_from_slice(&data[8..40]);
    let crc32 = u32::from_be_bytes([data[40], data[41], data[42], data[43]]);
    let payload = data[FRAME_HEADER_SIZE..payload_end].to_vec();
    let computed = crc32fast::hash(&payload);
    if computed != crc32 {
        return Err("CRC mismatch");
    }
    let mac = if flags & FLAG_MAC_PRESENT != 0 {
        if data.len() < payload_end + 32 {
            return Err("frame too short for MAC tag");
        }
        let mut tag = [0u8; 32];
        tag.copy_from_slice(&data[payload_end..payload_end + 32]);
        Some(tag)
    } else {
        None
    };
    Ok(Frame {
        magic,
        flags,
        length: length as u32,
        target,
        crc32,
        payload,
        mac,
    })
}

fn frame_to_bytes(frame: &Frame) -> Vec<u8> {
    let mac_len = if frame.mac.is_some() { 32 } else { 0 };
    let mut out = Vec::with_capacity(FRAME_HEADER_SIZE + frame.payload.len() + mac_len);
    out.extend_from_slice(&frame.magic.to_be_bytes());
    out.extend_from_slice(&frame.flags.to_be_bytes());
    out.extend_from_slice(&frame.length.to_be_bytes());
    out.extend_from_slice(&frame.target);
    out.extend_from_slice(&frame.crc32.to_be_bytes());
    out.extend_from_slice(&frame.payload);
    if let Some(tag) = &frame.mac {
        out.extend_from_slice(tag);
    }
    out
}
