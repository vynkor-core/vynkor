use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

#[allow(dead_code)]
#[derive(Clone, Default)]
pub enum RestartPolicy {
    Always,
    #[default]
    OnFailure,
    Never,
}

#[derive(Clone, Default)]
pub struct PluginConfig {
    pub plugin_id: String,
    pub binary_path: PathBuf,
    pub args: Vec<String>,
    /// Additional environment variables as "KEY=VALUE" strings.
    pub env: Vec<String>,
    pub restart_policy: RestartPolicy,
    pub max_restarts: u32,
}

#[allow(dead_code)]
pub struct PluginProcess {
    pub plugin_id: String,
    pub pid: u32,
}

struct PluginEntry {
    config: PluginConfig,
    restart_count: u32,
    pid: u32,
}

struct ExitEvent {
    plugin_id: String,
    success: bool,
}

pub struct PluginSupervisor {
    socket_path: String,
    entries: Arc<DashMap<String, PluginEntry>>,
    event_tx: mpsc::Sender<ExitEvent>,
    event_rx: Arc<Mutex<mpsc::Receiver<ExitEvent>>>,
}

impl PluginSupervisor {
    pub fn new(socket_path: &str) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<ExitEvent>(64);
        PluginSupervisor {
            socket_path: socket_path.to_string(),
            entries: Arc::new(DashMap::new()),
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
        }
    }

    #[allow(dead_code)]
    pub async fn spawn_plugin(&self, config: PluginConfig) -> Result<PluginProcess, VeyronError> {
        self.spawn_internal(config, 0).await
    }

    async fn spawn_internal(
        &self,
        config: PluginConfig,
        restart_count: u32,
    ) -> Result<PluginProcess, VeyronError> {
        let mut cmd = Command::new(&config.binary_path);
        cmd.args(&config.args)
            .env("VEYRON_SOCKET_PATH", &self.socket_path);
        for kv in &config.env {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
        let mut child = cmd.spawn().map_err(VeyronError::Io)?;

        let pid = child
            .id()
            .ok_or_else(|| VeyronError::Internal("no pid".into()))?;
        let plugin_id = config.plugin_id.clone();

        self.entries.insert(
            plugin_id.clone(),
            PluginEntry {
                config,
                restart_count,
                pid,
            },
        );

        let tx = self.event_tx.clone();
        let id = plugin_id.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let success = status.map(|s| s.success()).unwrap_or(false);
            let _ = tx
                .send(ExitEvent {
                    plugin_id: id,
                    success,
                })
                .await;
        });

        Ok(PluginProcess { plugin_id, pid })
    }

    #[allow(dead_code)]
    pub async fn stop_plugin(&self, plugin_id: &str) -> Result<(), VeyronError> {
        let entry = self
            .entries
            .remove(plugin_id)
            .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

        let pid = nix::unistd::Pid::from_raw(entry.1.pid as i32);
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        Ok(())
    }

    // Sends SIGTERM without removing the entry so monitor_loop restarts the plugin.
    pub async fn restart_plugin(&self, plugin_id: &str) -> Result<(), VeyronError> {
        let pid = self
            .entries
            .get(plugin_id)
            .map(|e| e.pid)
            .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

        let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
        let _ = nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGTERM);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_running(&self, plugin_id: &str) -> bool {
        self.entries.contains_key(plugin_id)
    }

    #[allow(dead_code)]
    pub fn restart_count(&self, plugin_id: &str) -> Option<u32> {
        self.entries.get(plugin_id).map(|e| e.restart_count)
    }

    pub async fn monitor_loop(self: &Arc<Self>) {
        let mut rx = self.event_rx.lock().await;
        while let Some(event) = rx.recv().await {
            let decision = self.entries.get(&event.plugin_id).and_then(|entry| {
                let should = match entry.config.restart_policy {
                    RestartPolicy::Never => false,
                    RestartPolicy::Always => entry.restart_count < entry.config.max_restarts,
                    RestartPolicy::OnFailure => {
                        !event.success && entry.restart_count < entry.config.max_restarts
                    }
                };
                if should {
                    Some(entry.config.clone())
                } else {
                    None
                }
            });

            match decision {
                Some(config) => {
                    let new_count = self
                        .entries
                        .get(&event.plugin_id)
                        .map(|e| e.restart_count + 1)
                        .unwrap_or(1);
                    let _ = self.spawn_internal(config, new_count).await;
                }
                None => {
                    // max restarts reached or Never policy — process is gone, entry stays
                }
            }
        }
    }
}
