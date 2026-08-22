use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use vynkor::events::bus::EventBus;
use vynkor::kernel::Kernel;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::proto::vynkor::PluginManifest;
use vynkor::utils::config::Config;
use vynkor_sdk::VynkorClient;

pub fn test_config(socket: &str, port: u16) -> Config {
    Config {
        socket_path: socket.to_string(),
        port,
        pid_file: "/tmp/vynkor_integ_test.pid".into(),
        log_file: "/tmp/vynkor_integ_test.log".into(),
        allow_no_auth: true, // tests exercise the no-auth path deliberately
        tls: false,          // tests hit the plain-HTTP/WS path
        ..Config::default()
    }
}

/// Starts kernel in background; returns shutdown sender and shared registry.
/// Registry is a fresh one wired into the kernel.
pub async fn start_kernel(
    socket: &str,
    port: u16,
) -> (oneshot::Sender<()>, Arc<PluginRegistry>, Arc<EventBus>) {
    start_kernel_with_config(test_config(socket, port)).await
}

/// Like `start_kernel` but takes a caller-built `Config`, for tests that need
/// to override a field `test_config` doesn't parameterize (e.g. `max_ws_connections`).
pub async fn start_kernel_with_config(
    cfg: Config,
) -> (oneshot::Sender<()>, Arc<PluginRegistry>, Arc<EventBus>) {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

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

/// Soak stats collected across all worker tasks.
pub struct SoakStats {
    pub total_pings: u64,
    pub successful_pings: u64,
    pub panics: u64,
}

/// Run a stability soak for `duration_secs` seconds against a kernel on `socket`.
///
/// Spawns `SOAK_WORKERS` concurrent tasks; each loops: connect → register → ping
/// until the deadline, then disconnects. Counters are shared via atomics.
pub async fn run_soak(socket: &str, port: u16, duration_secs: u64) -> SoakStats {
    const SOAK_WORKERS: usize = 8;

    let (shutdown_tx, _registry, _bus) = start_kernel(socket, port).await;

    let total = Arc::new(AtomicU64::new(0));
    let success = Arc::new(AtomicU64::new(0));
    let panics = Arc::new(AtomicU64::new(0));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);

    let mut handles = Vec::new();
    for i in 0..SOAK_WORKERS {
        let sock = socket.to_string();
        let tot = Arc::clone(&total);
        let suc = Arc::clone(&success);
        let _pan = Arc::clone(&panics);

        let handle = tokio::spawn(async move {
            let plugin_id = format!("soak-worker-{i}");
            loop {
                if tokio::time::Instant::now() >= deadline {
                    break;
                }
                let mut client = match VynkorClient::connect(&sock).await {
                    Ok(c) => c,
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                };
                let _ = client.register(&plugin_id, PluginManifest::default()).await;

                while tokio::time::Instant::now() < deadline {
                    tot.fetch_add(1, Ordering::Relaxed);
                    match tokio::time::timeout(Duration::from_secs(2), client.ping()).await {
                        Ok(Ok(_)) => {
                            suc.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => break, // reconnect on error or timeout
                    }
                }
            }
        });
        handles.push(handle);
    }

    for h in handles {
        if h.await.is_err() {
            panics.fetch_add(1, Ordering::Relaxed);
        }
    }

    let _ = shutdown_tx.send(());

    SoakStats {
        total_pings: total.load(Ordering::Relaxed),
        successful_pings: success.load(Ordering::Relaxed),
        panics: panics.load(Ordering::Relaxed),
    }
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
