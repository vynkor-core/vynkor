pub mod complete;
pub mod plugin;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use plugin::PluginCmd;

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
    Start {
        #[arg(short, long)]
        foreground: bool,

        #[arg(short, long)]
        port: Option<u16>,

        #[arg(short, long, default_value = "config.yaml")]
        config: String,

        #[arg(short, long)]
        debug: bool,
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
}
