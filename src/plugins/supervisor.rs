use crate::events::bus::EventBus;
use crate::ipc::connection::out_frame;
use crate::ipc::framing::Frame;
use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::{envelope, Envelope, Event, Ping};
use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use metrics::{counter, gauge};
use prost::Message;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

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
    /// Seconds to wait after SIGTERM before SIGKILL. 0 means use default (5s).
    pub grace_seconds: u32,
    /// RLIMIT_NPROC cap. None = `runner::DEFAULT_MAX_PROCS`. Applied
    /// unconditionally (not gated by `sandbox`).
    pub max_procs: Option<u64>,
    /// RLIMIT_AS cap in MiB. None = `runner::DEFAULT_MAX_VMEM_MB`. Applied
    /// unconditionally (not gated by `sandbox`).
    pub max_vmem_mb: Option<u64>,
}

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
    /// Final restart_count for plugins that have been removed from `entries` after
    /// exhausting their restart budget (VULN-018). Preserved for historical lookup.
    stopped_counts: Arc<DashMap<String, u32>>,
}

impl PluginSupervisor {
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
            stopped_counts: Arc::new(DashMap::new()),
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
        let max_procs = config
            .max_procs
            .unwrap_or(crate::plugins::runner::DEFAULT_MAX_PROCS);
        let max_vmem_mb = config
            .max_vmem_mb
            .unwrap_or(crate::plugins::runner::DEFAULT_MAX_VMEM_MB);
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let sandbox = config.sandbox;
            unsafe {
                cmd.as_std_mut().pre_exec(move || {
                    if sandbox {
                        crate::plugins::runner::sandbox_pre_exec(max_procs, max_vmem_mb)
                    } else {
                        crate::plugins::runner::apply_resource_limits(max_procs, max_vmem_mb)
                    }
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (max_procs, max_vmem_mb);
            if config.sandbox {
                warn!(
                    plugin_id = %config.plugin_id,
                    "sandbox=true has no effect on this OS (Linux required for namespace isolation)"
                );
            }
            warn!(
                plugin_id = %config.plugin_id,
                "resource limits (max_procs/max_vmem_mb) unsupported on this OS"
            );
        }

        let mut child = cmd.spawn().map_err(VeyronError::Io)?;

        let pid = child
            .id()
            .ok_or_else(|| VeyronError::Internal("no pid".into()))?;
        let plugin_id = config.plugin_id.clone();

        let log_buf = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(
            self.max_log_lines,
        )));
        self.log_buffers
            .insert(plugin_id.clone(), Arc::clone(&log_buf));

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

    pub fn is_running(&self, plugin_id: &str) -> bool {
        self.entries.contains_key(plugin_id)
    }

    /// Configured SIGTERM→SIGKILL grace for a supervised plugin. `None` when the
    /// plugin is not supervised or its config uses the default (0).
    pub fn grace_seconds_for(&self, plugin_id: &str) -> Option<u32> {
        self.entries
            .get(plugin_id)
            .map(|e| e.config.grace_seconds)
            .filter(|g| *g > 0)
    }

    pub fn restart_count(&self, plugin_id: &str) -> Option<u32> {
        self.entries
            .get(plugin_id)
            .map(|e| e.restart_count)
            .or_else(|| self.stopped_counts.get(plugin_id).map(|c| *c))
    }

    /// Send SIGTERM to all managed plugins, then SIGKILL each plugin on its own
    /// deadline — `grace_seconds` from its `PluginConfig`, falling back to
    /// `default_grace_seconds` when that field is 0. A plugin with a long grace
    /// period no longer delays SIGKILL for every other plugin (BUG-005).
    pub async fn graceful_shutdown(&self, default_grace_seconds: u32) {
        if self.entries.is_empty() {
            return;
        }

        for entry in self.entries.iter() {
            let pid = nix::unistd::Pid::from_raw(entry.value().pid as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }

        let handles: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                let pid = entry.value().pid;
                let grace = entry.value().config.grace_seconds;
                let grace = if grace > 0 {
                    grace
                } else {
                    default_grace_seconds
                };
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(grace as u64)).await;
                    let pid = nix::unistd::Pid::from_raw(pid as i32);
                    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                })
            })
            .collect();

        for handle in handles {
            let _ = handle.await;
        }
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
                    // max restarts reached or Never policy — remove dead entry so
                    // is_running() returns false (VULN-018). Preserve final restart_count
                    // in stopped_counts for historical lookup.
                    let final_count = self
                        .entries
                        .remove(&event.plugin_id)
                        .map(|(_, e)| e.restart_count)
                        .unwrap_or(0);
                    self.stopped_counts.insert(event.plugin_id, final_count);
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
                // --- T-07: per-plugin resource metrics (Linux only) ---
                #[cfg(target_os = "linux")]
                if let Some((cpu, rss)) = proc_resource_usage(pid) {
                    gauge!("veyron_plugin_cpu_seconds_total", "plugin_id" => plugin_id.clone())
                        .set(cpu);
                    gauge!("veyron_plugin_memory_rss_bytes", "plugin_id" => plugin_id.clone())
                        .set(rss);
                }

                if let Some(last_pong) = registry.last_pong(&plugin_id) {
                    if last_pong.elapsed() > deadline {
                        warn!(plugin_id = %plugin_id, "watchdog: plugin unresponsive, sending SIGKILL");
                        let nix_pid = nix::unistd::Pid::from_raw(pid as i32);
                        let _ = nix::sys::signal::kill(nix_pid, nix::sys::signal::Signal::SIGKILL);
                        // Do NOT reset pong here (VULN-021): the deadline must keep
                        // running so the watchdog can escalate (another SIGKILL) if the
                        // process is stuck in D-state. If the process truly died, the
                        // exit event from monitor_loop will clean up the entry.
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
                        let _ = reg_entry.write_tx.send(out_frame(frame)).await;
                    }
                }
            }
        }
    }
}

/// Read CPU seconds (user+system) and RSS bytes for a given PID from `/proc`.
/// Returns `(cpu_seconds, rss_bytes)` or None on any read/parse failure.
#[cfg(target_os = "linux")]
fn proc_resource_usage(pid: u32) -> Option<(f64, f64)> {
    // --- CPU time from /proc/<pid>/stat ---
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<&str> = stat.split_whitespace().collect();
    // utime = field 14 (0-indexed: 13), stime = field 15 (0-indexed: 14)
    let utime: u64 = fields.get(13)?.parse().ok()?;
    let stime: u64 = fields.get(14)?.parse().ok()?;
    let clk_tck = nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .unwrap_or(100) as f64;
    let cpu_seconds = (utime + stime) as f64 / clk_tck;

    // --- RSS from /proc/<pid>/status ---
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kb: f64 = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let rss_bytes = rss_kb * 1024.0;

    Some((cpu_seconds, rss_bytes))
}

fn backoff_delay(restart_count: u32) -> Duration {
    let ms = 100u64.saturating_mul(1u64 << restart_count.min(8));
    Duration::from_millis(ms.min(30_000))
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "linux"))]
    #[tracing_test::traced_test]
    #[test]
    fn sandbox_true_non_linux_emits_warn() {
        use super::{PluginConfig, RestartPolicy};

        let config = PluginConfig {
            plugin_id: "test-plugin".to_string(),
            binary_path: std::path::PathBuf::from("/nonexistent"),
            args: vec![],
            env: vec![],
            restart_policy: RestartPolicy::Never,
            grace_seconds: 5,
            sandbox: true,
        };
        if config.sandbox {
            tracing::warn!(
                plugin_id = %config.plugin_id,
                "sandbox=true has no effect on this OS (Linux required for namespace isolation)"
            );
        }
        assert!(logs_contain(
            "sandbox=true has no effect on this OS (Linux required for namespace isolation)"
        ));
    }
}
