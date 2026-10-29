use std::net::TcpListener;
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use vynkor::bridge::BridgeStatus;
use vynkor::embed::{self, InstanceLockError};
use vynkor::utils::config::Config;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn host_config(dir: &tempfile::TempDir) -> Config {
    Config {
        port: free_port(),
        allow_no_auth: true,
        pid_file: dir.path().join("vyn.pid"),
        socket_path: dir.path().join("vyn.sock").display().to_string(),
        data_dir: dir.path().join("data"),
        plugins: vec![],
        ..Default::default()
    }
}

#[tokio::test]
async fn embedded_host_runs_and_stops_on_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = host_config(&dir);
    let pid_file = cfg.pid_file.clone();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (status_tx, status_rx) = watch::channel(BridgeStatus::Connecting {
        transport: vynkor::bridge::TransportKind::Direct,
    });

    let task = tokio::spawn(embed::run(
        cfg,
        async {
            let _ = stop_rx.await;
        },
        Some(status_tx),
    ));

    // host role: bridge is disabled, and the pid lock is held while running
    let mut rx = status_rx.clone();
    tokio::time::timeout(
        Duration::from_secs(10),
        rx.wait_for(|s| *s == BridgeStatus::Disabled),
    )
    .await
    .expect("status never became Disabled")
    .unwrap();
    assert!(pid_file.exists(), "pid file must exist while running");

    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("embed::run did not return after shutdown")
        .unwrap()
        .expect("embed::run returned an error");
    assert!(!pid_file.exists(), "pid file must be gone after shutdown");
}

#[tokio::test]
async fn second_embedded_kernel_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = host_config(&dir);
    let second_cfg = Config {
        port: free_port(),
        ..cfg.clone()
    };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (status_tx, mut status_rx) = watch::channel(BridgeStatus::Connecting {
        transport: vynkor::bridge::TransportKind::Direct,
    });
    let first = tokio::spawn(embed::run(
        cfg,
        async {
            let _ = stop_rx.await;
        },
        Some(status_tx),
    ));
    tokio::time::timeout(
        Duration::from_secs(10),
        status_rx.wait_for(|s| *s == BridgeStatus::Disabled),
    )
    .await
    .expect("first kernel never started")
    .unwrap();

    let err = embed::run(second_cfg, std::future::pending::<()>(), None)
        .await
        .expect_err("second kernel must be refused");
    assert!(
        matches!(
            err.downcast_ref::<InstanceLockError>(),
            Some(InstanceLockError::AlreadyRunning { .. })
        ),
        "{err:#}"
    );

    stop_tx.send(()).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(10), first).await;
}
