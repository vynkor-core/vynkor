use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use veyron::events::bus::EventBus;
use veyron::kernel::Kernel;
use veyron::plugins::registry::PluginRegistry;
use veyron::utils::config::Config;

pub fn test_config(socket: &str, port: u16) -> Config {
    Config {
        socket_path: socket.to_string(),
        port,
        pid_file: "/tmp/veyron_integ_test.pid".into(),
        log_file: "/tmp/veyron_integ_test.log".into(),
        allow_no_auth: true, // tests exercise the no-auth path deliberately
        ..Config::default()
    }
}

/// Starts kernel in background; returns shutdown sender and shared registry.
/// Registry is a fresh one wired into the kernel.
pub async fn start_kernel(
    socket: &str,
    port: u16,
) -> (oneshot::Sender<()>, Arc<PluginRegistry>, Arc<EventBus>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = test_config(socket, port);

    let registry = Arc::new(PluginRegistry::new());
    let event_bus = Arc::new(EventBus::new());

    let reg = Arc::clone(&registry);
    let bus = Arc::clone(&event_bus);

    tokio::spawn(async move {
        Kernel::run_with_components(cfg, reg, bus, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });

    // wait for socket
    tokio::time::sleep(Duration::from_millis(30)).await;

    (shutdown_tx, registry, event_bus)
}

/// Like `start_kernel` but with JWT auth + frame MAC enabled (`jwt_secret` set).
pub async fn start_kernel_secured(
    socket: &str,
    port: u16,
    secret: &str,
) -> (oneshot::Sender<()>, Arc<PluginRegistry>, Arc<EventBus>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let mut cfg = test_config(socket, port);
    cfg.allow_no_auth = false;
    cfg.jwt_secret = Some(secret.to_string());

    let registry = Arc::new(PluginRegistry::new());
    let event_bus = Arc::new(EventBus::new());
    let reg = Arc::clone(&registry);
    let bus = Arc::clone(&event_bus);

    tokio::spawn(async move {
        Kernel::run_with_components(cfg, reg, bus, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    (shutdown_tx, registry, event_bus)
}
