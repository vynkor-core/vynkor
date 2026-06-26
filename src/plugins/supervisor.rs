use crate::events::bus::EventBus;
use crate::ipc::framing::Frame;
use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::{envelope, Envelope, Event, Ping};
use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use prost::Message;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use std::process::Stdio;
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use metrics::counter;
use tracing::{info, warn};

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
    /// Isolate plugin in new PID + network namespaces (Linux only).
    pub sandbox: bool,
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
    log_buffers: Arc<DashMap<String, Arc<Mutex<VecDeque<String>>>>>,
    max_log_lines: usize,
    event_bus: Option<Arc<EventBus>>,
    plugin_registry: Option<Arc<PluginRegistry>>,
    /// Plugins whose next exit is a manual restart (POST /restart) and must be
    /// respawned regardless of restart_policy / max_restarts.
    forced_restarts: Arc<DashMap<String, ()>>,
}

impl PluginSupervisor {
    #[allow(dead_code)]
    pub fn new(socket_path: &str) -> Self {
        Self::with_log_lines(socket_path, 1000)
    }


    pub fn with_log_lines(socket_path: &str, max_log_lines: usize) -> Self {
        Self::with_events(socket_path, max_log_lines, None, None)
    }

    pub fn with_events(
        socket_path: &str,
        max_log_lines: usize,
        event_bus: Option<Arc<EventBus>>,
        plugin_registry: Option<Arc<PluginRegistry>>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<ExitEvent>(64);
        PluginSupervisor {
            socket_path: socket_path.to_string(),
            entries: Arc::new(DashMap::new()),
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            log_buffers: Arc::new(DashMap::new()),
            max_log_lines,
            event_bus,
            plugin_registry,
            forced_restarts: Arc::new(DashMap::new()),
        }
    }

    pub async fn get_logs(&self, plugin_id: &str, n: usize) -> Vec<String> {
        let buf = match self.log_buffers.get(plugin_id) {
            Some(b) => b.clone(),
            None => return vec![],
        };
        let locked = buf.lock().await;
        let skip = locked.len().saturating_sub(n);
        locked.iter().skip(skip).cloned().collect()
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
            .env("VEYRON_SOCKET_PATH", &self.socket_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for kv in &config.env {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
        #[cfg(target_os = "linux")]
        if config.sandbox {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.as_std_mut().pre_exec(crate::plugins::runner::sandbox_pre_exec);
            }
        }

        let mut child = cmd.spawn().map_err(VeyronError::Io)?;

        let pid = child
            .id()
            .ok_or_else(|| VeyronError::Internal("no pid".into()))?;
        let plugin_id = config.plugin_id.clone();

        let log_buf = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(
            self.max_log_lines,
        )));
        self.log_buffers.insert(plugin_id.clone(), Arc::clone(&log_buf));

        let max_lines = self.max_log_lines;

        if let Some(stdout) = child.stdout.take() {
            let buf = Arc::clone(&log_buf);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut locked = buf.lock().await;
                    if locked.len() >= max_lines {
                        locked.pop_front();
                    }
                    locked.push_back(line);
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let buf = Arc::clone(&log_buf);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut locked = buf.lock().await;
                    if locked.len() >= max_lines {
                        locked.pop_front();
                    }
                    locked.push_back(line);
                }
            });
        }

        info!(plugin_id = %plugin_id, pid = pid, restart_count = restart_count, "plugin spawned");
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

        // Explicit stop overrides any pending manual restart.
        self.forced_restarts.remove(plugin_id);
        let pid = nix::unistd::Pid::from_raw(entry.1.pid as i32);
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        Ok(())
    }

    // Sends SIGTERM without removing the entry so monitor_loop restarts the plugin.
    // Marks the plugin for forced restart so it respawns even under a Never /
    // OnFailure policy or after max_restarts — a manual restart overrides policy.
    pub async fn restart_plugin(&self, plugin_id: &str) -> Result<(), VeyronError> {
        let pid = self
            .entries
            .get(plugin_id)
            .map(|e| e.pid)
            .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

        self.forced_restarts.insert(plugin_id.to_string(), ());
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
            // A manual restart (POST /restart) forces respawn regardless of policy.
            let forced = self.forced_restarts.remove(&event.plugin_id).is_some();
            let decision = self.entries.get(&event.plugin_id).and_then(|entry| {
                let should = forced
                    || match entry.config.restart_policy {
                        RestartPolicy::Never => false,
                        RestartPolicy::Always => entry.restart_count < entry.config.max_restarts,
                        RestartPolicy::OnFailure => {
                            !event.success && entry.restart_count < entry.config.max_restarts
                        }
                    };
                if should {
                    Some((entry.config.clone(), entry.restart_count))
                } else {
                    None
                }
            });

            let will_restart = decision.is_some();
            let restart_count = self
                .entries
                .get(&event.plugin_id)
                .map(|e| e.restart_count)
                .unwrap_or(0);
            info!(
                plugin_id = %event.plugin_id,
                success = event.success,
                will_restart = will_restart,
                restart_count = restart_count,
                "plugin exited"
            );

            if let (Some(bus), Some(reg)) = (&self.event_bus, &self.plugin_registry) {
                let payload = format!(
                    r#"{{"plugin_id":"{}","restart_count":{},"will_restart":{}}}"#,
                    event.plugin_id, restart_count, will_restart
                );
                bus.publish(
                    Event {
                        event_id: format!("sys-died-{}-{}", event.plugin_id, restart_count),
                        event_type: "system.plugin_died".to_string(),
                        payload_json: payload.into_bytes(),
                        retry_count: 0,
                    },
                    reg,
                )
                .await;
            }

            match decision {
                Some((config, prev_count)) => {
                    let new_count = prev_count + 1;
                    info!(
                        plugin_id = %config.plugin_id,
                        restart_count = new_count,
                        "restarting plugin"
                    );
                    counter!("plugin_restarts_total", "plugin_id" => config.plugin_id.clone())
                        .increment(1);
                    tokio::time::sleep(backoff_delay(new_count)).await;
                    let _ = self.spawn_internal(config, new_count).await;
                }
                None => {
                    // max restarts reached or Never policy — process is gone, entry stays
                }
            }
        }
    }

    pub async fn watchdog_loop(
        self: Arc<Self>,
        registry: Arc<PluginRegistry>,
        interval: Duration,
        timeout: Duration,
    ) {
        let deadline = interval + timeout;
        loop {
            tokio::time::sleep(interval).await;

            let supervised: Vec<(String, u32)> = self
                .entries
                .iter()
                .map(|e| (e.key().clone(), e.value().pid))
                .collect();

            for (plugin_id, pid) in supervised {
                if let Some(last_pong) = registry.last_pong(&plugin_id) {
                    if last_pong.elapsed() > deadline {
                        warn!(plugin_id = %plugin_id, "watchdog: plugin unresponsive, sending SIGKILL");
                        let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
                        let _ = nix::sys::signal::kill(
                            nix_pid,
                            nix::sys::signal::Signal::SIGKILL,
                        );
                        registry.record_pong(&plugin_id);
                        continue;
                    }
                }

                if let Some(reg_entry) = registry.get(&plugin_id) {
                    let timestamp = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let env = Envelope {
                        payload: Some(envelope::Payload::Ping(Ping { timestamp })),
                        ..Default::default()
                    };
                    let mut payload = Vec::new();
                    if env.encode(&mut payload).is_ok() {
                        let crc = crc32fast::hash(&payload);
                        let mut target = [0u8; 32];
                        target[..6].copy_from_slice(b"client");
                        let frame = Frame {
                            magic: 0x5652,
                            flags: 0,
                            length: payload.len() as u32,
                            target,
                            crc32: crc,
                            payload,
                            mac: None,
                        };
                        let _ = reg_entry.write_tx.send(frame).await;
                    }
                }
            }
        }
    }
}

fn backoff_delay(restart_count: u32) -> Duration {
    let ms = 100u64.saturating_mul(1u64 << restart_count.min(8));
    Duration::from_millis(ms.min(30_000))
}
