#![allow(dead_code)]

use crate::ipc::framing::{read_frame, write_frame_raw, Frame};
use crate::ipc::messages::IncomingMessage;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

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
        while let Ok(frame) = read_frame(&mut self.read_half).await {
            let msg = IncomingMessage {
                conn_id: self.conn_id,
                frame,
                write_tx: self.write_tx.clone(),
            };
            if self.incoming_tx.send(msg).await.is_err() {
                break;
            }
        }
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
