use anyhow::Result;
use clap::Parser;
use std::fs;
use std::process::Command;
use tracing::{info, warn};
use veyron::cli::{complete, plugin};
use veyron::cli::{Cli, Commands};
use veyron::kernel;
use veyron::utils;
use veyron::utils::config::{load_config, resolve_plugins_dir, Config};

fn main() -> Result<()> {
    let cli = Cli::parse();

    // sandbox shim: must stay single-threaded — unshare(CLONE_NEWUSER) fails
    // with EINVAL in a multithreaded process, and the tokio runtime below
    // spawns worker threads. Dispatch it before the runtime exists. Linux
    // only: PID namespaces do not exist elsewhere.
    #[cfg(target_os = "linux")]
    if let Commands::Shim {
        plugin_binary,
        args,
    } = &cli.command
    {
        let code = match veyron::plugins::shim::run(plugin_binary, args) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("shim: {e:#}");
                1
            }
        };
        std::process::exit(code);
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_kernel(cli))
}

async fn run_kernel(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Start {
            foreground,
            port,
            config,
            debug,
        } => {
            // Defer the load-failure warning until after logging is configured,
            // so it honours the resolved log level.
            let (mut cfg, load_err) = match load_config(&config) {
                Ok(c) => (c, None),
                Err(e) => (Config::default(), Some(e.to_string())),
            };

            if let Some(p) = port {
                cfg.port = p;
            }
            if debug {
                cfg.log_level = "debug".to_string();
            }

            utils::logging::init(&cfg.log_level);
            if let Some(e) = load_err {
                warn!("failed to load config '{}': {}, using defaults", config, e);
            }

            ensure_auth_configured(&cfg)?;

            if foreground {
                // Foreground mode — including the child the daemon launcher spawns
                // with --foreground — is guarded by the exclusive PID-file flock in
                // run_foreground. Do NOT run is_running() here: the daemon parent
                // writes the child's PID before the child starts, so the child would
                // probe its own live PID, conclude "already running", and abort.
                run_foreground(cfg).await?;
            } else {
                if is_running(&cfg.pid_file)? {
                    let pid = read_pid(&cfg.pid_file)?;
                    warn!("kernel already running (PID: {})", pid);
                    return Ok(());
                }
                daemonize_and_run(&cfg, &config, debug)?;
            }
        }
        Commands::Stop { config } => {
            let cfg = load_config(&config).unwrap_or_default();
            utils::logging::init(&cfg.log_level);
            stop_kernel(&cfg.pid_file)?;
        }
        Commands::Restart { config, debug } => {
            let cfg = load_config(&config).unwrap_or_default();
            utils::logging::init(&cfg.log_level);
            ensure_auth_configured(&cfg)?;
            // Capture the PID before stop_kernel removes the pid file, so we can
            // confirm the actual process is gone rather than guessing with a sleep.
            let old_pid = read_pid(&cfg.pid_file).ok();
            stop_kernel(&cfg.pid_file)?;
            if let Some(pid) = old_pid {
                if !wait_pid_gone(pid, std::time::Duration::from_secs(5)) {
                    anyhow::bail!(
                        "restart aborted: kernel (PID {pid}) still alive after stop — \
                         refusing to start a second instance"
                    );
                }
            }
            daemonize_and_run(&cfg, &config, debug)?;
        }
        Commands::Status { config } => {
            let cfg = load_config(&config).unwrap_or_default();
            utils::logging::init(&cfg.log_level);
            if is_running(&cfg.pid_file)? {
                let pid = read_pid(&cfg.pid_file)?;
                println!("veyron is running (PID: {})", pid);
            } else {
                println!("veyron is not running");
            }
        }
        Commands::Logs { lines, config } => {
            let cfg = load_config(&config).unwrap_or_default();
            utils::logging::init(&cfg.log_level);
            show_logs(&cfg.log_file, lines)?;
        }
        Commands::Plugin { cmd, config, token } => {
            let cfg = load_config(&config).unwrap_or_default();
            let token = token.or_else(|| std::env::var("VEYRON_JWT_TOKEN").ok());
            let plugins_dir = resolve_plugins_dir(&config, cfg.plugins_dir.as_deref());
            plugin::handle(
                cmd,
                cfg.port,
                cfg.registry_url.as_deref(),
                token.as_deref(),
                cfg.tls_cert_path.is_some(),
                cfg.registry_cache_ttl_secs,
                &cfg.tmp_dir,
                cfg.max_archive_bytes,
                cfg.max_extracted_bytes,
                cfg.max_archive_entries,
                cfg.marketplace_public_key.as_deref(),
                &config,
                &plugins_dir,
            )
            .await?;
        }
        Commands::Completions { shell } => {
            complete::generate_completions(shell);
        }
        Commands::CompleteSlugs => {
            complete::complete_slugs().await?;
        }
        #[cfg(target_os = "linux")]
        Commands::Shim { .. } => unreachable!("__shim is dispatched before the tokio runtime"),
    }

    Ok(())
}

// ---- helpers ----

/// Fail fast (before spawning anything) if the kernel would start without auth
/// and the operator has not explicitly opted in to that.
fn ensure_auth_configured(cfg: &Config) -> Result<()> {
    if cfg.jwt_secret.is_none() && !cfg.allow_no_auth {
        anyhow::bail!(
            "refusing to start without authentication — set `jwt_secret` in config, \
             or set `allow_no_auth: true` to run without auth (insecure)"
        );
    }
    Ok(())
}

fn is_running(pid_file: &std::path::Path) -> Result<bool> {
    let pid = match read_pid(pid_file) {
        Ok(p) => p,
        Err(_) => return Ok(false),
    };
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    // Signal 0 (None) is a pure existence/permission probe — it does not
    // deliver a signal. (SIGCONT would resume a Ctrl-Z'd kernel as a side effect.)
    match kill(Pid::from_raw(pid), None) {
        Ok(_) => Ok(true),
        Err(nix::errno::Errno::ESRCH) => {
            let _ = fs::remove_file(pid_file);
            Ok(false)
        }
        Err(_) => Ok(false),
    }
}

/// Poll a specific PID until it no longer exists (ESRCH) or the timeout elapses.
/// Returns true if the process is gone. Uses signal 0 (no signal delivered).
fn wait_pid_gone(pid: i32, timeout: std::time::Duration) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    let start = std::time::Instant::now();
    loop {
        if let Err(nix::errno::Errno::ESRCH) = kill(Pid::from_raw(pid), None) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn read_pid(pid_file: &std::path::Path) -> Result<i32> {
    let content = fs::read_to_string(pid_file)?;
    let pid = content.trim().parse::<i32>()?;
    Ok(pid)
}

fn write_pid(pid_file: &std::path::Path, pid: i32) -> Result<()> {
    fs::write(pid_file, pid.to_string())?;
    Ok(())
}

fn stop_kernel(pid_file: &std::path::Path) -> Result<()> {
    if !is_running(pid_file)? {
        warn!("kernel is not running");
        return Ok(());
    }
    let pid = read_pid(pid_file)?;
    info!("stopping kernel with PID {}", pid);
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    kill(Pid::from_raw(pid), Signal::SIGTERM)?;
    for _ in 0..10 {
        if !is_running(pid_file)? {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if is_running(pid_file)? {
        warn!("force killing...");
        kill(Pid::from_raw(pid), Signal::SIGKILL)?;
    }
    let _ = fs::remove_file(pid_file);
    info!("kernel stopped");
    Ok(())
}

fn daemonize_and_run(cfg: &Config, config_path: &str, debug: bool) -> Result<()> {
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;

    // Readiness handshake (N4): the child signals over this pipe only after it
    // holds the exclusive pid-file flock. Without it the parent published the
    // PID before the child even started, so `vyn status` could report "running"
    // for a kernel that then aborted on the lock.
    let (mut ready_rx, ready_tx) = UnixStream::pair()?;
    let ready_fd = ready_tx.as_raw_fd();

    let current_exe = std::env::current_exe()?;
    let mut command = Command::new(&current_exe);
    command
        .arg("start")
        .arg("--foreground")
        .arg("--config")
        .arg(config_path)
        .arg("--port")
        .arg(cfg.port.to_string())
        .env("VEYRON_READY_FD", ready_fd.to_string());
    if debug {
        command.arg("--debug");
    }
    unsafe {
        // keep the ready pipe open across exec in the child
        command.pre_exec(move || {
            let fd = std::os::fd::BorrowedFd::borrow_raw(ready_fd);
            let flags =
                nix::fcntl::fcntl(fd, nix::fcntl::FcntlArg::F_GETFD).map_err(nix_err_to_io)?;
            nix::fcntl::fcntl(
                fd,
                nix::fcntl::FcntlArg::F_SETFD(
                    nix::fcntl::FdFlag::from_bits_truncate(flags) & !nix::fcntl::FdFlag::FD_CLOEXEC,
                ),
            )
            .map_err(nix_err_to_io)?;
            Ok(())
        });
    }
    // One open file shared (dup) across stdout+stderr so the two streams share a
    // single write offset and interleave correctly. Two separate File::create
    // handles would each start at offset 0 and clobber each other's output.
    let log = std::fs::File::create(&cfg.log_file)?;
    let log_err = log.try_clone()?;
    let mut child = command.stdout(log).stderr(log_err).spawn()?;
    // parent no longer needs its copy; EOF on ready_rx now means the child is gone
    drop(ready_tx);

    // A healthy child reaches the pid-file write in well under a second.
    ready_rx.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut buf = [0u8; 64];
    let mut line = String::new();
    let pid_line = loop {
        match ready_rx.read(&mut buf) {
            Ok(0) => {
                break Err(anyhow::anyhow!(
                    "kernel child exited before signaling readiness"
                ))
            }
            Ok(n) => {
                line.push_str(&String::from_utf8_lossy(&buf[..n]));
                if line.contains('\n') {
                    break Ok(line);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                break Err(anyhow::anyhow!(
                    "kernel child did not signal readiness within 10s"
                ))
            }
            Err(e) => break Err(anyhow::anyhow!("readiness pipe error: {e}")),
        }
    };

    let pid = child.id() as i32;
    match pid_line {
        Ok(line) => {
            let child_pid = line.trim().parse::<i32>().map_err(|_| {
                anyhow::anyhow!("corrupt readiness line from kernel child: {line:?}")
            })?;
            if child_pid != pid {
                anyhow::bail!("readiness pid {child_pid} does not match child pid {pid}");
            }
            write_pid(&cfg.pid_file, pid)?;
            info!("kernel started in background with PID {}", pid);
            Ok(())
        }
        Err(e) => {
            // reap the failed child so it does not linger as a zombie, and drop
            // any pid file it managed to write before dying
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
            let _ = fs::remove_file(&cfg.pid_file);
            Err(anyhow::anyhow!("kernel failed to start in background: {e}"))
        }
    }
}

fn nix_err_to_io(e: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(e as i32)
}

async fn run_foreground(cfg: Config) -> Result<()> {
    use nix::fcntl::{Flock, FlockArg};

    // O_NOFOLLOW: refuse to write through a symlink planted at the pid path
    // by another local user (AUDIT M-09 — matches the socket/pid-file hardening
    // already applied to socket_path, BUG-006).
    use std::os::unix::fs::OpenOptionsExt;
    let pid_file_handle = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&cfg.pid_file)?;

    let _lock = Flock::lock(pid_file_handle, FlockArg::LockExclusiveNonblock).map_err(|_| {
        anyhow::anyhow!("kernel already running — another instance holds the PID lock")
    })?;

    let pid = std::process::id() as i32;
    fs::write(&cfg.pid_file, pid.to_string())?;
    // N4: the daemon parent waits on this line before publishing the pid file,
    // so it must come only after the flock + pid write above are done. Unset in
    // plain foreground runs (vyn start --foreground in a shell) — no-op there.
    if let Ok(fd) = std::env::var("VEYRON_READY_FD") {
        if let Ok(fd) = fd.parse::<std::os::unix::io::RawFd>() {
            use std::io::Write;
            use std::os::unix::io::FromRawFd;
            let mut pipe = unsafe { std::fs::File::from_raw_fd(fd) };
            let _ = pipe.write_all(format!("{pid}\n").as_bytes());
            // drop closes the fd; the parent sees EOF only after the line
        }
    }
    info!("veyron starting in foreground (port {})", cfg.port);
    let pid_file = cfg.pid_file.clone();
    kernel::Kernel::run(cfg).await?;
    let _ = fs::remove_file(&pid_file);
    info!("kernel stopped");
    Ok(())
}

fn show_logs(log_file: &std::path::Path, lines: usize) -> Result<()> {
    if !log_file.exists() {
        println!("no log file found.");
        return Ok(());
    }
    let content = fs::read_to_string(log_file)?;
    let all_lines: Vec<&str> = content.lines().collect();
    let start = if all_lines.len() > lines {
        all_lines.len() - lines
    } else {
        0
    };
    for line in &all_lines[start..] {
        println!("{}", line);
    }
    Ok(())
}
