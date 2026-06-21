use serde::Deserialize;
use std::path::PathBuf;

/// One plugin entry in the `plugins:` list in config.yaml.
#[derive(Debug, Deserialize, Clone)]
pub struct PluginDef {
    pub id: String,
    pub binary: String,
    #[serde(default = "default_restart_policy")]
    pub restart: String,
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub sandbox: bool,
}

fn default_restart_policy() -> String {
    "on-failure".to_string()
}
fn default_max_restarts() -> u32 {
    5
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub port: u16,
    pub log_level: String,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    #[allow(dead_code)]
    pub data_dir: PathBuf,
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
    #[serde(default)]
    pub jwt_secret: Option<String>,
    /// Plugins to auto-spawn on kernel start. Empty by default.
    #[serde(default)]
    pub plugins: Vec<PluginDef>,
    #[serde(default = "default_watchdog_interval")]
    pub watchdog_interval_secs: u64,
    #[serde(default = "default_watchdog_timeout")]
    pub watchdog_timeout_secs: u64,
    #[serde(default = "default_log_buffer_lines")]
    pub log_buffer_lines: usize,
    /// Path this config was loaded from — used by reload_config kernel command.
    #[serde(skip)]
    pub config_file: Option<String>,
}

fn default_socket_path() -> String {
    "/tmp/veyron.sock".to_string()
}
fn default_watchdog_interval() -> u64 {
    30
}
fn default_watchdog_timeout() -> u64 {
    10
}
fn default_log_buffer_lines() -> usize {
    1000
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8000,
            log_level: "info".to_string(),
            pid_file: PathBuf::from("/tmp/veyron.pid"),
            log_file: PathBuf::from("/tmp/veyron.log"),
            data_dir: PathBuf::from("/var/lib/veyron"),
            socket_path: default_socket_path(),
            jwt_secret: None,
            plugins: vec![],
            watchdog_interval_secs: default_watchdog_interval(),
            watchdog_timeout_secs: default_watchdog_timeout(),
            log_buffer_lines: default_log_buffer_lines(),
            config_file: None,
        }
    }
}

pub fn load_config(path: &str) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let mut config: Config = serde_yaml::from_str(&content)?;
    config.config_file = Some(path.to_string());
    Ok(config)
}
