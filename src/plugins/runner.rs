use std::io;

/// Default cap on child process count (RLIMIT_NPROC) when a plugin doesn't
/// configure `max_procs`.
pub const DEFAULT_MAX_PROCS: u64 = 64;
/// Default cap on virtual memory in MiB (RLIMIT_AS) when a plugin doesn't
/// configure `max_vmem_mb`.
pub const DEFAULT_MAX_VMEM_MB: u64 = 512;

/// Run inside new PID + network namespaces and apply resource limits.
/// Passed as a `pre_exec` hook; executes in the child process before exec.
#[cfg(target_os = "linux")]
pub fn sandbox_pre_exec(max_procs: u64, max_vmem_mb: u64) -> io::Result<()> {
    use nix::sched::{unshare, CloneFlags};

    unshare(CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNET)
        .map_err(|e| io::Error::other(e.to_string()))?;
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
