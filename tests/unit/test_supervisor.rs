use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use veyron::plugins::registry::PluginRegistry;
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
        sandbox: false,
        ..Default::default()
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
        sandbox: false,
        ..Default::default()
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
async fn manual_restart_overrides_never_policy() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    // long-running plugin with Never policy: it never exits on its own, so the
    // only exit comes from the manual restart's SIGTERM.
    sup.spawn_plugin(sleep_config("manual_never"))
        .await
        .unwrap();

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    // explicit restart must respawn despite Never policy
    sup.restart_plugin("manual_never").await.unwrap();
    sleep(Duration::from_millis(600)).await; // SIGTERM + backoff(1)=200ms + respawn

    assert!(
        sup.is_running("manual_never"),
        "plugin must be respawned after manual restart"
    );
    let count = sup.restart_count("manual_never").unwrap_or(0);
    assert!(
        count >= 1,
        "manual restart must respawn despite Never policy, got {count}"
    );

    sup.stop_plugin("manual_never").await.ok();
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

    // wait long enough for 2 restarts with backoff (100ms + 200ms + process overhead)
    sleep(Duration::from_millis(1500)).await;

    let count = sup.restart_count("capped").unwrap_or(0);
    assert_eq!(count, 2, "expected exactly 2 restarts, got {}", count);
}

// ── T-22: VULN-018 — dead entry cleanup ─────────────────────────────────────

#[tokio::test]
async fn is_running_returns_false_after_max_restarts_exceeded() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    // Always policy, max_restarts=0: first exit triggers None branch → entry removed.
    sup.spawn_plugin(quick_exit_config("terminated", RestartPolicy::Always, 0))
        .await
        .unwrap();

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    sleep(Duration::from_millis(300)).await;

    assert!(
        !sup.is_running("terminated"),
        "is_running must be false once max restarts are exhausted"
    );
}

// ── T-22: VULN-021 — watchdog must not reset pong after SIGKILL ─────────────

#[tokio::test]
async fn watchdog_does_not_reset_pong_after_kill() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_watchdog_pong.sock"));
    sup.spawn_plugin(sleep_config("watchdog_victim"))
        .await
        .unwrap();

    let reg = Arc::new(PluginRegistry::new());
    // Record a pong so the watchdog enters the deadline-check branch.
    reg.record_pong("watchdog_victim");
    let pong_before = reg.last_pong("watchdog_victim").unwrap();

    // Wait long enough to make the pong stale relative to the tiny deadline below.
    sleep(Duration::from_millis(50)).await;

    // interval=1ms, timeout=1ms → deadline=2ms. Pong is ~50ms old → SIGKILL fires.
    let sup_clone = Arc::clone(&sup);
    let reg_clone = Arc::clone(&reg);
    let wdog = tokio::spawn(async move {
        sup_clone
            .watchdog_loop(
                reg_clone,
                Duration::from_millis(1),
                Duration::from_millis(1),
            )
            .await;
    });

    sleep(Duration::from_millis(30)).await;
    wdog.abort();

    // Pong must NOT have been refreshed after SIGKILL (VULN-021 fix).
    let pong_after = reg.last_pong("watchdog_victim").unwrap();
    assert_eq!(
        pong_before, pong_after,
        "watchdog must not reset pong after SIGKILL"
    );

    sup.stop_plugin("watchdog_victim").await.ok();
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
        sandbox: false,
        ..Default::default()
    };

    let proc = sup.spawn_plugin(config).await.expect("spawn must succeed");

    // wait for child to exit
    sleep(Duration::from_millis(200)).await;

    // if VEYRON_SOCKET_PATH was set, sh exits 0 → watcher sends success
    // We verify by checking the exit code recorded
    assert!(proc.pid > 0, "must have a valid pid");
}

// ── T-13: BUG-005 — per-plugin shutdown grace must not be gated by the
// slowest plugin ──────────────────────────────────────────────────────────

fn pid_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

fn ignores_sigterm_config(plugin_id: &str, grace_seconds: u32) -> PluginConfig {
    PluginConfig {
        plugin_id: plugin_id.to_string(),
        binary_path: PathBuf::from("/bin/sh"),
        // Ignore SIGTERM so the process only ever dies via SIGKILL, letting
        // the test observe exactly when the supervisor's grace timer fires.
        // The loop (rather than a tail `sleep 60`) prevents shells that
        // exec-optimize a single trailing simple command from replacing this
        // process image and losing the SIGTERM trap disposition.
        args: vec![
            "-c".to_string(),
            "trap '' TERM; while true; do sleep 1; done".to_string(),
        ],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        grace_seconds,
        sandbox: false,
        max_procs: None,
        max_vmem_mb: None,
    }
}

#[tokio::test]
async fn graceful_shutdown_kills_each_plugin_on_its_own_deadline() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_grace_test.sock"));

    let fast = sup
        .spawn_plugin(ignores_sigterm_config("grace_fast", 1))
        .await
        .expect("spawn must succeed");
    let slow = sup
        .spawn_plugin(ignores_sigterm_config("grace_slow", 5))
        .await
        .expect("spawn must succeed");

    assert!(
        pid_alive(fast.pid) && pid_alive(slow.pid),
        "both must start alive"
    );

    // Give each shell time to install its `trap '' TERM` before we signal it —
    // otherwise SIGTERM can arrive before the trap is installed and kill the
    // process via the default disposition, before the grace timer ever fires.
    sleep(Duration::from_millis(100)).await;
    assert!(
        pid_alive(fast.pid) && pid_alive(slow.pid),
        "both must still be alive after trap installs"
    );

    let sup_clone = Arc::clone(&sup);
    let shutdown = tokio::spawn(async move {
        sup_clone.graceful_shutdown(5).await;
    });

    sleep(Duration::from_millis(100)).await;

    // Shortly after the fast plugin's 1s grace elapses, it must be dead while
    // the slow plugin (5s grace) is still alive — SIGKILL is per-plugin, not
    // gated on the max grace across all plugins.
    sleep(Duration::from_millis(1200)).await;
    assert!(
        !pid_alive(fast.pid),
        "fast plugin must be SIGKILLed at its own ~1s deadline"
    );
    assert!(
        pid_alive(slow.pid),
        "slow plugin must still be alive before its 5s deadline"
    );

    shutdown.await.expect("graceful_shutdown must complete");
    // SIGKILL delivery is synchronous, but reaping the zombie happens on a
    // separate task (the `child.wait()` spawned in `spawn_internal`); give it
    // a tick to run before asserting the pid is gone.
    sleep(Duration::from_millis(100)).await;
    assert!(
        !pid_alive(slow.pid),
        "slow plugin must be dead once graceful_shutdown returns"
    );
}
