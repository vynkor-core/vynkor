pub mod complete;
pub mod device;
pub mod devices;
pub mod plugin;
pub mod token;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use device::DeviceCmd;
use plugin::PluginCmd;
use std::path::PathBuf;
use token::TokenCmd;

#[derive(Parser)]
#[command(name = "vyn", about = "vynkor kernel control", version = env!("CARGO_PKG_VERSION"))]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Manage plugins (list/start/stop/logs; marketplace via vynm).
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        /// JWT bearer token for a secured kernel. Falls back to VYN_JWT_TOKEN.
        #[arg(long)]
        token: Option<String>,
    },
    /// List devices ever seen by the kernel (D-04) — identity, os, state,
    /// last seen, capabilities.
    Devices {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        /// JWT bearer token for a secured kernel. Falls back to VYN_JWT_TOKEN.
        #[arg(long)]
        token: Option<String>,
    },
    /// Mint JWTs offline (D-07) — per-device tokens for remote device agents.
    /// Reads jwt_secret from the config file; the kernel need not be running.
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Pair a remote device agent by QR code (D-14 companion) — render a
    /// `vynkor://pair` QR/link with the host URL, device id, JWT, frame-MAC
    /// secret, and served TLS cert so the Android app can scan-and-connect.
    Device {
        #[command(subcommand)]
        cmd: DeviceCmd,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Start the kernel daemon (background by default, --foreground to stay attached).
    Start {
        #[arg(short, long)]
        foreground: bool,

        #[arg(short, long)]
        port: Option<u16>,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        #[arg(short, long)]
        debug: bool,

        /// Kernel role: host (default) or client (mirrors plugins to a host kernel, D-06).
        #[arg(long, value_parser = ["client", "host"])]
        role: Option<String>,

        /// Host kernel base URL for the client bridge; http/https base gets /ws appended (D-06).
        #[arg(long)]
        bridge_url: Option<String>,

        /// JWT for the host kernel's WS gateway (D-06).
        #[arg(long)]
        bridge_token: Option<String>,

        /// Host kernel's jwt_secret, needed to derive the bridge frame MAC key (D-06).
        #[arg(long)]
        bridge_secret: Option<String>,

        /// Local plugin id to mirror to the host; repeatable (D-06).
        #[arg(long)]
        bridge_mirror: Vec<String>,
    },

    /// Stop the running kernel daemon.
    Stop {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Stop and start the kernel daemon.
    Restart {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        #[arg(short, long)]
        debug: bool,
    },
    /// Show kernel status — pid, uptime, supervised plugin count.
    Status {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Print the last N lines of the kernel log file.
    Logs {
        #[arg(short, long, default_value = "20")]
        lines: usize,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    /// Generate shell completion scripts for the given shell.
    Completions { shell: Shell },
    #[command(name = "__complete-slugs", hide = true)]
    CompleteSlugs,
    /// Internal sandbox shim entrypoint (R9-02): re-exec'd by the supervisor
    /// to run a plugin inside a private PID namespace. Hidden from help.
    #[cfg(target_os = "linux")]
    #[command(name = "__shim", hide = true)]
    Shim {
        /// Plugin binary to run inside the namespace.
        plugin_binary: PathBuf,
        /// Plugin argv, passed through verbatim.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        args: Vec<String>,
    },
}
