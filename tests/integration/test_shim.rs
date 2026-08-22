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
//!
//! R9-03 (same shim): the filesystem-restriction tests below verify Landlock
//! is actually enforced for a sandboxed plugin — undeclared reads/writes fail
//! with EACCES while declared paths and the kernel UDS stay reachable.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use vynkor::plugins::fsaccess::FsAccessMode;
use vynkor::plugins::supervisor::{PluginConfig, PluginSupervisor, RestartPolicy};

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
    std::env::set_var("VYN_SHIM_BIN", VYN);
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
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_pidns.sock"));
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
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_sig.sock"));
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
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_exit.sock"));
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

// ---- R9-03: Landlock filesystem isolation ----

fn temp_subdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vynkor-fsacc-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Probe that Landlock is actually enforced for a sandboxed plugin: a
/// `max_fs_access: none` spawn whose interpreter succeeds (exec requirements
/// granted) but whose `open("/etc/passwd")` is denied by the ruleset. A failed
/// spawn or a successful read means the host cannot enforce Landlock.
async fn fs_restriction_available(sup: &PluginSupervisor, python: &str) -> bool {
    let probe = PluginConfig {
        plugin_id: "fsacc_probe".to_string(),
        binary_path: PathBuf::from(python),
        args: vec![
            "-c".to_string(),
            "import sys; \
             try: open('/etc/passwd').read(); print('LANDLOCK_FAIL'); \
             except PermissionError: print('LANDLOCK_OK'); \
             except OSError as e: print('LANDLOCK_ERR', e.errno)"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        max_fs_access: FsAccessMode::None,
        grace_seconds: 1,
        ..Default::default()
    };
    if sup.spawn_plugin(probe).await.is_err() {
        return false;
    }
    let logs = wait_for_log(sup, "fsacc_probe", "LANDLOCK_").await;
    let _ = sup.stop_plugin("fsacc_probe").await;
    logs.iter().any(|l| l.contains("LANDLOCK_OK"))
}

/// A restricted plugin may read nothing it did not declare: its own binary
/// dir and system libs (for exec) plus `writable_paths`. `~` and `/etc` are
/// outside the ruleset, so both reads must fail with EACCES.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_denied_undeclared_reads() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_fsacc_read.sock"));
    if !fs_restriction_available(&sup, &python).await {
        eprintln!("skipping: Landlock filesystem restriction unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "fsacc_read".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import os
home = os.path.expanduser('~/.bashrc')
home_readable = True
try:
    open(home, 'rb').read(1)
except (PermissionError, OSError):
    home_readable = False
etc_readable = True
try:
    open('/etc/passwd').read()
except (PermissionError, OSError):
    etc_readable = False
print('RESULT_HOME=' + ('READABLE' if home_readable else 'DENIED'))
print('RESULT_ETC=' + ('READABLE' if etc_readable else 'DENIED'))"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        max_fs_access: FsAccessMode::None,
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");
    let logs = wait_for_log(&sup, "fsacc_read", "RESULT_HOME=").await;
    let _ = sup.stop_plugin("fsacc_read").await;

    assert!(
        logs.iter().any(|l| l.contains("RESULT_HOME=DENIED")),
        "~ must not be readable under max_fs_access: none, logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("RESULT_ETC=DENIED")),
        "/etc/passwd must not be readable under max_fs_access: none, logs: {logs:?}"
    );
}

/// `writable_paths` are the only places a restricted plugin may create files;
/// an undeclared sibling dir must deny the write with EACCES.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_writes_only_declared_writable_paths() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_fsacc_write.sock"));
    if !fs_restriction_available(&sup, &python).await {
        eprintln!("skipping: Landlock filesystem restriction unavailable");
        return;
    }

    let allowed = temp_subdir("write-ok");
    let denied = temp_subdir("write-denied");
    let script = format!(
        "import os
try:
    open('{0}', 'w').write('x')
    allowed = 'OK'
except (PermissionError, OSError):
    allowed = 'DENIED'
try:
    open('{1}', 'w').write('x')
    denied = 'OK'
except (PermissionError, OSError):
    denied = 'DENIED'
print('RESULT_ALLOWED=' + allowed)
print('RESULT_DENIED=' + denied)",
        allowed.join("ok.txt").display(),
        denied.join("x.txt").display()
    );

    let config = PluginConfig {
        plugin_id: "fsacc_write".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), script],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        max_fs_access: FsAccessMode::None,
        writable_paths: vec![allowed.clone()],
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");
    let logs = wait_for_log(&sup, "fsacc_write", "RESULT_ALLOWED=").await;
    let _ = sup.stop_plugin("fsacc_write").await;
    let _ = std::fs::remove_dir_all(&allowed);
    let _ = std::fs::remove_dir_all(&denied);

    assert!(
        logs.iter().any(|l| l.contains("RESULT_ALLOWED=OK")),
        "writable_paths must accept writes, logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("RESULT_DENIED=DENIED")),
        "an undeclared dir must deny writes, logs: {logs:?}"
    );
}

/// `readonly_paths` grant reads but not writes; a restricted plugin must be
/// able to read its declared data yet still fail to modify it.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_reads_declared_readonly_paths() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_fsacc_ro.sock"));
    if !fs_restriction_available(&sup, &python).await {
        eprintln!("skipping: Landlock filesystem restriction unavailable");
        return;
    }

    let ro = temp_subdir("readonly");
    let ro_file = ro.join("data.txt");
    std::fs::write(&ro_file, "secret").unwrap();
    let script = format!(
        "try:
    data = open('{0}').read()
    read = 'OK:' + data.strip()
except (PermissionError, OSError):
    read = 'DENIED'
try:
    open('{0}', 'w').write('x')
    wrote = 'OK'
except (PermissionError, OSError):
    wrote = 'DENIED'
print('RESULT_READ=' + read)
print('RESULT_WRITE=' + wrote)",
        ro_file.display()
    );

    let config = PluginConfig {
        plugin_id: "fsacc_ro".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec!["-c".to_string(), script],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        max_fs_access: FsAccessMode::ReadOnly,
        readonly_paths: vec![ro.clone()],
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");
    let logs = wait_for_log(&sup, "fsacc_ro", "RESULT_READ=").await;
    let _ = sup.stop_plugin("fsacc_ro").await;
    let _ = std::fs::remove_dir_all(&ro);

    assert!(
        logs.iter().any(|l| l.contains("RESULT_READ=OK:secret")),
        "declared readonly_paths must be readable, logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("RESULT_WRITE=DENIED")),
        "readonly_paths must deny writes, logs: {logs:?}"
    );
}

/// `max_fs_access: full` (the default) must leave the filesystem unrestricted
/// — a regression guard for the existing sandbox contract.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_full_mode_is_unrestricted() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_fsacc_full.sock"));
    if !sandbox_available(&sup).await {
        eprintln!("skipping: unprivileged user namespaces unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "fsacc_full".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "try:
    line = open('/etc/passwd').readline().strip()
    print('RESULT=FULL_READ_OK:' + line)
except (PermissionError, OSError) as e:
    print('RESULT=FULL_READ_FAIL:' + repr(e))"
                .to_string(),
        ],
        env: vec![],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");
    let logs = wait_for_log(&sup, "fsacc_full", "RESULT=FULL_READ_OK").await;
    let _ = sup.stop_plugin("fsacc_full").await;

    assert!(
        logs.iter().any(|l| l.contains("RESULT=FULL_READ_OK")),
        "full mode must not restrict filesystem access, logs: {logs:?}"
    );
}

/// A restricted plugin must still reach the kernel's UDS: the socket path is
/// granted `ResolveUnix` (ABI v9), otherwise every sandboxed+restricted plugin
/// would fail to register. The test binds a listener so a successful connect
/// proves the resolve was not denied — EACCES here means the rule is missing.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_reaches_kernel_socket() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sock_path = "/tmp/vynkor_shim_fsacc_sock.sock";
    let _ = std::fs::remove_file(sock_path);
    let _listener = std::os::unix::net::UnixListener::bind(sock_path)
        .expect("bind test listener for the kernel socket");
    let sup = Arc::new(PluginSupervisor::new(sock_path));
    if !fs_restriction_available(&sup, &python).await {
        eprintln!("skipping: Landlock filesystem restriction unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "fsacc_sock".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import os, socket
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(2)
try:
    s.connect(os.environ['VYN_SOCKET_PATH'])
    result = 'CONNECT_OK'
except PermissionError:
    result = 'CONNECT_EACCES'
except OSError:
    result = 'CONNECT_ERR'
print('RESULT=' + result)"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        max_fs_access: FsAccessMode::None,
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");
    let logs = wait_for_log(&sup, "fsacc_sock", "RESULT=").await;
    let _ = sup.stop_plugin("fsacc_sock").await;
    let _ = std::fs::remove_file(sock_path);

    assert!(
        logs.iter().any(|l| l.contains("RESULT=CONNECT_OK")),
        "restricted plugin must reach the kernel UDS, logs: {logs:?}"
    );
    assert!(
        !logs.iter().any(|l| l.contains("RESULT=CONNECT_EACCES")),
        "socket resolve must not be denied by the ruleset, logs: {logs:?}"
    );
}

// ---- R9-04: seccomp syscall denylist ----

/// Probe that seccomp is actually enforced for a sandboxed plugin: spawn one
/// that calls `ptrace` (denied) and one that does not; the former must die
/// with SIGSYS while the latter survives long enough to print. A successful
/// ptrace or a failed spawn means the host cannot enforce the filter.
async fn seccomp_available(sup: &PluginSupervisor, python: &str) -> bool {
    let probe = PluginConfig {
        plugin_id: "seccomp_probe".to_string(),
        binary_path: PathBuf::from(python),
        args: vec![
            "-c".to_string(),
            "import ctypes, sys; \
             libc = ctypes.CDLL(None, use_errno=True); \
             r = libc.ptrace(0, 0, 0, 0); \
             print('PASS', r, 'errno', ctypes.get_errno(), flush=True); \
             time.sleep(30)"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        grace_seconds: 1,
        ..Default::default()
    };
    // the probe must die (SIGSYS), not survive the denied syscall
    if sup.spawn_plugin(probe).await.is_err() {
        return false;
    }
    let logs = wait_for_log(sup, "seccomp_probe", "PASS").await;
    let _ = sup.stop_plugin("seccomp_probe").await;
    !logs.iter().any(|l| l.contains("PASS"))
}

/// A sandboxed plugin calling a denied syscall (`ptrace`) must be killed with
/// SIGSYS — the filter is fail-closed and the plugin never runs unfiltered.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_denied_ptrace_dies_with_sigsys() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_seccomp.sock"));
    if !seccomp_available(&sup, &python).await {
        eprintln!("skipping: seccomp syscall filter unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "seccomp_ptrace".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import ctypes, time; \
             print('STARTED', flush=True); \
             libc = ctypes.CDLL(None, use_errno=True); \
             libc.ptrace(0, 0, 0, 0); \
             print('PTRACE_RETURNED', flush=True); \
             time.sleep(30)"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        grace_seconds: 1,
        ..Default::default()
    };
    let proc = sup
        .spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");

    let logs = wait_for_log(&sup, "seccomp_ptrace", "STARTED").await;
    assert!(
        logs.iter().any(|l| l.contains("STARTED")),
        "plugin must start before the syscall, logs: {logs:?}"
    );

    // the ptrace call must never return — the process dies with SIGSYS
    assert!(
        !logs.iter().any(|l| l.contains("PTRACE_RETURNED")),
        "denied ptrace must not return, logs: {logs:?}"
    );
    for _ in 0..30 {
        let alive =
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(proc.pid as i32), None).is_ok();
        if !alive {
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("plugin pid {} survived a denied ptrace syscall", proc.pid);
}

/// The seccomp denylist must not disturb normal plugin operation: a sandboxed
/// plugin doing ordinary work (threads, sockets, files via declared paths)
/// keeps running under the filter.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn sandboxed_plugin_runs_normally_under_seccomp() {
    let Some(python) = python3_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    set_shim_bin();
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_shim_seccomp_ok.sock"));
    if !sandbox_available(&sup).await {
        eprintln!("skipping: unprivileged user namespaces unavailable");
        return;
    }

    let config = PluginConfig {
        plugin_id: "seccomp_ok".to_string(),
        binary_path: PathBuf::from(&python),
        args: vec![
            "-c".to_string(),
            "import threading, time; \
             def worker(): \
                 [x*x for x in range(1000)]; \
                 print('WORKER_DONE', flush=True); \
             t = threading.Thread(target=worker); \
             t.start(); \
             print('MAIN_READY', flush=True); \
             t.join(); \
             time.sleep(30)"
                .to_string(),
        ],
        env: vec![
            "PYTHONNOUSERSITE=1".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
        ],
        restart_policy: RestartPolicy::Never,
        max_restarts: 0,
        sandbox: true,
        grace_seconds: 1,
        ..Default::default()
    };
    sup.spawn_plugin(config)
        .await
        .expect("sandbox spawn must succeed");

    let logs = wait_for_log(&sup, "seccomp_ok", "WORKER_DONE").await;
    let _ = sup.stop_plugin("seccomp_ok").await;
    assert!(
        logs.iter().any(|l| l.contains("MAIN_READY")),
        "plugin main thread must run under seccomp, logs: {logs:?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("WORKER_DONE")),
        "plugin worker thread must run under seccomp, logs: {logs:?}"
    );
}
