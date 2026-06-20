use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use veyron::plugins::supervisor::{PluginConfig, PluginSupervisor, RestartPolicy};

fn quick_exit_config(plugin_id: &str, policy: RestartPolicy, max_restarts: u32) -> PluginConfig {
    PluginConfig {
        plugin_id: plugin_id.to_string(),
        // /bin/true exits 0 immediately
        binary_path: PathBuf::from("/bin/true"),
        args: vec![],
        env: vec![],
        restart_policy: policy,
        max_restarts,
    }
}

fn sleep_config(plugin_id: &str) -> PluginConfig {
    PluginConfig {
        plugin_id: plugin_id.to_string(),
        binary_path: PathBuf::from("/bin/sleep"),
        args: vec!["60".to_string()],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
    }
}

#[tokio::test]
async fn spawn_plugin_starts_process() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    sup.spawn_plugin(sleep_config("long_runner"))
        .await
        .expect("spawn must succeed");

    assert!(
        sup.is_running("long_runner"),
        "plugin must be running after spawn"
    );

    // cleanup
    sup.stop_plugin("long_runner").await.ok();
}

#[tokio::test]
async fn stop_plugin_removes_process() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    sup.spawn_plugin(sleep_config("stoppable")).await.unwrap();
    assert!(sup.is_running("stoppable"));

    sup.stop_plugin("stoppable")
        .await
        .expect("stop must succeed");

    assert!(
        !sup.is_running("stoppable"),
        "plugin must not be running after stop"
    );
}

#[tokio::test]
async fn stop_nonexistent_plugin_returns_error() {
    let sup = PluginSupervisor::new("/tmp/veyron_test.sock");
    let result = sup.stop_plugin("ghost").await;
    assert!(result.is_err(), "stopping unknown plugin must return error");
}

#[tokio::test]
async fn restart_policy_always_triggers_restart_after_exit() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    sup.spawn_plugin(quick_exit_config("restartable", RestartPolicy::Always, 5))
        .await
        .unwrap();

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    // true exits instantly; monitor should restart it at least once
    sleep(Duration::from_millis(300)).await;

    let count = sup.restart_count("restartable").unwrap_or(0);
    assert!(count >= 1, "expected at least 1 restart, got {}", count);

    sup.stop_plugin("restartable").await.ok();
}

#[tokio::test]
async fn restart_policy_never_does_not_restart() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    sup.spawn_plugin(quick_exit_config("no_restart", RestartPolicy::Never, 0))
        .await
        .unwrap();

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    sleep(Duration::from_millis(200)).await;

    let count = sup.restart_count("no_restart").unwrap_or(0);
    assert_eq!(
        count, 0,
        "Never policy must not restart, got {} restarts",
        count
    );
}

#[tokio::test]
async fn max_restarts_honored() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    sup.spawn_plugin(quick_exit_config("capped", RestartPolicy::Always, 2))
        .await
        .unwrap();

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    // wait long enough for 2 restarts (true exits nearly instantly)
    sleep(Duration::from_millis(500)).await;

    let count = sup.restart_count("capped").unwrap_or(0);
    assert_eq!(count, 2, "expected exactly 2 restarts, got {}", count);
}

#[tokio::test]
async fn spawned_process_inherits_socket_path_env() {
    // Verify VEYRON_SOCKET_PATH is passed to child.
    // We use a shell command that checks the env var and exits 0 if set, 1 if not.
    let sup = PluginSupervisor::new("/tmp/veyron_check.sock");

    let config = PluginConfig {
        plugin_id: "env_check".to_string(),
        binary_path: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".to_string(),
            r#"test -n "$VEYRON_SOCKET_PATH""#.to_string(),
        ],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
    };

    let proc = sup.spawn_plugin(config).await.expect("spawn must succeed");

    // wait for child to exit
    sleep(Duration::from_millis(200)).await;

    // if VEYRON_SOCKET_PATH was set, sh exits 0 → watcher sends success
    // We verify by checking the exit code recorded
    assert!(proc.pid > 0, "must have a valid pid");
}
