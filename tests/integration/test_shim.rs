//! R9-02: PID-namespace isolation through `vyn __shim`.
//!
//! The supervisor cannot place a plugin into a PID namespace from its own
//! spawn path (a pending `pid_for_children` namespace breaks thread creation),
//! so sandboxed plugins are re-exec'd through our own binary with the hidden
//! `__shim` subcommand. These tests drive `PluginSupervisor` directly and
//! assert the three things the shim must provide: the plugin really is PID 1
//! in its own namespace, lifecycle signals reach it through the shim, and the
//! exit status is mirrored so supervision (restart) still works.
//!
//! All three need Linux + unprivileged user namespaces; each test probes once
//! with a `/bin/true` sandbox spawn and skips where the host cannot sandbox.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use veyron::plugins::supervisor::{PluginConfig, PluginSupervisor, RestartPolicy};

const VYN: &str = env!("CARGO_BIN_EXE_vyn");

fn python3_bin() -> Option<String> {
    for name in ["python3", "python"] {
        if std::process::Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return Some(name.to_string());
        }
    }
    None
}

/// sandboxed plugins run through `vyn __shim`; point the supervisor at the
/// real binary instead of the test harness (which has no `__shim`)
fn set_shim_bin() {
    std::env::set_var("VEYRON_SHIM_BIN", VYN);
}

async fn sandbox_available(sup: &PluginSupervisor) -> bool {
    let probe = PluginConfig {
        plugin_id: "shim_probe".to_string(),
        binary_path: PathBuf::from("/bin/true"),
        args: vec![],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        ..Default::default()
    };
    if sup.spawn_plugin(probe).await.is_err() {
        return false;
    }
    let _ = sup.stop_plugin("shim_probe").await;
    true
}

async fn wait_for_log(sup: &PluginSupervisor, id: &str, needle: &str) -> Vec<String> {
    for _ in 0..50 {
        let logs = sup.get_logs(id, 50).await;
        if logs.iter().any(|l| l.contains(needle)) {
            return logs;
        }
        sleep(Duration::from_millis(100)).await;
    }
    sup.get_logs(id, 50).await
}

/// The plugin must be PID 1 of its namespace and `/proc` must show only its
/// own task — no host or other-plugin processes visible.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_sees_only_its_own_pid_namespace() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_shim_pidns.sock"));
    if !sandbox_available(&sup).await {
        eprintln!("skipping: unprivileged user namespaces unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "shim_pidns".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import os,time; \
             print(f'getpid={os.getpid()}', flush=True); \
             procs=[p for p in os.listdir('/proc') if p.isdigit()]; \
             print(f'procs={sorted(procs)}', flush=True); \
             time.sleep(60)"
                .to_string(),
        ],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        // the script installs no SIGTERM handler, so the final stop_plugin
        // relies on the shim escalating TERM→SIGKILL — keep the grace small
        grace_seconds: 1,
        ..Default::default()
    };
    let proc = sup
        .spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");

    let logs = wait_for_log(&sup, "shim_pidns", "getpid=").await;
    assert!(
        logs.iter().any(|l| l.contains("getpid=1")),
        "plugin must be PID 1 inside its namespace, logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("procs=['1']")),
        "namespace /proc must list only the plugin itself, logs: {logs:?}"
    );

    // the plugin's host pid is real and large — the namespace pid is 1
    assert!(
        proc.pid > 10,
        "host pid must not be 1 (that would be the init process), got {}",
        proc.pid
    );
    let _ = sup.stop_plugin("shim_pidns").await;
}

/// SIGTERM goes to the shim (`signal_target`), which must forward it to the
/// plugin — a plugin that handles SIGTERM must observe it and exit cleanly.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn shim_forwards_sigterm_to_sandboxed_plugin() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_shim_sig.sock"));
    if !sandbox_available(&sup).await {
        eprintln!("skipping: unprivileged user namespaces unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "shim_sig".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import signal,sys,time; \
             def h(s,f): print('got SIGTERM', flush=True); sys.exit(0); \
             signal.signal(signal.SIGTERM, h); \
             print('ready', flush=True); \
             time.sleep(60)"
                .to_string(),
        ],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        ..Default::default()
    };
    let proc = sup
        .spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");

    let ready = wait_for_log(&sup, "shim_sig", "ready").await;
    assert!(
        ready.iter().any(|l| l.contains("ready")),
        "plugin must start before the signal test, logs: {ready:?}"
    );

    sup.stop_plugin("shim_sig")
        .await
        .expect("stop must succeed");
    let logs = wait_for_log(&sup, "shim_sig", "got SIGTERM").await;
    assert!(
        logs.iter().any(|l| l.contains("got SIGTERM")),
        "SIGTERM sent to the shim must reach the plugin, logs: {logs:?}"
    );

    // the plugin must actually die — kill(pid, 0) eventually reports ESRCH
    for _ in 0..30 {
        let alive =
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(proc.pid as i32), None).is_ok();
        if !alive {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "plugin pid {} still alive after forwarded SIGTERM",
        proc.pid
    );
}

/// A plugin exiting with a non-zero code must have that status mirrored by
/// the shim, so the supervisor sees the death and honours restart policy.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn shim_reports_exit_status_for_supervision() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_shim_exit.sock"));
    if !sandbox_available(&sup).await {
        eprintln!("skipping: unprivileged user namespaces unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "shim_exit".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), "import sys; sys.exit(3)".to_string()],
        env: vec![],
        restart_policy: RestartPolicy::Always,
        max_restarts: 5,
        sandbox: true,
        ..Default::default()
    };
    sup.spawn_plugin(config).await.expect("spawn must succeed");

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });

    // exits instantly with 3; the shim-mirrored status must trigger a restart
    for _ in 0..50 {
        if sup.restart_count("shim_exit").unwrap_or(0) >= 1 {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        sup.restart_count("shim_exit").unwrap_or(0) >= 1,
        "supervisor must observe the shim-mirrored exit status and restart"
    );

    let _ = sup.stop_plugin("shim_exit").await;
}
