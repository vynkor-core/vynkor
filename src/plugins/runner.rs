use std::io;

/// Default cap on child process count (RLIMIT_NPROC) when a plugin doesn't
/// configure `max_procs`.
///
/// RLIMIT_NPROC is checked at every fork/clone against the number of
/// threads the calling process's **real uid** owns system-wide. A cap below
/// that baseline (a desktop session routinely runs hundreds of threads)
/// makes every thread the plugin spawns fail with EAGAIN and the plugin
/// dies instantly. 1024 still bounds a runaway plugin while surviving
/// ordinary session baselines; operators on heavily loaded machines can
/// raise it per-plugin via `max_procs`.
pub const DEFAULT_MAX_PROCS: u64 = 1024;
/// Default cap on virtual memory in MiB (RLIMIT_AS) when a plugin doesn't
/// configure `max_vmem_mb`.
pub const DEFAULT_MAX_VMEM_MB: u64 = 512;

/// Run inside new user + network namespaces and apply resource limits.
/// Passed as a `pre_exec` hook; executes in the child process before exec.
///
/// The original implementation also unshared the PID namespace
/// (`CLONE_NEWPID`). That is fundamentally incompatible with this
/// supervisor's spawn path: `unshare(CLONE_NEWPID)` marks the *caller* so
/// that its children land in a fresh PID namespace, and the exec'd plugin
/// inherits that state. The kernel refuses thread creation for a process
/// with a pending `pid_for_children` namespace (`CLONE_NEWPID` cannot be
/// combined with `CLONE_THREAD` — threads must share a PID namespace), so
/// every multithreaded plugin (tokio runtimes included) died instantly
/// with EINVAL on its first worker-thread spawn — as root and non-root
/// alike. Doing it correctly requires a shim process: a supervisor-forked
/// wrapper that unshares `CLONE_NEWPID`, forks the plugin (which is *born*
/// into the namespace as PID 1, where threads work), and forwards signals
/// and exit status. That redesign is out of scope here; until it lands,
/// the sandbox isolates via user + network namespaces and rlimits instead.
///
/// User namespace: for an unprivileged operator (no CAP_SYS_ADMIN in the
/// current user namespace) the direct `CLONE_NEWNET` unshare fails with
/// EPERM, so first unshare into a **new user namespace** (permitted when
/// `kernel.unprivileged_userns_clone` is enabled) and map the real uid/gid
/// to root inside it — the process then holds CAP_SYS_ADMIN *within* that
/// namespace, which is all `CLONE_NEWNET` requires. Root callers skip the
/// dance entirely.
#[cfg(target_os = "linux")]
pub fn sandbox_pre_exec(max_procs: u64, max_vmem_mb: u64) -> io::Result<()> {
    use nix::sched::{unshare, CloneFlags};
    use nix::unistd::{getgid, getuid};

    // Real ids must be captured before the user-namespace switch —
    // afterwards getuid()/getgid() report the in-namespace root (0).
    let uid = getuid().as_raw();
    let gid = getgid().as_raw();

    if uid != 0 {
        unshare(CloneFlags::CLONE_NEWUSER).map_err(|e| io::Error::other(e.to_string()))?;
        // "deny" must be written before gid_map, or the map write fails
        // with EPERM when the parent namespace is not writable.
        std::fs::write("/proc/self/setgroups", "deny\n")
            .map_err(|e| io::Error::other(e.to_string()))?;
        std::fs::write("/proc/self/uid_map", format!("0 {uid} 1\n"))
            .map_err(|e| io::Error::other(e.to_string()))?;
        std::fs::write("/proc/self/gid_map", format!("0 {gid} 1\n"))
            .map_err(|e| io::Error::other(e.to_string()))?;
    }

    unshare(CloneFlags::CLONE_NEWNET).map_err(|e| io::Error::other(e.to_string()))?;
    apply_resource_limits(max_procs, max_vmem_mb)
}

/// Apply per-process resource limits (RLIMIT_NPROC, RLIMIT_AS). Applied to
/// every spawned plugin regardless of `sandbox` (AUDIT M-03) — sandboxing is
/// namespace isolation, a separate concern from resource caps.
#[cfg(target_os = "linux")]
pub fn apply_resource_limits(max_procs: u64, max_vmem_mb: u64) -> io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};

    let max_vmem_bytes = max_vmem_mb * 1024 * 1024;

    setrlimit(Resource::RLIMIT_NPROC, max_procs, max_procs)
        .map_err(|e| io::Error::other(e.to_string()))?;
    setrlimit(Resource::RLIMIT_AS, max_vmem_bytes, max_vmem_bytes)
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}
