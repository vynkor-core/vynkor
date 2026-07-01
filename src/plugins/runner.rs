use std::io;

/// Run inside new PID + network namespaces and apply resource limits.
/// Passed as a `pre_exec` hook; executes in the child process before exec.
#[cfg(target_os = "linux")]
pub fn sandbox_pre_exec() -> io::Result<()> {
    use nix::sched::{unshare, CloneFlags};

    unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNET)
        .map_err(|e| io::Error::other(e.to_string()))?;
    apply_resource_limits()
}

/// Apply per-process resource limits (RLIMIT_NPROC, RLIMIT_AS).
/// Called from `sandbox_pre_exec`; may also be called standalone.
#[cfg(target_os = "linux")]
pub fn apply_resource_limits() -> io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};

    const MAX_PROCS: u64 = 64;
    const MAX_VMEM: u64 = 512 * 1024 * 1024; // 512 MiB

    setrlimit(Resource::RLIMIT_NPROC, MAX_PROCS, MAX_PROCS)
        .map_err(|e| io::Error::other(e.to_string()))?;
    setrlimit(Resource::RLIMIT_AS, MAX_VMEM, MAX_VMEM)
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}
