//! Seccomp syscall filter (R9-04).
//!
//! A tight denylist of kernel-escape-capable syscalls, installed in the
//! plugin's `pre_exec` after Landlock (fail-closed: a plugin that cannot be
//! filtered is killed and never runs unfiltered). Everything else stays
//! allowed, so arbitrary third-party plugins (Python/C++/tokio runtimes,
//! whatever they link) keep working without a per-SDK allowlist that would
//! rot the moment a runtime version changes. The deny set is chosen so that a
//! compromised plugin cannot attack the kernel: no tracing (ptrace), no
//! eBPF (bpf), no kernel keyrings (keyctl/add_key/request_key), no module
//! loading (init/finit/delete_module), no reboot/kexec, no mount namespace
//! escape (mount/umount2/pivot_root/chroot/setns + the new mount API), no
//! file-handle-based Landlock bypass (open_by_handle_at/name_to_handle_at),
//! no cross-process memory (process_vm_readv/writev), no perf/userfaultfd,
//! no io_uring (which would let the plugin submit kernel work outside the
//! syscall filter), no swap/acct/syslog/hostname fiddling.
//!
//! Filter action for a denied syscall is `SIGSYS` (KillProcess): the plugin
//! dies instead of getting a retryable EPERM, and the supervisor restarts it
//! per its policy — the same fail-closed posture as the rest of the sandbox.

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};
use std::convert::TryInto;
use std::env::consts::ARCH;

/// Kernel-escape-capable syscalls every sandboxed plugin is denied.
///
/// The list is deliberately conservative — a syscall the plugin legitimately
/// needs is never added here; when in doubt, leave it allowed and rely on the
/// namespaces + Landlock. New mount-API entries and any future escape vector
/// belong at the top so they cannot be missed.
fn denied() -> Vec<(i64, Vec<SeccompRule>)> {
    use libc::{
        SYS_acct, SYS_add_key, SYS_bpf, SYS_chroot, SYS_delete_module, SYS_fanotify_init,
        SYS_finit_module, SYS_fsconfig, SYS_fsopen, SYS_fspick, SYS_init_module,
        SYS_io_uring_enter, SYS_io_uring_register, SYS_io_uring_setup, SYS_kcmp,
        SYS_kexec_file_load, SYS_kexec_load, SYS_keyctl, SYS_lookup_dcookie, SYS_modify_ldt,
        SYS_mount, SYS_mount_setattr, SYS_move_mount, SYS_name_to_handle_at, SYS_open_by_handle_at,
        SYS_open_tree, SYS_perf_event_open, SYS_pivot_root, SYS_process_vm_readv,
        SYS_process_vm_writev, SYS_ptrace, SYS_quotactl, SYS_reboot, SYS_request_key,
        SYS_setdomainname, SYS_sethostname, SYS_setns, SYS_swapoff, SYS_swapon, SYS_syslog,
        SYS_umount2, SYS_userfaultfd, SYS_vhangup,
    };
    vec![
        (SYS_ptrace, vec![]),
        (SYS_bpf, vec![]),
        (SYS_keyctl, vec![]),
        (SYS_add_key, vec![]),
        (SYS_request_key, vec![]),
        (SYS_reboot, vec![]),
        (SYS_kexec_load, vec![]),
        (SYS_kexec_file_load, vec![]),
        (SYS_init_module, vec![]),
        (SYS_finit_module, vec![]),
        (SYS_delete_module, vec![]),
        (SYS_mount, vec![]),
        (SYS_umount2, vec![]),
        (SYS_pivot_root, vec![]),
        (SYS_chroot, vec![]),
        (SYS_setns, vec![]),
        (SYS_open_tree, vec![]),
        (SYS_move_mount, vec![]),
        (SYS_fsopen, vec![]),
        (SYS_fsconfig, vec![]),
        (SYS_fspick, vec![]),
        (SYS_mount_setattr, vec![]),
        (SYS_open_by_handle_at, vec![]),
        (SYS_name_to_handle_at, vec![]),
        (SYS_process_vm_readv, vec![]),
        (SYS_process_vm_writev, vec![]),
        (SYS_perf_event_open, vec![]),
        (SYS_userfaultfd, vec![]),
        (SYS_io_uring_setup, vec![]),
        (SYS_io_uring_enter, vec![]),
        (SYS_io_uring_register, vec![]),
        (SYS_kcmp, vec![]),
        (SYS_swapon, vec![]),
        (SYS_swapoff, vec![]),
        (SYS_acct, vec![]),
        (SYS_syslog, vec![]),
        (SYS_sethostname, vec![]),
        (SYS_setdomainname, vec![]),
        (SYS_modify_ldt, vec![]),
        (SYS_quotactl, vec![]),
        (SYS_lookup_dcookie, vec![]),
        (SYS_vhangup, vec![]),
        (SYS_fanotify_init, vec![]),
    ]
}

/// Install the denylist on the calling thread. Runs in the plugin's
/// `pre_exec` — single-threaded, before exec — so the filter is inherited by
/// the plugin and all its threads, and cannot be removed afterwards. On any
/// failure the spawn errors (the shim sees no readiness byte and kills the
/// child): a plugin that cannot be filtered never runs.
#[cfg(target_os = "linux")]
pub fn apply() -> std::io::Result<()> {
    let arch: TargetArch = ARCH.try_into().map_err(|e: seccompiler::BackendError| {
        io_err(format!("seccomp: unsupported architecture {ARCH}: {e}"))
    })?;
    let filter = SeccompFilter::new(
        denied().into_iter().collect(),
        SeccompAction::Allow,       // not in the deny list — allowed
        SeccompAction::KillProcess, // in the deny list — SIGSYS
        arch,
    )
    .map_err(|e| io_err(format!("seccomp: invalid filter: {e}")))?;
    let bpf: BpfProgram = filter
        .try_into()
        .map_err(|e| io_err(format!("seccomp: compile failed: {e}")))?;
    seccompiler::apply_filter(&bpf).map_err(|e| io_err(format!("seccomp: apply failed: {e}")))?;
    tracing::info!(denied = denied().len(), "seccomp syscall denylist enforced");
    Ok(())
}

fn io_err(msg: String) -> std::io::Error {
    std::io::Error::other(msg)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn deny_list_covers_known_escape_vectors() {
        let denied = denied();
        let nrs: Vec<i64> = denied.iter().map(|(nr, _)| *nr).collect();
        for (name, nr) in [
            ("ptrace", libc::SYS_ptrace),
            ("bpf", libc::SYS_bpf),
            ("keyctl", libc::SYS_keyctl),
            ("mount", libc::SYS_mount),
            ("umount2", libc::SYS_umount2),
            ("pivot_root", libc::SYS_pivot_root),
            ("chroot", libc::SYS_chroot),
            ("setns", libc::SYS_setns),
            ("open_by_handle_at", libc::SYS_open_by_handle_at),
            ("process_vm_readv", libc::SYS_process_vm_readv),
            ("process_vm_writev", libc::SYS_process_vm_writev),
            ("io_uring_setup", libc::SYS_io_uring_setup),
            ("reboot", libc::SYS_reboot),
            ("kexec_load", libc::SYS_kexec_load),
            ("init_module", libc::SYS_init_module),
        ] {
            assert!(nrs.contains(&nr), "{name} must be denied");
        }
    }

    #[test]
    fn deny_list_has_no_duplicates() {
        let nrs: Vec<i64> = denied().iter().map(|(nr, _)| *nr).collect();
        let mut sorted = nrs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), nrs.len(), "duplicate syscall in deny list");
    }

    #[test]
    fn deny_list_compiles_to_bpf() {
        let arch: TargetArch = ARCH.try_into().unwrap();
        let filter = SeccompFilter::new(
            denied().into_iter().collect(),
            SeccompAction::Allow,
            SeccompAction::KillProcess,
            arch,
        )
        .unwrap();
        let bpf: BpfProgram = filter.try_into().unwrap();
        assert!(!bpf.is_empty());
    }

    #[test]
    fn critical_runtime_syscalls_stay_allowed() {
        // the deny set must never swallow the syscalls every plugin needs to
        // boot and speak to the kernel — a regression guard against someone
        // "hardening" the list into an allowlist by accident
        let denied_nrs: Vec<i64> = denied().iter().map(|(nr, _)| *nr).collect();
        for (name, nr) in [
            ("read", libc::SYS_read),
            ("write", libc::SYS_write),
            ("mmap", libc::SYS_mmap),
            ("mprotect", libc::SYS_mprotect),
            ("openat", libc::SYS_openat),
            ("socket", libc::SYS_socket),
            ("connect", libc::SYS_connect),
            ("sendmsg", libc::SYS_sendmsg),
            ("recvmsg", libc::SYS_recvmsg),
            ("epoll_wait", libc::SYS_epoll_wait),
            ("futex", libc::SYS_futex),
            ("clone", libc::SYS_clone),
            ("clone3", libc::SYS_clone3),
            ("execve", libc::SYS_execve),
            ("getrandom", libc::SYS_getrandom),
            ("arch_prctl", libc::SYS_arch_prctl),
        ] {
            assert!(!denied_nrs.contains(&nr), "{name} must remain allowed");
        }
    }
}
