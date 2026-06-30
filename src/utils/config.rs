use serde::Deserialize;
use std::path::PathBuf;

/// One plugin entry in the `plugins:` list in config.yaml.
#[derive(Debug, Deserialize, Clone, Default)]
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
    /// Seconds to wait after SIGTERM before SIGKILL on shutdown. 0 = default (5s).
    #[serde(default)]
    pub grace_seconds: u32,
    /// Permissions granted to this plugin by the operator. Any permission declared
    /// in plugin.json but absent here causes the kernel to refuse to load the plugin.
    /// Empty list means "allow all declared permissions" (no restriction).
    #[serde(default)]
    pub permissions: Vec<String>,
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
    /// Explicit opt-out: allow the kernel to start with no JWT auth. Insecure —
    /// any local process can register as any plugin. Must be set deliberately.
    #[serde(default)]
    pub allow_no_auth: bool,
    /// Plugins to auto-spawn on kernel start. Empty by default.
    #[serde(default)]
    pub plugins: Vec<PluginDef>,
    #[serde(default = "default_watchdog_interval")]
    pub watchdog_interval_secs: u64,
    #[serde(default = "default_watchdog_timeout")]
    pub watchdog_timeout_secs: u64,
    #[serde(default = "default_log_buffer_lines")]
    pub log_buffer_lines: usize,
    /// Maximum concurrent UDS plugin connections. Excess connections are dropped immediately.
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
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
fn default_max_connections() -> usize {
    1024
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
            allow_no_auth: false,
            plugins: vec![],
            watchdog_interval_secs: default_watchdog_interval(),
            watchdog_timeout_secs: default_watchdog_timeout(),
            log_buffer_lines: default_log_buffer_lines(),
            max_connections: default_max_connections(),
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
