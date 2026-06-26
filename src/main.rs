mod api;
mod auth;
mod cli;
mod events;
mod ipc;
mod kernel;
mod metrics;
mod plugins;
mod proto;
mod utils;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use std::fs;
use std::process::Command;
use tracing::{info, warn};
use utils::config::{load_config, Config};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

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
            stop_kernel(&cfg.pid_file)?;
            std::thread::sleep(std::time::Duration::from_millis(500));
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
    }

    Ok(())
}

// ---- helpers ----

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
    let current_exe = std::env::current_exe()?;
    let mut command = Command::new(&current_exe);
    command
        .arg("start")
        .arg("--foreground")
        .arg("--config")
        .arg(config_path)
        .arg("--port")
        .arg(cfg.port.to_string());
    if debug {
        command.arg("--debug");
    }
    let child = command
        .stdout(std::fs::File::create(&cfg.log_file)?)
        .stderr(std::fs::File::create(&cfg.log_file)?)
        .spawn()?;
    let pid = child.id() as i32;
    write_pid(&cfg.pid_file, pid)?;
    info!("kernel started in background with PID {}", pid);
    Ok(())
}

async fn run_foreground(cfg: Config) -> Result<()> {
    use nix::fcntl::{Flock, FlockArg};

    let pid_file_handle = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&cfg.pid_file)?;

    let _lock = Flock::lock(pid_file_handle, FlockArg::LockExclusiveNonblock).map_err(|_| {
        anyhow::anyhow!("kernel already running — another instance holds the PID lock")
    })?;

    let pid = std::process::id() as i32;
    fs::write(&cfg.pid_file, pid.to_string())?;
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
