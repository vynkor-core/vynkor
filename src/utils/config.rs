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
    /// How long the registry cache (`~/.cache/veyron/registry.json`) is considered
    /// fresh before `plugin list --refresh` re-fetches it. Default: 3600 (1h).
    #[serde(default = "default_registry_cache_ttl_secs")]
    pub registry_cache_ttl_secs: u64,
    /// Base directory for scratch/cache files (marketplace registry cache, plugin
    /// install staging). Defaults to a per-user private dir — never the shared
    /// `/tmp` (AUDIT M-09) — via `XDG_RUNTIME_DIR`/`/run/user/<uid>`/`~/.veyron/run`.
    #[serde(default = "default_tmp_dir")]
    pub tmp_dir: PathBuf,
    /// Default action timeout (ms) when a plugin's `ActionRequest.timeout_ms` is 0.
    #[serde(default = "default_action_timeout_ms")]
    pub action_timeout_ms: u32,
    /// Base delay (ms) for exponential plugin restart backoff: `base * 2^restart_count`.
    #[serde(default = "default_restart_backoff_base_ms")]
    pub restart_backoff_base_ms: u64,
    /// Ceiling (ms) for exponential plugin restart backoff.
    #[serde(default = "default_restart_backoff_max_ms")]
    pub restart_backoff_max_ms: u64,
    /// Seconds to wait after SIGTERM before SIGKILL when a plugin doesn't set its
    /// own `grace_seconds`. Overridable per-plugin via `plugins[].grace_seconds`.
    #[serde(default = "default_grace_seconds")]
    pub default_grace_seconds: u32,
    /// Max delivery attempts for an event before the event bus marks it dead.
    #[serde(default = "default_event_max_retries")]
    pub event_max_retries: u32,
    /// Seconds terminal (delivered/dead) events are kept in `events.db` before
    /// being pruned.
    #[serde(default = "default_event_retention_secs")]
    pub event_retention_secs: u64,
    /// Capacity of the kernel's inbound IPC message channel (backpressure bound).
    #[serde(default = "default_router_channel_capacity")]
    pub router_channel_capacity: usize,
    /// Seconds an incomplete fragmented message is retained before being
    /// discarded (fragment-reassembly memory bound).
    #[serde(default = "default_fragment_timeout_secs")]
    pub fragment_timeout_secs: u64,
    /// Max concurrent in-flight fragment-reassembly streams per connection.
    #[serde(default = "default_max_reassembly_streams")]
    pub max_reassembly_streams: usize,
    /// Consecutive protocol errors from one connection before it is throttled.
    #[serde(default = "default_max_conn_errors")]
    pub max_conn_errors: u32,
    /// Cap on the per-connection error-budget map before idle entries are pruned.
    #[serde(default = "default_max_tracked_error_conns")]
    pub max_tracked_error_conns: usize,
    /// Seconds to wait for a WebSocket upgrade handshake before returning 408.
    #[serde(default = "default_ws_handshake_timeout_secs")]
    pub ws_handshake_timeout_secs: u64,
    /// Max size (bytes) of a downloaded plugin archive before install is aborted.
    #[serde(default = "default_max_archive_bytes")]
    pub max_archive_bytes: u64,
    /// Max total decompressed size (bytes) an installed archive may extract to.
    #[serde(default = "default_max_extracted_bytes")]
    pub max_extracted_bytes: u64,
    /// Max number of entries an installed archive may contain.
    #[serde(default = "default_max_archive_entries")]
    pub max_archive_entries: usize,
    /// Path this config was loaded from — used by reload_config kernel command.
    #[serde(skip)]
    pub config_file: Option<String>,
}

/// Public so SDKs resolve the same default as the kernel when
/// `VEYRON_SOCKET_PATH` is not set.
pub use veyron_wire::socket::default_socket_path;

/// pid/log files got the same symlink-attack surface as the socket
/// (AUDIT M-09) — default them out of the shared `/tmp` the same way.
fn default_pid_path() -> PathBuf {
    veyron_wire::socket::default_private_dir()
        .map(|dir| dir.join("veyron.pid"))
        .unwrap_or_else(|| PathBuf::from("veyron.pid"))
}

fn default_log_path() -> PathBuf {
    veyron_wire::socket::default_private_dir()
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
fn default_registry_cache_ttl_secs() -> u64 {
    3600
}
/// Per-user private scratch dir — mirrors `default_pid_path`/`default_log_path`'s
/// refusal to fall back into the shared, world-writable `/tmp` (AUDIT M-09).
fn default_tmp_dir() -> PathBuf {
    veyron_wire::socket::default_private_dir().unwrap_or_else(std::env::temp_dir)
}
fn default_action_timeout_ms() -> u32 {
    30_000
}
fn default_restart_backoff_base_ms() -> u64 {
    100
}
fn default_restart_backoff_max_ms() -> u64 {
    30_000
}
fn default_grace_seconds() -> u32 {
    5
}
fn default_event_max_retries() -> u32 {
    5
}
fn default_event_retention_secs() -> u64 {
    3600
}
fn default_router_channel_capacity() -> usize {
    1024
}
fn default_fragment_timeout_secs() -> u64 {
    30
}
fn default_max_reassembly_streams() -> usize {
    64
}
fn default_max_conn_errors() -> u32 {
    16
}
fn default_max_tracked_error_conns() -> usize {
    8192
}
fn default_ws_handshake_timeout_secs() -> u64 {
    5
}
fn default_max_archive_bytes() -> u64 {
    256 * 1024 * 1024
}
fn default_max_extracted_bytes() -> u64 {
    1024 * 1024 * 1024
}
fn default_max_archive_entries() -> usize {
    10_000
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
            registry_cache_ttl_secs: default_registry_cache_ttl_secs(),
            tmp_dir: default_tmp_dir(),
            action_timeout_ms: default_action_timeout_ms(),
            restart_backoff_base_ms: default_restart_backoff_base_ms(),
            restart_backoff_max_ms: default_restart_backoff_max_ms(),
            default_grace_seconds: default_grace_seconds(),
            event_max_retries: default_event_max_retries(),
            event_retention_secs: default_event_retention_secs(),
            router_channel_capacity: default_router_channel_capacity(),
            fragment_timeout_secs: default_fragment_timeout_secs(),
            max_reassembly_streams: default_max_reassembly_streams(),
            max_conn_errors: default_max_conn_errors(),
            max_tracked_error_conns: default_max_tracked_error_conns(),
            ws_handshake_timeout_secs: default_ws_handshake_timeout_secs(),
            max_archive_bytes: default_max_archive_bytes(),
            max_extracted_bytes: default_max_extracted_bytes(),
            max_archive_entries: default_max_archive_entries(),
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
        temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/1000"), || {
            let path = default_socket_path();
            assert_eq!(path, "/run/user/1000/veyron.sock");
        });
    }

    #[test]
    fn default_socket_path_never_falls_back_to_shared_tmp() {
        temp_env::with_var_unset("XDG_RUNTIME_DIR", || {
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
        });
    }

    #[test]
    fn default_pid_and_log_paths_never_land_in_shared_tmp() {
        temp_env::with_var("XDG_RUNTIME_DIR", Some("/run/user/1000"), || {
            let pid = default_pid_path();
            let log = default_log_path();

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
        });
    }
}
