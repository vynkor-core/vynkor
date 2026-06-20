use crate::ipc::connection::ConnectionHandler;
use crate::ipc::messages::IncomingMessage;
use crate::utils::errors::VeyronError;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::info;

pub struct UdsServer;

impl UdsServer {
    pub async fn start(
        socket_path: &Path,
        tx: tokio::sync::mpsc::Sender<IncomingMessage>,
    ) -> Result<(JoinHandle<()>, tokio::sync::mpsc::Receiver<u64>), VeyronError> {
        let _ = std::fs::remove_file(socket_path);

        let listener = UnixListener::bind(socket_path)?;
        fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600))
            .map_err(VeyronError::Io)?;
        info!(
            "socket {} bound with 0o600 permissions",
            socket_path.display()
        );
        let counter = Arc::new(AtomicU64::new(1));
        let (disconnect_tx, disconnect_rx) = tokio::sync::mpsc::channel::<u64>(64);

        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let conn_id = counter.fetch_add(1, Ordering::Relaxed);
                let (handler, _write_tx) =
                    ConnectionHandler::new(conn_id, stream, tx.clone(), disconnect_tx.clone());
                tokio::spawn(handler.run());
            }
        });

        Ok((handle, disconnect_rx))
    }
}
