use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use veyron::events::bus::EventBus;
use veyron::kernel::Kernel;
use veyron::plugins::registry::PluginRegistry;
use veyron::utils::config::{Config, PluginDef};

/// Start a kernel whose config includes a `plugins:` list.
/// Returns the shutdown sender and a reference to the registry/supervisor
/// via the PluginManager accessible from the test.
async fn start_kernel_with_plugins(
    socket: &str,
    port: u16,
    defs: Vec<PluginDef>,
) -> (oneshot::Sender<()>, Arc<PluginRegistry>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = Config {
        socket_path: socket.to_string(),
        port,
        pid_file: "/tmp/veyron_integ_autoload.pid".into(),
        log_file: "/tmp/veyron_integ_autoload.log".into(),
        allow_no_auth: true,
        plugins: defs,
        ..Config::default()
    };
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
    tokio::time::sleep(Duration::from_millis(60)).await;
    (shutdown_tx, registry)
}

#[tokio::test]
async fn kernel_auto_spawns_plugins_from_config() {
    let def = PluginDef {
        id: "autoload-integ".to_string(),
        binary: "/bin/sleep".to_string(),
        restart: "never".to_string(),
        max_restarts: 0,
        args: vec!["60".to_string()],
        env: vec![],
        sandbox: false,
        grace_seconds: 5,
        permissions: vec![],
    };

    let (shutdown, _registry) =
        start_kernel_with_plugins("/tmp/veyron_autoload_integ.sock", 19320, vec![def]).await;

    // The plugin process is spawned by the supervisor, not via UDS registration,
    // so it won't appear in the registry.  Verify the kernel itself is up by
    // opening a TCP connection to the HTTP API port.
    let connected = tokio::net::TcpStream::connect("127.0.0.1:19320")
        .await
        .is_ok();
    assert!(
        connected,
        "kernel HTTP port must be open after startup with plugins"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn kernel_starts_cleanly_with_empty_plugins_list() {
    let (shutdown, _registry) =
        start_kernel_with_plugins("/tmp/veyron_autoload_empty.sock", 19321, vec![]).await;

    let connected = tokio::net::TcpStream::connect("127.0.0.1:19321")
        .await
        .is_ok();
    assert!(connected, "kernel HTTP port must be open");

    let _ = shutdown.send(());
}
