use std::io;
use std::path::{Path, PathBuf};
use tracing::{info, trace, warn};

/// Default cap on child process count when a plugin doesn't configure
/// `max_procs`.
///
/// The cap is enforced one of two ways. With a writable cgroup v2 subtree
/// (R9-01) the budget is `pids.max` on the plugin's own cgroup — it counts
/// only tasks inside that cgroup, so the plugin is isolated from other
/// processes of the same uid. Without one the fallback is RLIMIT_NPROC,
/// which is checked at every fork/clone against the number of threads the
/// calling process's **real uid** owns system-wide: a cap below that
/// baseline (a desktop session routinely runs hundreds of threads) makes
/// every thread the plugin spawns fail with EAGAIN and the plugin dies
/// instantly. 1024 still bounds a runaway plugin while surviving ordinary
/// session baselines; operators on heavily loaded machines can raise it
/// per-plugin via `max_procs`.
pub const DEFAULT_MAX_PROCS: u64 = 1024;
/// Default cap on virtual memory in MiB (RLIMIT_AS) when a plugin doesn't
/// configure `max_vmem_mb`. 0 = unlimited (RLIM_INFINITY). Raised from 512
/// (2026-08-26) because ONNX Runtime reserves ~500 MiB of virtual address
/// space at init — the old default starved tts/speech models on mmap.
pub const DEFAULT_MAX_VMEM_MB: u64 = 2048;

/// Component name (under the delegated cgroup root) holding all per-plugin
/// pids scopes.
const VYN_CGROUP_DIR: &str = "vyn";

/// Run inside new user + network namespaces and apply resource limits.
/// Passed as a `pre_exec` hook; executes in the child process before exec.
///
/// PID-namespace isolation is not done here (and cannot be): the original
/// implementation unshared `CLONE_NEWPID` in this hook, but the exec'd plugin
/// then inherited a pending `pid_for_children` namespace and the kernel
/// refused thread creation (`CLONE_NEWPID` cannot be combined with
/// `CLONE_THREAD` — threads must share a PID namespace), so every
/// multithreaded plugin (tokio runtimes included) died instantly with EINVAL
/// on its first worker-thread spawn — as root and non-root alike. Correct
/// PID-namespace isolation needs a shim process, which has landed as R9-02:
/// sandboxed plugins are spawned through `vyn __shim` (see `plugins::shim`),
/// which unshares a fresh namespace and forks the plugin into it as PID 1.
/// This hook stays the host-side sandbox: user + network namespaces, rlimits,
/// and the per-plugin pids-cgroup join below.
///
/// User namespace: for an unprivileged operator (no CAP_SYS_ADMIN in the
/// current user namespace) the direct `CLONE_NEWNET` unshare fails with
/// EPERM, so first unshare into a **new user namespace** (permitted when
/// `kernel.unprivileged_userns_clone` is enabled) and map the real uid/gid
/// to root inside it — the process then holds CAP_SYS_ADMIN *within* that
/// namespace, which is all `CLONE_NEWNET` requires. Root callers skip the
/// dance entirely.
/// `pids_cgroup` is the plugin's prepared cgroup v2 scope (see
/// `prepare_pids_cgroup`). The join must happen after the user-namespace
/// switch so the in-namespace root maps to the real uid that owns the
/// delegated subtree.
#[cfg(target_os = "linux")]
pub fn sandbox_pre_exec(
    max_procs: u64,
    max_vmem_mb: u64,
    pids_cgroup: Option<&Path>,
) -> io::Result<()> {
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
    apply_resource_limits(max_procs, max_vmem_mb, pids_cgroup)
}

/// Apply per-process resource limits (RLIMIT_NPROC, RLIMIT_AS). Applied to
/// every spawned plugin regardless of `sandbox` (AUDIT M-03) — sandboxing is
/// namespace isolation, a separate concern from resource caps.
///
/// When `pids_cgroup` is Some, the process-count budget is enforced by the
/// cgroup's `pids.max` (R9-01) instead of RLIMIT_NPROC: the cgroup counts
/// only the plugin's own tasks, so a thread storm in one plugin — or in the
/// desktop session — no longer starves another plugin of its `max_procs`
/// budget. RLIMIT_NPROC remains as a fallback if the join fails, so the cap
/// is never silently dropped.
#[cfg(target_os = "linux")]
pub fn apply_resource_limits(
    max_procs: u64,
    max_vmem_mb: u64,
    pids_cgroup: Option<&Path>,
) -> io::Result<()> {
    use nix::sys::resource::{setrlimit, Resource};

    let joined_cgroup = pids_cgroup
        .map(|path| join_pids_cgroup(path).is_ok())
        .unwrap_or(false);
    if !joined_cgroup {
        warn!(
            max_procs,
            "no per-plugin cgroup pids scope — RLIMIT_NPROC not applied (would count uid-wide \
             threads and break thread-heavy plugins); process accounting degraded"
        );
    }
    if max_vmem_mb == 0 {
        // 0 = unlimited — don't impose a new limit, just keep the inherited one.
        // Raising to RLIM_INFINITY would require CAP_SYS_RESOURCE when the hard
        // limit is already lowered (e.g. a previous test set 2048), so we skip.
    } else {
        let max_vmem_bytes = max_vmem_mb * 1024 * 1024;
        setrlimit(Resource::RLIMIT_AS, max_vmem_bytes, max_vmem_bytes)
            .map_err(|e| io::Error::other(e.to_string()))?;
    }
    Ok(())
}

/// Prepare a per-plugin cgroup v2 `pids` scope: `vyn/<plugin_id>` under
/// the delegated subtree, with `pids.max = max_procs`. Returns the scope dir
/// on success; None (with a warning) when cgroup v2 is unavailable or no
/// writable subtree exposes the `pids` controller — the caller falls back to
/// RLIMIT_NPROC.
///
/// The delegated root is found by probing ancestors from the cgroup mount
/// root downward: the first cgroup whose `cgroup.subtree_control` enables
/// `pids` *and* that is writable by us wins. For root that is the cgroup
/// root itself; for an unprivileged operator it is the systemd-delegated
/// `user@1000.service` subtree, whose controller files are owned by the user.
///
/// A child cgroup only gets a controller that its *parent* enabled in
/// `cgroup.subtree_control`, so the intermediate `vyn` cgroup must enable
/// `pids` itself — otherwise the leaf scope's `pids.max` is a dead file and
/// every write fails with EPERM.
#[cfg(target_os = "linux")]
pub fn prepare_pids_cgroup(plugin_id: &str, max_procs: u64) -> Option<PathBuf> {
    let mount = PathBuf::from("/sys/fs/cgroup");
    if !mount.join("cgroup.controllers").is_file() {
        warn!(plugin_id = %plugin_id, "cgroup v2 not available — falling back to RLIMIT_NPROC");
        return None;
    }
    let self_cgroup = cgroup_relative_path()?;
    let scope_name = sanitize_plugin_id(plugin_id);
    for ancestor in ancestor_paths(&self_cgroup) {
        // ancestor is absolute (starts at the cgroup root); join() replaces
        // the base path instead of appending, so strip the leading slash.
        let dir = mount.join(ancestor.strip_prefix("/").unwrap_or(&ancestor));
        if !has_pids_controller(&dir) {
            continue;
        }
        let container = dir.join(VYN_CGROUP_DIR);
        if let Err(e) = std::fs::create_dir_all(&container) {
            // root-owned subtree below the delegated root (or the mount root
            // itself for non-root) — probe the next ancestor
            trace!(
                plugin_id = %plugin_id,
                cgroup = %dir.display(),
                error = %e,
                "cgroup not writable, probing next ancestor"
            );
            continue;
        }
        if let Err(e) = enable_pids_controller(&container) {
            trace!(
                plugin_id = %plugin_id,
                cgroup = %container.display(),
                error = %e,
                "cannot enable pids on vyn container, probing next ancestor"
            );
            continue;
        }
        let scope = container.join(&scope_name);
        if let Err(e) = std::fs::create_dir_all(&scope) {
            trace!(
                plugin_id = %plugin_id,
                cgroup = %scope.display(),
                error = %e,
                "cannot create plugin scope, probing next ancestor"
            );
            continue;
        }
        match std::fs::write(scope.join("pids.max"), format!("{max_procs}\n")) {
            Ok(()) => {
                info!(
                    plugin_id = %plugin_id,
                    cgroup = %scope.display(),
                    pids_max = max_procs,
                    "per-plugin process accounting via cgroup v2 pids.max"
                );
                return Some(scope);
            }
            Err(e) => {
                // scope may pre-exist from a different (root-owned) level —
                // that level is unusable, probe the next ancestor
                warn!(
                    plugin_id = %plugin_id,
                    cgroup = %scope.display(),
                    error = %e,
                    "cannot write pids.max — probing next ancestor"
                );
                let _ = std::fs::remove_dir(&scope);
                continue;
            }
        }
    }
    warn!(
        plugin_id = %plugin_id,
        "no writable cgroup v2 subtree with the pids controller — falling back to RLIMIT_NPROC"
    );
    None
}

/// Enable the `pids` controller on `cgroup_dir`'s `cgroup.subtree_control`
/// so that child cgroups get a live `pids.max`. Idempotent.
#[cfg(target_os = "linux")]
fn enable_pids_controller(cgroup_dir: &Path) -> io::Result<()> {
    let ctl = std::fs::read_to_string(cgroup_dir.join("cgroup.subtree_control"))?;
    if ctl.split_whitespace().any(|c| c == "pids") {
        return Ok(());
    }
    std::fs::write(cgroup_dir.join("cgroup.subtree_control"), "+pids\n")
}

/// Best-effort removal of a plugin's empty pids scope after its process
/// exits. No-op when the scope is gone already or still populated (a zombie
/// not yet reaped by the supervisor keeps `cgroup.procs` non-empty).
#[cfg(target_os = "linux")]
pub fn cleanup_pids_cgroup(path: &Path) {
    match std::fs::remove_dir(path) {
        Ok(()) => info!(cgroup = %path.display(), "removed empty plugin pids cgroup"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            trace!(
                cgroup = %path.display(),
                "pids cgroup still populated at cleanup (tasks not yet reaped)"
            );
        }
        Err(e) => {
            warn!(cgroup = %path.display(), error = %e, "failed to remove plugin pids cgroup")
        }
    }
}

/// Move the current process (the `pre_exec` child) into the prepared pids
/// scope. cgroup v2 permits a task to migrate itself downward into a child
/// cgroup; moving back up is not allowed, which is fine — the plugin lives in
/// its scope until exit.
#[cfg(target_os = "linux")]
fn join_pids_cgroup(path: &Path) -> io::Result<()> {
    std::fs::write(
        path.join("cgroup.procs"),
        format!("{}\n", std::process::id()),
    )
}

/// This process's cgroup v2 path from `/proc/self/cgroup` (e.g.
/// `/user.slice/user-1000.slice/user@1000.service/session.slice/x.scope`),
/// or None on cgroup v1 / absence.
#[cfg(target_os = "linux")]
fn cgroup_relative_path() -> Option<String> {
    let contents = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    contents.lines().find_map(parse_cgroup_v2_line)
}

/// Extract the v2 path from a single `/proc/<pid>/cgroup` line. The unified
/// hierarchy is line `0::/path`; cgroup v1 lines (`id:name:/path`) and
/// `0::` with no path yield None.
#[cfg(target_os = "linux")]
fn parse_cgroup_v2_line(line: &str) -> Option<String> {
    let path = line.strip_prefix("0::")?.trim();
    if path.is_empty() {
        return None;
    }
    Some(path.to_string())
}

/// Every cgroup path from the cgroup root down to `self_path`, root first.
/// `self_path` is the absolute (leading-slash) path from `/proc/self/cgroup`.
#[cfg(target_os = "linux")]
fn ancestor_paths(self_path: &str) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/")];
    let mut acc = PathBuf::from("/");
    for comp in Path::new(self_path).components() {
        if let std::path::Component::Normal(part) = comp {
            acc.push(part);
            out.push(acc.clone());
        }
    }
    out
}

/// Whether the `pids` controller is enabled for children of `cgroup_dir`
/// (`pids` appears in its `cgroup.subtree_control`).
#[cfg(target_os = "linux")]
fn has_pids_controller(cgroup_dir: &Path) -> bool {
    let Ok(ctl) = std::fs::read_to_string(cgroup_dir.join("cgroup.subtree_control")) else {
        return false;
    };
    ctl.split_whitespace().any(|c| c == "pids")
}

/// Cgroup component names allow only `[a-zA-Z0-9._-]`; map everything else to
/// `_` and cap length so a hostile plugin id cannot smuggle path separators
/// or dot-dot traversal into the cgroup path.
#[cfg(target_os = "linux")]
fn sanitize_plugin_id(plugin_id: &str) -> String {
    let mut out = String::with_capacity(plugin_id.len().min(63));
    for c in plugin_id.chars().take(63) {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    match out.as_str() {
        "" | "." | ".." => out = "plugin".to_string(),
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_allows_documented_ids() {
        assert_eq!(sanitize_plugin_id("my-plugin"), "my-plugin");
        assert_eq!(sanitize_plugin_id("stt.v2_1"), "stt.v2_1");
    }

    #[test]
    fn sanitize_neutralizes_path_traversal() {
        // dots are legal cgroup component chars, so the slash is the only
        // separator that matters — the result is one flat component
        assert_eq!(sanitize_plugin_id("../etc"), ".._etc");
        // a bare dot-dot/dot would collide with the cgroup root/parent
        // pseudo-entries, so those collapse to a neutral name
        assert_eq!(sanitize_plugin_id(".."), "plugin");
        assert_eq!(sanitize_plugin_id("."), "plugin");
        assert_eq!(sanitize_plugin_id(""), "plugin");
        assert_eq!(sanitize_plugin_id("a/b\\c"), "a_b_c");
    }

    #[test]
    fn sanitize_truncates_long_ids() {
        let long = "x".repeat(200);
        assert_eq!(sanitize_plugin_id(&long).len(), 63);
    }

    #[test]
    fn parses_cgroup_v2_line() {
        assert_eq!(
            parse_cgroup_v2_line("0::/user.slice/user-1000.slice/user@1000.service/x.scope"),
            Some("/user.slice/user-1000.slice/user@1000.service/x.scope".to_string())
        );
        // cgroup v1 lines and the empty unified root are rejected
        assert_eq!(parse_cgroup_v2_line("2:cpu:/docker/abc"), None);
        assert_eq!(parse_cgroup_v2_line("0::"), None);
    }

    #[test]
    fn ancestor_paths_goes_root_first() {
        let paths: Vec<PathBuf> =
            ancestor_paths("/user.slice/user-1000.slice/user@1000.service/session.slice/x.scope");
        let strings: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            strings,
            vec![
                "/",
                "/user.slice",
                "/user.slice/user-1000.slice",
                "/user.slice/user-1000.slice/user@1000.service",
                "/user.slice/user-1000.slice/user@1000.service/session.slice",
                "/user.slice/user-1000.slice/user@1000.service/session.slice/x.scope",
            ]
        );
    }

    #[test]
    fn ancestor_paths_single_component() {
        assert_eq!(ancestor_paths("/"), vec![PathBuf::from("/")]);
        assert_eq!(
            ancestor_paths("/vyn"),
            vec![PathBuf::from("/"), PathBuf::from("/vyn")]
        );
    }

    #[test]
    fn has_pids_controller_reads_subtree_control() {
        let dir = std::env::temp_dir().join(format!("vynkor-cg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup.subtree_control"), "cpu memory pids\n").unwrap();
        assert!(has_pids_controller(&dir));
        std::fs::write(dir.join("cgroup.subtree_control"), "cpu memory\n").unwrap();
        assert!(!has_pids_controller(&dir));
        // missing file → false
        assert!(!has_pids_controller(&dir.join("nope")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enable_pids_controller_writes_plus_pids_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("vynkor-cg-enable-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup.subtree_control"), "cpu memory\n").unwrap();
        enable_pids_controller(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("cgroup.subtree_control")).unwrap(),
            "+pids\n"
        );
        // second call must not duplicate or error
        enable_pids_controller(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("cgroup.subtree_control")).unwrap(),
            "+pids\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_vmem_is_2048_and_zero_means_unlimited() {
        assert_eq!(DEFAULT_MAX_VMEM_MB, 2048);
        // 0 is documented as unlimited — apply_resource_limits must not cap
        // vmem to 0 bytes (would instantly OOM on any mmap).
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn apply_resource_limits_zero_vmem_sets_infinity() {
        // should succeed and set RLIMIT_AS to infinity without error
        let result = apply_resource_limits(1024, 0, None);
        assert!(
            result.is_ok(),
            "0 vmem should mean unlimited, not error: {result:?}"
        );
        // restore a sane limit for this test process so later tests aren't
        // left with infinity (harmless, but keep deterministic)
        let _ = apply_resource_limits(1024, 2048, None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn apply_resource_limits_without_cgroup_warns_but_succeeds() {
        // no cgroup scope — must warn but still succeed (no NPROC cap)
        let result = apply_resource_limits(64, 512, None);
        assert!(result.is_ok());
        // even with small max_procs (64) the fallback must NOT set NPROC
        // to 64 — desktop sessions exceed that and would EAGAIN on thread spawn
        let result2 =
            apply_resource_limits(64, 512, Some(std::path::Path::new("/nonexistent/cgroup")));
        assert!(result2.is_ok());
    }
}
