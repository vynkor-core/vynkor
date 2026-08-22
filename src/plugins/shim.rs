//! Sandbox shim (R9-02).
//!
//! The supervisor cannot place a plugin into a PID namespace from its own
//! spawn path: `unshare(CLONE_NEWPID)` only applies to *future* children, so
//! the exec'd plugin would inherit a pending `pid_for_children` namespace and
//! the kernel then refuses thread creation (EINVAL) — every multithreaded
//! plugin dies at startup. Instead the supervisor re-execs its own binary
//! with the hidden `__shim` subcommand: this process does the unshare, forks
//! the plugin into the fresh namespace (born as PID 1, where threads work),
//! and stays alive to forward signals and reap the exit status.
//!
//! The shim is single-threaded by construction: `unshare(CLONE_NEWUSER)`
//! fails with EINVAL in a multithreaded process, so main dispatches it before
//! the tokio runtime exists. The supervisor signals the shim, never the
//! plugin, so the shim's `waitpid` always runs.
//!
//! The plugin is PID 1 of its namespace, where the kernel silently drops
//! signals for which no handler is installed (`SIGNAL_UNKILLABLE`) — even a
//! SIGTERM sent from the parent namespace. A handler-less plugin would
//! therefore block the shim's `waitpid` (and the supervisor's `child.wait()`)
//! forever. The shim starts an escalation timer when it forwards a terminal
//! signal and SIGKILLs the plugin once `VYN_SHIM_GRACE_SECS` (default 5)
//! elapse, mirroring the supervisor's own SIGTERM→SIGKILL grace.

use anyhow::{anyhow, bail, Context};
use nix::errno::Errno;
use nix::libc::{self, c_int};
use nix::mount::{mount, MsFlags};
use nix::poll::{poll, PollFd, PollFlags};
use nix::sched::{unshare, CloneFlags};
use nix::sys::prctl::set_pdeathsig;
use nix::sys::signal::{
    kill, sigaction, sigprocmask, SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal,
};
use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{getgid, getuid, read, write, Pid};
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

/// Host pid of the plugin, set once it is forked. The signal handlers forward
/// TERM/INT/HUP to it; before it is set they restore the default disposition
/// and re-raise, so an early signal still kills the shim instead of hanging.
static CHILD_PID: AtomicI32 = AtomicI32::new(0);
/// Set once a TERM/INT/HUP has been forwarded to the plugin. It may ignore
/// the signal (it is PID 1 of its namespace), so the reap loop starts the
/// escalation timer on first observation. Async-signal-safe: lock-free store.
static TERMINAL_FORWARDED: AtomicBool = AtomicBool::new(false);
/// True once the grace period elapsed and SIGKILL was sent — the reap loop
/// must not re-kill a corpse on every poll.
static SIGKILL_SENT: AtomicBool = AtomicBool::new(false);

extern "C" fn forward_signal(sig: c_int) {
    let child = CHILD_PID.load(Ordering::SeqCst);
    if child > 0 {
        unsafe {
            libc::kill(child, sig);
        }
        TERMINAL_FORWARDED.store(true, Ordering::SeqCst);
    } else {
        unsafe {
            libc::signal(sig, libc::SIG_DFL);
            libc::raise(sig);
        }
    }
}

/// Run a plugin inside a private user + PID + mount namespace.
///
/// Returns the plugin's exit status. The plugin's host pid is printed to
/// stdout — the supervisor reads it as the spawn handshake, so this function
/// returning normally implies the plugin entered the sandbox.
pub fn run(plugin_binary: &Path, args: &[String]) -> anyhow::Result<i32> {
    // readiness handshake: the plugin writes one byte from its pre_exec hook;
    // the shim waits for it before reporting a pid, so a plugin that could
    // not enter the sandbox is killed and never runs unisolated
    let (ready_rx, ready_tx) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None,
        SockFlag::SOCK_CLOEXEC,
    )
    .context("socketpair")?;
    let ready_fd = ready_tx.as_raw_fd();

    // TERM/INT/HUP must not kill the shim (that would orphan the plugin); they
    // are blocked during setup and forwarded once the plugin exists
    let mut forwarded = SigSet::empty();
    forwarded.add(Signal::SIGTERM);
    forwarded.add(Signal::SIGINT);
    forwarded.add(Signal::SIGHUP);
    let action = SigAction::new(
        SigHandler::Handler(forward_signal),
        SaFlags::SA_RESTART,
        SigSet::empty(),
    );
    unsafe {
        sigaction(Signal::SIGTERM, &action)?;
        sigaction(Signal::SIGINT, &action)?;
        sigaction(Signal::SIGHUP, &action)?;
        sigprocmask(SigmaskHow::SIG_BLOCK, Some(&forwarded), None)?;
    }

    // die with the supervisor so a kernel crash cannot orphan the plugin
    set_pdeathsig(Some(Signal::SIGKILL)).context("set_pdeathsig")?;

    // R9-03: Landlock filesystem restriction the supervisor requested (None for
    // `max_fs_access: full`). Read here, applied in the plugin's pre_exec so
    // only the plugin is restricted — the shim keeps unrestricted access.
    let fs_restriction = crate::plugins::fsaccess::from_env();
    // legacy fallback pairs with the supervisor's transition alias (stage 4/A):
    // shims spawned by a pre-cutover kernel still announce VEYRON_SOCKET_PATH
    let socket_path = std::env::var("VYN_SOCKET_PATH")
        .or_else(|_| std::env::var("VEYRON_SOCKET_PATH"))
        .ok()
        .map(PathBuf::from);

    // outer uid/gid must be captured before unshare — afterwards the process
    // is already root inside the new user namespace
    let (uid, gid) = (getuid().as_raw(), getgid().as_raw());

    // one unshare: a fresh PID namespace is only creatable unprivileged when
    // it comes with a fresh user namespace, and the mount namespace goes along
    // so the plugin can mount a private /proc without host CAP_SYS_ADMIN
    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWPID | CloneFlags::CLONE_NEWNS)
        .context("unshare(CLONE_NEWUSER|CLONE_NEWPID|CLONE_NEWNS)")?;

    // single-entry map: inner root -> our outer uid
    std::fs::write("/proc/self/setgroups", "deny").context("write setgroups")?;
    std::fs::write("/proc/self/uid_map", format!("0 {uid} 1\n")).context("write uid_map")?;
    std::fs::write("/proc/self/gid_map", format!("0 {gid} 1\n")).context("write gid_map")?;

    // a fresh /proc mounted later would propagate onto the host's /proc
    // (shared mount propagation) — make the whole mount namespace private first
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("make / rprivate")?;

    // owned copy so the pre_exec closure (required 'static) can reference the
    // plugin binary while Command::new borrows the caller's path
    let plugin_binary_owned = plugin_binary.to_path_buf();

    let child = unsafe {
        Command::new(plugin_binary)
            .args(args)
            .pre_exec(move || {
                // the plugin is PID 1 in the new namespace from birth — threads
                // work here, unlike a pending pid_for_children namespace
                set_pdeathsig(Some(Signal::SIGKILL)).map_err(errno_to_io)?;
                // clear the inherited blocked mask so forwarded signals land
                let empty = SigSet::empty();
                sigprocmask(SigmaskHow::SIG_SETMASK, Some(&empty), None).map_err(errno_to_io)?;
                // fresh /proc bound to the new PID namespace
                mount(
                    Some("proc"),
                    "/proc",
                    Some("proc"),
                    MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
                    None::<&str>,
                )
                .map_err(errno_to_io)?;
                // plugin stdout joins its stderr (the supervisor's log pipe) —
                // the shim's own stdout is the pid channel
                if libc::dup2(2, 1) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // R9-03: enforce the Landlock filesystem restriction before the
                // plugin starts. On failure this errors the spawn and the
                // supervisor sees no pid line — the plugin never runs
                // unrestricted (the readiness byte is written only below).
                if let Some(restriction) = &fs_restriction {
                    crate::plugins::fsaccess::apply(
                        restriction,
                        &plugin_binary_owned,
                        socket_path.as_deref(),
                    )?;
                }
                // R9-04: seccomp syscall denylist (ptrace, bpf, mount, ...).
                // Fail-closed like Landlock: a plugin that cannot be filtered
                // never runs — the readiness byte is written only below.
                crate::plugins::seccomp::apply()?;
                // the readiness byte; the supervisor treats a missing pid line as
                // a failed spawn, so this is the sandbox admission gate
                match write(std::os::fd::BorrowedFd::borrow_raw(ready_fd), b"R") {
                    Ok(1) => Ok(()),
                    _ => Err(std::io::Error::last_os_error()),
                }
            })
            .spawn()
    }
    .context("spawn plugin")?;

    // only the plugin's copy of the readiness socket may stay open now — if it
    // dies before writing 'R' the shim sees EOF instead of hanging for 10s
    drop(ready_tx);
    let child_pid = child.id() as i32;
    CHILD_PID.store(child_pid, Ordering::SeqCst);
    sigprocmask(SigmaskHow::SIG_UNBLOCK, Some(&forwarded), None)?;

    // wait for the readiness byte (fail-closed: EOF, HUP or timeout => kill)
    let mut fds = [PollFd::new(ready_rx.as_fd(), PollFlags::POLLIN)];
    match poll(&mut fds, 10_000u16) {
        Ok(n)
            if n > 0
                && fds[0]
                    .revents()
                    .is_some_and(|f| f.contains(PollFlags::POLLIN)) =>
        {
            let mut buf = [0u8; 1];
            let n = read(ready_rx.as_fd(), &mut buf).context("read readiness")?;
            if n != 1 || buf[0] != b'R' {
                let _ = kill(Pid::from_raw(child_pid), Signal::SIGKILL);
                bail!("plugin did not signal readiness");
            }
        }
        _ => {
            let _ = kill(Pid::from_raw(child_pid), Signal::SIGKILL);
            bail!("plugin did not signal readiness within 10s");
        }
    }

    // host pid of the plugin — the supervisor's spawn handshake
    println!("{child_pid}");

    // reap the plugin and mirror its exit status (or 128+sig). The plugin is
    // PID 1 of its namespace and silently drops unhandled signals, so a
    // forwarded TERM/INT/HUP must escalate to SIGKILL after the grace period
    // or this loop (and the supervisor's child.wait()) blocks forever. The
    // signal handler cannot start the timer itself (async-signal-safety), so
    // the loop polls with WNOHANG and arms it on first observation.
    let grace_secs = std::env::var("VYN_SHIM_GRACE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);
    let mut kill_deadline: Option<Instant> = None;
    loop {
        match waitpid(Pid::from_raw(child_pid), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => return Ok(code),
            Ok(WaitStatus::Signaled(_, sig, _)) => return Ok(128 + sig as i32),
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(e) => return Err(anyhow!(e)),
        }
        if TERMINAL_FORWARDED.load(Ordering::SeqCst) && kill_deadline.is_none() {
            kill_deadline = Some(Instant::now() + Duration::from_secs(grace_secs));
        }
        if kill_deadline.is_some_and(|d| Instant::now() >= d)
            && !SIGKILL_SENT.swap(true, Ordering::SeqCst)
        {
            let _ = kill(Pid::from_raw(child_pid), Signal::SIGKILL);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn errno_to_io(e: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}
