use crate::ipc::framing::Frame;
use tokio::sync::mpsc;

#[allow(dead_code)]
pub struct IncomingMessage {
    pub conn_id: u64,
    pub frame: Frame,
    pub write_tx: mpsc::Sender<Frame>,
}
