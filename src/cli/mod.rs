use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "vyn")]
#[command(about = "Veyron kernel control", version = "0.1.0")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
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
}
