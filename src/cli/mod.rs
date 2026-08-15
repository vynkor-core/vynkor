pub mod complete;
pub mod devices;
pub mod plugin;
pub mod token;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use plugin::PluginCmd;
use std::path::PathBuf;
use token::TokenCmd;

#[derive(Parser)]
#[command(name = "vyn")]
#[command(about = "Veyron kernel control", version = "0.1.0")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        /// JWT bearer token for a secured kernel. Falls back to VEYRON_JWT_TOKEN.
        #[arg(long)]
        token: Option<String>,
    },
    /// List devices ever seen by the kernel (D-04) — identity, os, state,
    /// last seen, capabilities.
    Devices {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        /// JWT bearer token for a secured kernel. Falls back to VEYRON_JWT_TOKEN.
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

    Stop {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    Restart {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        #[arg(short, long)]
        debug: bool,
    },
    Status {
        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    Logs {
        #[arg(short, long, default_value = "20")]
        lines: usize,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,
    },
    Completions {
        shell: Shell,
    },
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
