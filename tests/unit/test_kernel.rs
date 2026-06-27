use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use veyron::kernel::Kernel;
use veyron::proto::veyron::PluginManifest;
use veyron::utils::config::Config;
use veyron_sdk::VeyronClient;

#[test]
fn pid_flock_prevents_double_start() {
    use nix::fcntl::{Flock, FlockArg};
    use std::fs;
    use std::path::Path;

    let pid_file = Path::new("/tmp/veyron_flock_double_start_test.pid");

    let h1 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(pid_file)
        .unwrap();
    let _lock = Flock::lock(h1, FlockArg::LockExclusiveNonblock).expect("first lock must succeed");

    let h2 = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(pid_file)
        .unwrap();
    let result = Flock::lock(h2, FlockArg::LockExclusiveNonblock);
    assert!(result.is_err(), "second lock must fail while first is held");

    let _ = fs::remove_file(pid_file);
}

fn test_config(socket: &str, port: u16) -> Config {
    Config {
        socket_path: socket.to_string(),
        port,
        pid_file: "/tmp/veyron_kernel_test.pid".into(),
        log_file: "/tmp/veyron_kernel_test.log".into(),
        allow_no_auth: true, // tests exercise the no-auth path deliberately
        ..Config::default()
    }
}

#[tokio::test]
async fn kernel_refuses_to_start_without_auth() {
    let mut cfg = test_config("/tmp/veyron_kernel_noauth.sock", 19199);
    // no JWT secret and no explicit opt-out → must refuse
    cfg.jwt_secret = None;
    cfg.allow_no_auth = false;

    let result = Kernel::run_with_shutdown(cfg, async {}).await;
    assert!(
        result.is_err(),
        "kernel must refuse to start without auth or an explicit opt-out"
    );
}

#[tokio::test]
async fn kernel_starts_and_accepts_plugin_registration() {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = test_config("/tmp/veyron_kernel_a11_reg.sock", 19100);

    tokio::spawn(async move {
        Kernel::run_with_shutdown(cfg, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });

    // wait for socket
    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut client = timeout(
        Duration::from_secs(2),
        VeyronClient::connect("/tmp/veyron_kernel_a11_reg.sock"),
    )
    .await
    .expect("connect timed out")
    .expect("connect failed");

    let ack = timeout(
        Duration::from_secs(2),
        client.register("hello_plugin", PluginManifest::default()),
    )
    .await
    .expect("register timed out")
    .expect("register failed");

    assert!(ack.accepted, "plugin registration must be accepted");

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_ping_pong_works_after_registration() {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = test_config("/tmp/veyron_kernel_a11_ping.sock", 19101);

    tokio::spawn(async move {
        Kernel::run_with_shutdown(cfg, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    let mut client = VeyronClient::connect("/tmp/veyron_kernel_a11_ping.sock")
        .await
        .unwrap();
    client
        .register("pinger", PluginManifest::default())
        .await
        .unwrap();

    let rtt = timeout(Duration::from_secs(2), client.ping())
        .await
        .expect("ping timed out")
        .expect("ping failed");

    assert!(rtt < Duration::from_secs(1));

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_graceful_shutdown_does_not_panic() {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = test_config("/tmp/veyron_kernel_a11_shutdown.sock", 19102);

    let handle = tokio::spawn(async move {
        Kernel::run_with_shutdown(cfg, async {
            let _ = shutdown_rx.await;
        })
        .await
    });

    tokio::time::sleep(Duration::from_millis(30)).await;

    // connect and register a plugin so shutdown has something to notify
    let mut client = VeyronClient::connect("/tmp/veyron_kernel_a11_shutdown.sock")
        .await
        .unwrap();
    client
        .register("shutdown_test", PluginManifest::default())
        .await
        .unwrap();

    let _ = shutdown_tx.send(());

    // kernel task must finish without panic
    let result = timeout(Duration::from_secs(5), handle)
        .await
        .expect("kernel did not shut down in time");
    assert!(result.is_ok());
    assert!(result.unwrap().is_ok());
}
