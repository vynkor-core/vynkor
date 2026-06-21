use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::auth::jwt::JwtValidator;
use crate::ipc::framing::{Frame, MAX_PAYLOAD_SIZE};
use crate::ipc::messages::IncomingMessage;

const FRAME_HEADER_SIZE: usize = 44;
pub const WS_CONN_ID_BASE: u64 = 1_000_000_000;

pub struct WsGateway {
    pub router_tx: mpsc::Sender<IncomingMessage>,
    pub disconnect_tx: mpsc::Sender<u64>,
    pub conn_counter: Arc<AtomicU64>,
    pub jwt_validator: Option<Arc<JwtValidator>>,
}

#[derive(Deserialize)]
pub struct WsQuery {
    pub token: Option<String>,
}

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(q): Query<WsQuery>,
    State(state): State<Arc<WsGateway>>,
) -> Response {
    if let Some(validator) = &state.jwt_validator {
        let token = q.token.as_deref().unwrap_or("");
        if let Err(e) = validator.validate(token) {
            warn!("WS: JWT rejected: {e}");
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }

    let conn_id = state.conn_counter.fetch_add(1, Ordering::Relaxed) + WS_CONN_ID_BASE;
    let router_tx = state.router_tx.clone();
    let disconnect_tx = state.disconnect_tx.clone();

    ws.on_upgrade(move |socket| handle_socket(socket, conn_id, router_tx, disconnect_tx))
}

async fn handle_socket(
    mut socket: WebSocket,
    conn_id: u64,
    router_tx: mpsc::Sender<IncomingMessage>,
    disconnect_tx: mpsc::Sender<u64>,
) {
    info!(conn_id = conn_id, "WS client connected");

    let (write_tx, mut write_rx) = mpsc::channel::<Frame>(64);

    loop {
        tokio::select! {
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match parse_frame(&data) {
                            Ok(frame) => {
                                let incoming = IncomingMessage {
                                    conn_id,
                                    frame,
                                    write_tx: write_tx.clone(),
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
            frame = write_rx.recv() => {
                match frame {
                    Some(f) => {
                        if socket.send(Message::Binary(frame_to_bytes(&f))).await.is_err() {
                            break;
                        }
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
    if data.len() < FRAME_HEADER_SIZE + length {
        return Err("truncated payload");
    }
    let mut target = [0u8; 32];
    target.copy_from_slice(&data[8..40]);
    let crc32 = u32::from_be_bytes([data[40], data[41], data[42], data[43]]);
    let payload = data[FRAME_HEADER_SIZE..FRAME_HEADER_SIZE + length].to_vec();
    let computed = crc32fast::hash(&payload);
    if computed != crc32 {
        return Err("CRC mismatch");
    }
    Ok(Frame {
        magic,
        flags,
        length: length as u32,
        target,
        crc32,
        payload,
    })
}

fn frame_to_bytes(frame: &Frame) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_SIZE + frame.payload.len());
    out.extend_from_slice(&frame.magic.to_be_bytes());
    out.extend_from_slice(&frame.flags.to_be_bytes());
    out.extend_from_slice(&frame.length.to_be_bytes());
    out.extend_from_slice(&frame.target);
    out.extend_from_slice(&frame.crc32.to_be_bytes());
    out.extend_from_slice(&frame.payload);
    out
}
