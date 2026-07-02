use serde::Deserialize;
use std::os::unix::fs::PermissionsExt;
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
    /// RLIMIT_NPROC cap. Unset = `runner::DEFAULT_MAX_PROCS`. Applied to
    /// every plugin, not just sandboxed ones.
    #[serde(default)]
    pub max_procs: Option<u64>,
    /// RLIMIT_AS cap in MiB. Unset = `runner::DEFAULT_MAX_VMEM_MB`. Applied
    /// to every plugin, not just sandboxed ones.
    #[serde(default)]
    pub max_vmem_mb: Option<u64>,
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
    #[serde(default = "default_pid_path")]
    pub pid_file: PathBuf,
    #[serde(default = "default_log_path")]
    pub log_file: PathBuf,
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
    /// HTTP API rate limit: sustained requests per second per JWT token. Default 100.
    #[serde(default)]
    pub api_rate_limit_rps: Option<u32>,
    /// HTTP API rate limit: burst allowance per JWT token on top of sustained rate. Default 20.
    #[serde(default)]
    pub api_rate_limit_burst: Option<u32>,
    /// Per-plugin IPC send rate limit (messages per second per connection). None = unlimited.
    /// Exceeding the limit sends ERR_RATE_LIMITED without disconnecting the plugin.
    #[serde(default)]
    pub ipc_rate_limit_rps: Option<u32>,
    /// TLS certificate (PEM). When both cert and key are set, the WS/HTTP gateway binds TLS.
    #[serde(default)]
    pub tls_cert_path: Option<PathBuf>,
    /// TLS private key (PEM). When both cert and key are set, the WS/HTTP gateway binds TLS.
    #[serde(default)]
    pub tls_key_path: Option<PathBuf>,
    /// Override the plugin registry URL. Default: official veyron-core/veyron-plugins registry.
    /// Set to a private registry URL for air-gapped or enterprise deployments.
    #[serde(default)]
    pub registry_url: Option<String>,
    /// Path this config was loaded from — used by reload_config kernel command.
    #[serde(skip)]
    pub config_file: Option<String>,
}

/// Picks a per-user private directory, preferring `XDG_RUNTIME_DIR`, then the
/// kernel-provided `/run/user/<uid>`, then a private 0o700 directory under
/// `$HOME`. Never falls back to the world-writable shared `/tmp` (BUG-006):
/// if none of those are available, returns `None` so callers can fail closed
/// with a clear error instead of writing into `/tmp`.
fn default_private_dir() -> Option<PathBuf> {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime_dir));
    }

    let uid = nix::unistd::Uid::current().as_raw();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if run_user_dir.is_dir() {
        return Some(run_user_dir);
    }

    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(home).join(".veyron").join("run");
        if std::fs::create_dir_all(&dir).is_ok()
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok()
        {
            return Some(dir);
        }
    }

    None
}

/// Public so SDKs resolve the same default as the kernel when
/// `VEYRON_SOCKET_PATH` is not set.
pub fn default_socket_path() -> String {
    default_private_dir()
        .map(|dir| dir.join("veyron.sock").to_string_lossy().to_string())
        .unwrap_or_default()
}

/// pid/log files got the same symlink-attack surface as the socket
/// (AUDIT M-09) — default them out of the shared `/tmp` the same way.
fn default_pid_path() -> PathBuf {
    default_private_dir()
        .map(|dir| dir.join("veyron.pid"))
        .unwrap_or_else(|| PathBuf::from("veyron.pid"))
}

fn default_log_path() -> PathBuf {
    default_private_dir()
        .map(|dir| dir.join("veyron.log"))
        .unwrap_or_else(|| PathBuf::from("veyron.log"))
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
            pid_file: default_pid_path(),
            log_file: default_log_path(),
            data_dir: PathBuf::from("/var/lib/veyron"),
            socket_path: default_socket_path(),
            jwt_secret: None,
            allow_no_auth: false,
            plugins: vec![],
            watchdog_interval_secs: default_watchdog_interval(),
            watchdog_timeout_secs: default_watchdog_timeout(),
            log_buffer_lines: default_log_buffer_lines(),
            max_connections: default_max_connections(),
            api_rate_limit_rps: None,
            api_rate_limit_burst: None,
            ipc_rate_limit_rps: None,
            tls_cert_path: None,
            tls_key_path: None,
            registry_url: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_path_uses_xdg_runtime_dir() {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let path = default_socket_path();
        assert_eq!(path, "/run/user/1000/veyron.sock");
        std::env::remove_var("XDG_RUNTIME_DIR");
    }

    #[test]
    fn default_socket_path_never_falls_back_to_shared_tmp() {
        std::env::remove_var("XDG_RUNTIME_DIR");
        let path = default_socket_path();
        assert_ne!(
            path, "/tmp/veyron.sock",
            "must not default into world-writable shared /tmp (BUG-006)"
        );
        // Must land in a per-user location: either the kernel-provided
        // /run/user/<uid>, or a private 0o700 dir under $HOME.
        assert!(
            path.starts_with("/run/user/") || path.contains("/.veyron/"),
            "expected a per-user private socket dir, got {path}"
        );
    }

    #[test]
    fn default_pid_and_log_paths_never_land_in_shared_tmp() {
        std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        let pid = default_pid_path();
        let log = default_log_path();
        std::env::remove_var("XDG_RUNTIME_DIR");

        assert_eq!(pid, PathBuf::from("/run/user/1000/veyron.pid"));
        assert_eq!(log, PathBuf::from("/run/user/1000/veyron.log"));
        assert!(
            !pid.starts_with("/tmp"),
            "pid file must not default into /tmp (AUDIT M-09)"
        );
        assert!(
            !log.starts_with("/tmp"),
            "log file must not default into /tmp (AUDIT M-09)"
        );
    }
}
