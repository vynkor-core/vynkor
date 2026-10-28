use crate::ipc::connection::{Outbound, SessionKeyCell};
use crate::ipc::framing::Frame;
use tokio::sync::mpsc;

// Clone: the lazy waker re-injects a queued copy once the plugin registers
#[derive(Clone)]
pub struct IncomingMessage {
    pub conn_id: u64,
    pub frame: Frame,
    pub write_tx: mpsc::Sender<Outbound>,
    /// Shared cell the router writes the derived MAC key into on registration.
    pub session_key: SessionKeyCell,
}
