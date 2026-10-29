//! embedding api: config validation and the single-instance lock, shared by
//! the `vyn` cli and in-process embedders (desktop app).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::utils::config::{Config, Role};

/// fail fast (before spawning anything) on a config the kernel cannot run:
/// no auth without explicit opt-in, or an unusable client bridge block.
pub fn validate_config(cfg: &Config) -> anyhow::Result<()> {
    ensure_auth_configured(cfg)?;
    validate_bridge_config(cfg)
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

/// A client kernel is unusable without a reachable host and a non-empty mirror
/// list, and mirroring a plugin that is not configured would silently no-op.
fn validate_bridge_config(cfg: &Config) -> anyhow::Result<()> {
    if cfg.role != Role::Client {
        return Ok(());
    }
    let bridge = cfg.bridge.as_ref().ok_or_else(|| {
        anyhow::anyhow!("role: client requires a `bridge:` block in config or --bridge-url")
    })?;
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
    if bridge.ca_cert_path.is_some()
        && (bridge.host_url.starts_with("ws://") || bridge.host_url.starts_with("http://"))
    {
        anyhow::bail!(
            "bridge.ca_cert_path is set but host_url '{}' is plaintext: use https:// or wss://",
            bridge.host_url
        );
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
