use crate::ipc::connection::ConnectionHandler;
use crate::ipc::messages::IncomingMessage;
use crate::utils::errors::VeyronError;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use metrics::counter;

pub struct UdsServer;

impl UdsServer {
    pub async fn start(
        socket_path: &Path,
        tx: tokio::sync::mpsc::Sender<IncomingMessage>,
        max_connections: usize,
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
        let open_conns = Arc::new(AtomicUsize::new(0));
        let (disconnect_tx, disconnect_rx) = tokio::sync::mpsc::channel::<u64>(64);

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        if open_conns.load(Ordering::Relaxed) >= max_connections {
                            warn!(
                                "UDS connection limit ({max_connections}) reached — rejecting connection"
                            );
                            counter!("ipc_connections_rejected_total").increment(1);
                            drop(stream);
                            continue;
                        }
                        let conn_id = counter.fetch_add(1, Ordering::Relaxed);
                        let open = open_conns.clone();
                        open.fetch_add(1, Ordering::Relaxed);
                        let (handler, _write_tx) = ConnectionHandler::new(
                            conn_id,
                            stream,
                            tx.clone(),
                            disconnect_tx.clone(),
                        );
                        tokio::spawn(async move {
                            handler.run().await;
                            open.fetch_sub(1, Ordering::Relaxed);
                        });
                    }
                    // Accept errors (EMFILE/ENFILE/ECONNABORTED) are transient — keep
                    // the listener alive. A `while let Ok(..)` here would kill the whole
                    // accept loop on the first hiccup. Brief backoff avoids a busy spin
                    // when the cause persists (e.g. fd exhaustion).
                    Err(e) => {
                        warn!("UDS accept error: {e}; continuing");
                        counter!("ipc_accept_errors_total").increment(1);
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });

        Ok((handle, disconnect_rx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn uds_connection_limit_rejects_excess() {
        let sock_path = "/tmp/veyron_test_conn_limit.sock";
        let _ = std::fs::remove_file(sock_path);

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let (_handle, _disc) =
            UdsServer::start(std::path::Path::new(sock_path), tx, 1)
                .await
                .unwrap();

        // First connection: accepted (open_conns goes 0 → 1)
        let _c1 = UnixStream::connect(sock_path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Second connection: limit reached — server drops stream, peer sees EOF
        let mut c2 = UnixStream::connect(sock_path).await.unwrap();
        let mut buf = [0u8; 1];
        let n = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            c2.read(&mut buf),
        )
        .await
        .expect("read timed out")
        .expect("read error");
        assert_eq!(n, 0, "excess connection must receive EOF");

        // _c1 kept alive to hold the slot during the test
        drop(_c1);
        let _ = std::fs::remove_file(sock_path);
    }
}
