//! embedding api: config validation and the single-instance lock, shared by
//! the `vyn` cli and in-process embedders (desktop app).

use std::collections::HashSet;
use std::fs::{self, File};
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::bridge::BridgeStatus;
use crate::events::bus::EventBus;
use crate::kernel::{Kernel, RunOptions};
use crate::plugins::registry::PluginRegistry;
use crate::utils::config::{Config, Role, TransportConfig};
use std::sync::Arc;
use tokio::sync::watch;

/// fail fast (before spawning anything) on a config the kernel cannot run:
/// no auth without explicit opt-in, or an unusable client bridge block.
pub fn validate_config(cfg: &Config) -> anyhow::Result<()> {
    // bridge first: same order `vyn start` always reported errors in
    validate_bridge_config(cfg)?;
    ensure_auth_configured(cfg)
}

pub fn ensure_auth_configured(cfg: &Config) -> anyhow::Result<()> {
    if cfg.jwt_secret.is_none() && !cfg.allow_no_auth {
        anyhow::bail!(
            "refusing to start without authentication — set `jwt_secret` in config, \
             or set `allow_no_auth: true` to run without auth (insecure)"
        );
    }
    Ok(())
}

fn is_plaintext(url: &str) -> bool {
    url.starts_with("ws://") || url.starts_with("http://")
}

/// A client kernel is unusable without a reachable host and a non-empty mirror
/// list, and mirroring a plugin that is not configured would silently no-op.
fn validate_bridge_config(cfg: &Config) -> anyhow::Result<()> {
    if cfg.role != Role::Client {
        return Ok(());
    }
    let bridge = cfg.bridge.as_ref().ok_or_else(|| {
        anyhow::anyhow!("role: client requires a `bridge:` block in config or --bridge-url")
    })?;
    let has_url = !bridge.host_url.is_empty();
    if has_url && !bridge.transports.is_empty() {
        anyhow::bail!("bridge: set either host_url or transports, not both");
    }
    if !has_url && bridge.transports.is_empty() {
        anyhow::bail!("bridge: host_url or transports is required");
    }
    if has_url {
        let url_ok = ["ws://", "wss://", "http://", "https://"]
            .iter()
            .any(|p| bridge.host_url.starts_with(p));
        if !url_ok {
            anyhow::bail!(
                "invalid bridge host_url '{}': expected http(s):// or ws(s)://",
                bridge.host_url
            );
        }
        // a pin on a plaintext url would silently pin nothing
        if bridge.ca_cert_path.is_some() && is_plaintext(&bridge.host_url) {
            anyhow::bail!(
                "bridge.ca_cert_path is set but host_url '{}' is plaintext: use https:// or wss://",
                bridge.host_url
            );
        }
    }
    for (i, t) in bridge.transports.iter().enumerate() {
        match t {
            TransportConfig::Direct { url, ca_cert_path } => {
                if !["ws://", "wss://", "http://", "https://"]
                    .iter()
                    .any(|p| url.starts_with(p))
                {
                    anyhow::bail!(
                        "invalid bridge.transports[{i}] url '{url}': expected http(s):// or ws(s)://"
                    );
                }
                if ca_cert_path.is_some() && is_plaintext(url) {
                    anyhow::bail!(
                        "bridge.transports[{i}].ca_cert_path is set but url '{url}' is plaintext: use https:// or wss://"
                    );
                }
            }
            TransportConfig::Relay { url, .. } => {
                if !(url.starts_with("wss://") || url.starts_with("https://")) {
                    anyhow::bail!(
                        "invalid bridge.transports[{i}] relay url '{url}': expected wss:// or https://"
                    );
                }
            }
        }
    }
    if bridge.mirror.is_empty() {
        anyhow::bail!(
            "role: client requires at least one mirrored plugin (`bridge.mirror` or --bridge-mirror)"
        );
    }
    let configured: HashSet<&str> = cfg.plugins.iter().map(|p| p.id.as_str()).collect();
    for cap in &bridge.mirror {
        if !configured.contains(cap.as_str()) {
            anyhow::bail!(
                "bridge mirror '{cap}' is not a configured plugin (plugins: [{}])",
                configured.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }
    Ok(())
}

/// run the kernel in-process until `shutdown` resolves. Takes the single-instance
/// lock first (error downcasts to `InstanceLockError`); installs no signal
/// handlers — the caller owns shutdown.
pub async fn run<F: Future<Output = ()>>(
    cfg: Config,
    shutdown: F,
    bridge_status: Option<watch::Sender<BridgeStatus>>,
) -> anyhow::Result<()> {
    validate_config(&cfg)?;
    let _lock = InstanceLock::acquire(&cfg.pid_file)?;
    Kernel::run_with_options(
        cfg,
        Arc::new(PluginRegistry::new()),
        Arc::new(EventBus::new()),
        shutdown,
        RunOptions { bridge_status },
    )
    .await
}

#[derive(Debug)]
pub enum InstanceLockError {
    AlreadyRunning { pid: Option<i32> },
    Io(std::io::Error),
}

impl std::fmt::Display for InstanceLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning { pid } => {
                write!(
                    f,
                    "kernel already running — another instance holds the PID lock"
                )?;
                if let Some(pid) = pid {
                    write!(f, " (pid {pid})")?;
                }
                Ok(())
            }
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for InstanceLockError {}

impl From<std::io::Error> for InstanceLockError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// exclusive pid-file flock; the pid file is removed on drop.
pub struct InstanceLock {
    _lock: nix::fcntl::Flock<File>,
    path: PathBuf,
}

impl InstanceLock {
    pub fn acquire(pid_file: &Path) -> Result<InstanceLock, InstanceLockError> {
        use nix::fcntl::{Flock, FlockArg};
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW: refuse to write through a symlink planted at the pid path
        // by another local user (AUDIT M-09 — matches the socket/pid-file hardening
        // already applied to socket_path, BUG-006).
        let handle = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(pid_file)?;
        let mut lock = match Flock::lock(handle, FlockArg::LockExclusiveNonblock) {
            Ok(l) => l,
            Err((mut h, _)) => {
                // read the holder's pid without truncating: the loser must not
                // clobber the file the winner owns
                let mut s = String::new();
                let _ = h.read_to_string(&mut s);
                return Err(InstanceLockError::AlreadyRunning {
                    pid: s.trim().parse().ok(),
                });
            }
        };
        // truncate only after the lock is ours
        lock.set_len(0)?;
        lock.write_all(std::process::id().to_string().as_bytes())?;
        lock.flush()?;
        Ok(InstanceLock {
            _lock: lock,
            path: pid_file.to_path_buf(),
        })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::config::BridgeConfig;

    #[test]
    fn ca_cert_path_with_plaintext_host_url_is_rejected() {
        let mut cfg = Config {
            role: Role::Client,
            allow_no_auth: true,
            bridge: Some(BridgeConfig {
                host_url: "http://h:8888".into(),
                mirror: vec!["geo".into()],
                ca_cert_path: Some("/x/host-cert.pem".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("ca_cert_path") && err.contains("plaintext"),
            "{err}"
        );
        cfg.bridge.as_mut().unwrap().host_url = "ws://h:8888/ws".into();
        assert!(validate_config(&cfg).is_err());
    }

    fn client_cfg(bridge: BridgeConfig) -> Config {
        Config {
            role: Role::Client,
            allow_no_auth: true,
            bridge: Some(bridge),
            plugins: vec![crate::utils::config::PluginDef {
                id: "geo".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn direct(url: &str, pin: bool) -> TransportConfig {
        TransportConfig::Direct {
            url: url.into(),
            ca_cert_path: pin.then(|| "/x/c.pem".into()),
        }
    }

    #[test]
    fn host_url_and_transports_conflict() {
        let cfg = client_cfg(BridgeConfig {
            host_url: "https://h:1".into(),
            transports: vec![direct("https://h:1", false)],
            mirror: vec!["geo".into()],
            ..Default::default()
        });
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("host_url") && err.contains("transports"),
            "{err}"
        );
    }

    #[test]
    fn empty_bridge_is_rejected() {
        let cfg = client_cfg(BridgeConfig {
            mirror: vec!["geo".into()],
            ..Default::default()
        });
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(err.contains("host_url or transports is required"), "{err}");
    }

    #[test]
    fn plaintext_direct_with_pin_names_index() {
        let cfg = client_cfg(BridgeConfig {
            transports: vec![direct("https://h:1", true), direct("ws://h:2/ws", true)],
            mirror: vec!["geo".into()],
            ..Default::default()
        });
        let err = validate_config(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("bridge.transports[1]") && err.contains("plaintext"),
            "{err}"
        );
    }

    #[test]
    fn relay_requires_tls_url() {
        let relay = |url: &str| {
            client_cfg(BridgeConfig {
                transports: vec![TransportConfig::Relay {
                    url: url.into(),
                    key: None,
                }],
                mirror: vec!["geo".into()],
                ..Default::default()
            })
        };
        let err = validate_config(&relay("ws://r/ws"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("bridge.transports[0]"), "{err}");
        assert!(validate_config(&relay("wss://r/ws")).is_ok());
    }

    #[test]
    fn no_auth_is_refused() {
        let err = validate_config(&Config::default()).unwrap_err().to_string();
        assert!(
            err.contains("refusing to start without authentication"),
            "{err}"
        );
    }

    #[test]
    fn second_lock_reports_holder_pid_and_keeps_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("vyn.pid");
        let _first = InstanceLock::acquire(&pid_file).unwrap();
        let me = std::process::id() as i32;
        match InstanceLock::acquire(&pid_file) {
            Err(InstanceLockError::AlreadyRunning { pid }) => assert_eq!(pid, Some(me)),
            other => panic!("expected AlreadyRunning, got {:?}", other.map(|_| ())),
        }
        assert_eq!(fs::read_to_string(&pid_file).unwrap(), me.to_string());
    }

    #[test]
    fn drop_removes_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("vyn.pid");
        let lock = InstanceLock::acquire(&pid_file).unwrap();
        assert!(pid_file.exists());
        drop(lock);
        assert!(!pid_file.exists());
    }
}
