use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use vynkor::events::bus::EventBus;
use vynkor::kernel::Kernel;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::utils::config::Config;

static NEXT_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(29900);

/// Allocates a unique port for each test to avoid socket conflicts.
pub fn alloc_port() -> u16 {
    NEXT_PORT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A live kernel instance for SDK integration tests.
/// Kernel is shut down when this value is dropped (via `shutdown_tx`).
#[allow(dead_code)]
pub struct SdkHarness {
    pub socket_path: PathBuf,
    pub port: u16,
    pub jwt_secret: String,
    pub registry: Arc<PluginRegistry>,
    pub event_bus: Arc<EventBus>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl SdkHarness {
    /// Start a kernel with no auth on a unique socket path and port.
    pub async fn start() -> Self {
        let port = alloc_port();
        let socket_path = PathBuf::from(format!("/tmp/veyron_sdk_test_{port}.sock"));
        Self::start_at(socket_path, port, None).await
    }

    /// Start a kernel with JWT auth.
    #[allow(dead_code)]
    pub async fn start_secured(secret: &str) -> Self {
        let port = alloc_port();
        let socket_path = PathBuf::from(format!("/tmp/veyron_sdk_sec_{port}.sock"));
        Self::start_at(socket_path, port, Some(secret.to_string())).await
    }

    async fn start_at(socket_path: PathBuf, port: u16, jwt_secret: Option<String>) -> Self {
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        let socket_str = socket_path.to_string_lossy().to_string();
        let cfg = Config {
            socket_path: socket_str,
            port,
            pid_file: format!("/tmp/veyron_sdk_{port}.pid").into(),
            log_file: format!("/tmp/veyron_sdk_{port}.log").into(),
            allow_no_auth: jwt_secret.is_none(),
            jwt_secret: jwt_secret.clone(),
            ..Config::default()
        };

        let registry = Arc::new(PluginRegistry::new());
        let event_bus = Arc::new(EventBus::new());

        tokio::spawn(Kernel::run_with_components(
            cfg,
            registry.clone(),
            event_bus.clone(),
            async {
                let _ = shutdown_rx.await;
            },
        ));

        // Wait for the UDS socket to be available.
        tokio::time::sleep(Duration::from_millis(40)).await;

        SdkHarness {
            socket_path,
            port,
            jwt_secret: jwt_secret.unwrap_or_default(),
            registry,
            event_bus,
            shutdown_tx: Some(shutdown_tx),
        }
    }
}

impl Drop for SdkHarness {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}
