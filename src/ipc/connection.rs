use crate::ipc::framing::{read_frame, write_frame_raw, Frame};
use crate::ipc::messages::IncomingMessage;
use crate::utils::errors::VeyronError;
use metrics::counter;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{info, warn};

pub struct ConnectionHandler {
    conn_id: u64,
    read_half: OwnedReadHalf,
    write_tx: mpsc::Sender<Frame>,
    incoming_tx: mpsc::Sender<IncomingMessage>,
    disconnect_tx: mpsc::Sender<u64>,
}

impl ConnectionHandler {
    pub fn new(
        conn_id: u64,
        stream: UnixStream,
        incoming_tx: mpsc::Sender<IncomingMessage>,
        disconnect_tx: mpsc::Sender<u64>,
    ) -> (Self, mpsc::Sender<Frame>) {
        let (read_half, write_half) = stream.into_split();
        let (write_tx, write_rx) = mpsc::channel::<Frame>(64);

        tokio::spawn(write_loop(write_half, write_rx));

        let handler = ConnectionHandler {
            conn_id,
            read_half,
            write_tx: write_tx.clone(),
            incoming_tx,
            disconnect_tx,
        };

        (handler, write_tx)
    }

    pub async fn run(mut self) {
        info!(conn_id = self.conn_id, "connection opened");
        loop {
            match read_frame(&mut self.read_half).await {
                Ok(frame) => {
                    let msg = IncomingMessage {
                        conn_id: self.conn_id,
                        frame,
                        write_tx: self.write_tx.clone(),
                    };
                    if self.incoming_tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(VeyronError::FrameCrcMismatch) => {
                    warn!(conn_id = self.conn_id, "CRC mismatch on incoming frame");
                    counter!("ipc_frame_errors_total", "error" => "crc").increment(1);
                    break;
                }
                Err(VeyronError::FrameMagicMismatch) => {
                    warn!(conn_id = self.conn_id, "frame magic mismatch");
                    counter!("ipc_frame_errors_total", "error" => "magic").increment(1);
                    break;
                }
                Err(VeyronError::PayloadTooLarge(n)) => {
                    warn!(conn_id = self.conn_id, bytes = n, "oversized frame rejected");
                    counter!("ipc_frame_errors_total", "error" => "oversized").increment(1);
                    break;
                }
                Err(_) => break, // IO errors / EOF — normal disconnect
            }
        }
        info!(conn_id = self.conn_id, "connection closed");
        let _ = self.disconnect_tx.send(self.conn_id).await;
    }
}

async fn write_loop(mut write_half: OwnedWriteHalf, mut rx: mpsc::Receiver<Frame>) {
    while let Some(frame) = rx.recv().await {
        if write_frame_raw(&mut write_half, &frame).await.is_err() {
            break;
        }
    }
}
