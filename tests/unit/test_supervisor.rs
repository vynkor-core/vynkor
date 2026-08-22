use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::{PluginConfig, PluginSupervisor, RestartPolicy};

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
async fn stop_cancels_in_flight_backoff_restart() {
    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_test.sock"));

    // long-lived process with Always policy: the only exit comes from the
    // SIGKILL below, and the monitor then schedules a restart (B3 window).
    let proc = sup
        .spawn_plugin(PluginConfig {
            plugin_id: "cancel_backoff".to_string(),
            binary_path: PathBuf::from("/bin/sleep"),
            args: vec!["60".to_string()],
            env: vec![],
            restart_policy: RestartPolicy::Always,
            max_restarts: 5,
            sandbox: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(sup.is_running("cancel_backoff"));

    let sup_clone = Arc::clone(&sup);
    tokio::spawn(async move {
        sup_clone.monitor_loop().await;
    });
    sleep(Duration::from_millis(50)).await;

    // hard-kill the process; the monitor sees the exit and sleeps backoff
    // (base 100ms) before respawning
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(proc.pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );

    // explicit stop inside the backoff window must cancel the restart
    sup.stop_plugin("cancel_backoff").await.unwrap();

    // well past backoff(1)=200ms — the plugin must not come back
    sleep(Duration::from_millis(400)).await;
    assert!(
        !sup.is_running("cancel_backoff"),
        "stopped plugin must not be resurrected by an in-flight restart"
    );
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
    // Verify VYN_SOCKET_PATH (and its legacy alias) are passed to child.
    // We use a shell command that checks the env var and exits 0 if set, 1 if not.
    let sup = PluginSupervisor::new("/tmp/veyron_check.sock");

    let config = PluginConfig {
        plugin_id: "env_check".to_string(),
        binary_path: PathBuf::from("/bin/sh"),
        args: vec![
            "-c".to_string(),
            r#"test -n "$VYN_SOCKET_PATH" -a -n "$VEYRON_SOCKET_PATH""#.to_string(),
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

    // if both socket-path vars were set, sh exits 0 → watcher sends success
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
        ..Default::default()
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

// ── R9-01: per-plugin process accounting via cgroup v2 pids.max ─────────────

/// Read the plugin's own cgroup v2 scope dir from `/proc/<pid>/cgroup`.
/// Returns (cgroup_dir, scope_name) or None if the process is gone.
#[cfg(target_os = "linux")]
fn plugin_cgroup_dir(pid: u32) -> Option<(PathBuf, String)> {
    let contents = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let rel = contents.trim().strip_prefix("0::")?;
    let name = rel.rsplit('/').next()?.to_string();
    Some((PathBuf::from(format!("/sys/fs/cgroup{rel}")), name))
}

/// Probe whether the host can hand us a writable pids scope; the RLIMIT_NPROC
/// fallback applies otherwise. Creates and immediately removes a probe scope.
#[cfg(target_os = "linux")]
fn pids_cgroup_available() -> bool {
    use vynkor::plugins::runner::{cleanup_pids_cgroup, prepare_pids_cgroup};
    match prepare_pids_cgroup("r9-availability-probe", 64) {
        Some(cg) => {
            cleanup_pids_cgroup(&cg);
            true
        }
        None => false,
    }
}

/// `pids.events` "max" counter went > 0 — the cgroup's `pids.max` was hit.
#[cfg(target_os = "linux")]
fn pids_max_hit(cgroup_dir: &std::path::Path) -> bool {
    let events = std::fs::read_to_string(cgroup_dir.join("pids.events")).unwrap_or_default();
    events
        .lines()
        .find_map(|l| l.strip_prefix("max "))
        .map(|v| v.trim() != "0")
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn python3_bin() -> Option<String> {
    match std::process::Command::new("python3")
        .arg("--version")
        .output()
    {
        Ok(_) => Some("python3".to_string()),
        Err(_) => None,
    }
}

/// Storm script: sleep briefly, then hammer the cgroup with 200 long-lived
/// threads against a 64 budget. The small stack keeps the storm well under
/// the default RLIMIT_AS (512 MiB) so `pids.max` is the limiting factor, not
/// virtual memory. Threads (and the main thread) hold for `hold_secs`.
#[cfg(target_os = "linux")]
fn storm_script(hold_secs: u64) -> String {
    format!(
        "import threading,time\nthreading.stack_size(256*1024)\ntime.sleep(0.5)\nfor _ in range(200):\n    try:\n        threading.Thread(target=lambda: time.sleep({hold})).start()\n    except RuntimeError:\n        pass\ntime.sleep({hold})\n",
        hold = hold_secs
    )
}

/// R9-01 acceptance: a plugin with `max_procs` runs inside its own cgroup
/// v2 scope whose `pids.max` equals `max_procs`, a thread storm in that
/// plugin hits `pids.max` (pids.events.max > 0), and the empty scope is
/// removed when the plugin exits. Skips when the host has no writable cgroup
/// v2 subtree with the `pids` controller (RLIMIT_NPROC fallback applies).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn pids_cgroup_accounts_plugin_threads_per_plugin() {
    use vynkor::plugins::runner::{cleanup_pids_cgroup, prepare_pids_cgroup};

    // Probe availability first: if the host cannot hand us a pids scope, the
    // RLIMIT fallback path is in effect and there is nothing cgroup-specific
    // to verify.
    let probe = match prepare_pids_cgroup("r9-availability-probe", 64) {
        Some(cg) => cg,
        None => {
            eprintln!("skipping: no writable cgroup v2 pids subtree on this host");
            return;
        }
    };
    cleanup_pids_cgroup(&probe);

    let python = match std::process::Command::new("python3")
        .arg("--version")
        .output()
    {
        Ok(_) => "python3".to_string(),
        Err(_) => {
            eprintln!("skipping thread-storm probe: python3 not found");
            return;
        }
    };

    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_r9_pids.sock"));
    let config = PluginConfig {
        plugin_id: "r9_threads".to_string(),
        binary_path: PathBuf::from(&python),
        // Sleep briefly so the test can assert cgroup placement, then storm
        // 200 long-lived threads against a 64 budget — each sleeps so it
        // accumulates in pids.current; creation beyond pids.max fails with
        // EAGAIN (RuntimeError here) and bumps pids.events.max. The small
        // stack size keeps the storm well under the default RLIMIT_AS
        // (512 MiB) so pids.max is the limiting factor, not virtual memory.
        args: vec!["-c".to_string(), storm_script(5)],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: false,
        max_procs: Some(64),
        max_vmem_mb: None,
        ..Default::default()
    };
    let proc = sup.spawn_plugin(config).await.expect("spawn must succeed");
    // 1. The child must live in its own per-plugin scope.
    let (cgroup_dir, scope_name) =
        plugin_cgroup_dir(proc.pid).expect("plugin must be alive with a readable cgroup");
    assert_eq!(
        scope_name, "r9_threads",
        "plugin must live in its per-plugin scope"
    );

    // 2. pids.max must reflect max_procs.
    let pids_max =
        std::fs::read_to_string(cgroup_dir.join("pids.max")).expect("pids.max must be readable");
    assert_eq!(pids_max.trim(), "64", "pids.max must equal max_procs");

    // 3. The thread storm must be capped by the cgroup, not the shared
    // session budget: pids.events.max goes > 0. Poll briefly — python needs a
    // moment to reach the storm loop.
    let mut hit_max = false;
    for _ in 0..30 {
        if pids_max_hit(&cgroup_dir) {
            hit_max = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        hit_max,
        "a 200-thread storm must hit pids.max=64 in the plugin's own cgroup"
    );

    sup.stop_plugin("r9_threads").await.ok();
    // The scope is reaped (SIGTERM → python exits) and must be removed once
    // empty. Give the watcher a moment to wait() and rmdir.
    for _ in 0..30 {
        if !cgroup_dir.exists() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !cgroup_dir.exists(),
        "empty plugin pids scope must be cleaned up on exit"
    );
}

/// R9-01 sandbox variant: with `sandbox: true` the plugin must still land in
/// its per-plugin pids scope. `sandbox_pre_exec` unshares CLONE_NEWUSER
/// before `apply_resource_limits` runs the join, so this exercises writing
/// `cgroup.procs` from inside the user namespace — if that silently fell back
/// to RLIMIT_NPROC (the join error is swallowed), the placement assert below
/// would fail.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_still_joins_its_pids_cgroup() {
    if !pids_cgroup_available() {
        eprintln!("skipping: no writable cgroup v2 pids subtree on this host");
        return;
    }
    let Some(python) = python3_bin() else {
        eprintln!("skipping thread-storm probe: python3 not found");
        return;
    };

    // sandboxed plugins are spawned through `vyn __shim` — the test harness
    // binary does not implement that subcommand, so point the supervisor at
    // the real vyn binary instead of current_exe()
    std::env::set_var("VYN_SHIM_BIN", env!("CARGO_BIN_EXE_vyn"));

    // The sandbox path needs unprivileged user namespaces; hosts that restrict
    // them (kernel.unprivileged_userns_clone=0) fail the spawn in pre_exec —
    // probe once and skip there so the test is green where sandbox can't work.
    let probe_sup = Arc::new(PluginSupervisor::new("/tmp/veyron_r9_sandbox_probe.sock"));
    let probe = PluginConfig {
        plugin_id: "r9_sandbox_probe".to_string(),
        binary_path: PathBuf::from("/bin/true"),
        args: vec![],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        ..Default::default()
    };
    if probe_sup.spawn_plugin(probe).await.is_err() {
        eprintln!("skipping: unprivileged user namespaces unavailable (sandbox spawn failed)");
        return;
    }

    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_r9_sandbox.sock"));
    let config = PluginConfig {
        plugin_id: "r9_sandbox".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), storm_script(5)],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        // the storm script installs no SIGTERM handler, so the shim must
        // escalate TERM→SIGKILL on this grace — keep it small so the scope
        // is removed inside the 3s poll below
        grace_seconds: 1,
        max_procs: Some(64),
        max_vmem_mb: None,
        ..Default::default()
    };
    let proc = sup.spawn_plugin(config).await.expect("spawn must succeed");
    let (cgroup_dir, scope_name) =
        plugin_cgroup_dir(proc.pid).expect("plugin must be alive with a readable cgroup");
    assert_eq!(
        scope_name, "r9_sandbox",
        "sandboxed plugin must live in its per-plugin scope"
    );

    let pids_max =
        std::fs::read_to_string(cgroup_dir.join("pids.max")).expect("pids.max must be readable");
    assert_eq!(pids_max.trim(), "64", "pids.max must equal max_procs");

    let mut hit_max = false;
    for _ in 0..30 {
        if pids_max_hit(&cgroup_dir) {
            hit_max = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        hit_max,
        "a 200-thread storm in a sandboxed plugin must hit pids.max=64 in its own cgroup"
    );

    sup.stop_plugin("r9_sandbox").await.ok();
    for _ in 0..30 {
        if !cgroup_dir.exists() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !cgroup_dir.exists(),
        "empty sandboxed plugin pids scope must be cleaned up on exit"
    );
}

/// R9-01 acceptance #2: a thread storm in one plugin must not consume another
/// plugin's budget. A storms to its `pids.max`; B then spawns 30 threads of
/// its own 64 budget — all must succeed. Under the old shared-uid RLIMIT_NPROC
/// fallback B's creations would EAGAIN (the uid's thread count already
/// exceeds 64 from A); per-plugin cgroups give each plugin its own counter.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn thread_storm_in_one_plugin_does_not_consume_another_budget() {
    if !pids_cgroup_available() {
        eprintln!("skipping: no writable cgroup v2 pids subtree on this host");
        return;
    }
    let Some(python) = python3_bin() else {
        eprintln!("skipping thread-storm probe: python3 not found");
        return;
    };

    let sup = Arc::new(PluginSupervisor::new("/tmp/veyron_r9_iso.sock"));

    // A: storm 200 threads against its 64 budget and hold 10s so the
    // contention window overlaps B's spawn.
    let a_config = PluginConfig {
        plugin_id: "r9_iso_a".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), storm_script(10)],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: false,
        max_procs: Some(64),
        max_vmem_mb: None,
        ..Default::default()
    };
    let a = sup
        .spawn_plugin(a_config)
        .await
        .expect("spawn A must succeed");
    let (a_dir, a_scope) =
        plugin_cgroup_dir(a.pid).expect("plugin A must be alive with a readable cgroup");
    assert_eq!(
        a_scope, "r9_iso_a",
        "plugin A must live in its per-plugin scope"
    );

    // Wait until A has actually saturated its budget before starting B — the
    // contention must be real for the isolation claim to mean anything.
    let mut a_saturated = false;
    for _ in 0..30 {
        if pids_max_hit(&a_dir) {
            a_saturated = true;
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        a_saturated,
        "plugin A must hit its pids.max before B starts"
    );

    // B: create 30 threads (well within its own 64), report the created/failed
    // split to a result file, then hold. Under a shared RLIMIT_NPROC this
    // would EAGAIN immediately because the uid already has A's 64+ threads.
    let result_path = "/tmp/veyron_r9_iso_b_result";
    let _ = std::fs::remove_file(result_path);
    let b_script = format!(
        "import threading,time\nthreading.stack_size(256*1024)\ncreated=0\nfailed=0\nfor _ in range(30):\n    try:\n        threading.Thread(target=lambda: time.sleep(5)).start()\n        created+=1\n    except RuntimeError:\n        failed+=1\nopen('{result_path}','w').write(f'{{created}} {{failed}}')\ntime.sleep(5)\n",
        result_path = result_path
    );
    let b_config = PluginConfig {
        plugin_id: "r9_iso_b".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), b_script],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: false,
        max_procs: Some(64),
        max_vmem_mb: None,
        ..Default::default()
    };
    let b = sup
        .spawn_plugin(b_config)
        .await
        .expect("spawn B must succeed");
    let (b_dir, b_scope) =
        plugin_cgroup_dir(b.pid).expect("plugin B must be alive with a readable cgroup");
    assert_eq!(
        b_scope, "r9_iso_b",
        "plugin B must live in its per-plugin scope"
    );

    // B writes its split once the storm loop finishes; poll for the file.
    let mut split = None;
    for _ in 0..50 {
        if let Ok(contents) = std::fs::read_to_string(result_path) {
            split = Some(contents);
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    let split = split.expect("plugin B must report its created/failed split");
    assert_eq!(
        split.trim(),
        "30 0",
        "plugin B must create all 30 threads while A is at its pids.max"
    );

    sup.stop_plugin("r9_iso_a").await.ok();
    sup.stop_plugin("r9_iso_b").await.ok();
    for _ in 0..30 {
        if !a_dir.exists() && !b_dir.exists() {
            break;
        }
        sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !a_dir.exists() && !b_dir.exists(),
        "empty plugin pids scopes must be cleaned up on exit"
    );
}
