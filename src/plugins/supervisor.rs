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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch, Mutex};
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
    /// Isolate plugin in private user + network + PID namespaces (Linux only).
    /// PID-namespace isolation runs through a shim process (`vyn __shim`, see
    /// `plugins::shim`): the supervisor re-execs itself, the shim nests a user
    /// namespace, creates a fresh PID namespace and private `/proc`, and forks
    /// the plugin into it as PID 1. A plugin cannot be moved into a PID
    /// namespace from its own spawn path — a pending `pid_for_children`
    /// namespace makes the kernel refuse thread creation (EINVAL).
    pub sandbox: bool,
    /// Seconds to wait after SIGTERM before SIGKILL. 0 means use default (5s).
    pub grace_seconds: u32,
    /// RLIMIT_NPROC cap. None = `runner::DEFAULT_MAX_PROCS`. Applied
    /// unconditionally (not gated by `sandbox`). The check counts *all*
    /// threads of the real uid system-wide at clone time, so a cap below
    /// the session's thread baseline kills every thread the plugin spawns
    /// with EAGAIN — busy desktop sessions need an explicit, higher value.
    pub max_procs: Option<u64>,
    /// RLIMIT_AS cap in MiB. None = `runner::DEFAULT_MAX_VMEM_MB`. Applied
    /// unconditionally (not gated by `sandbox`).
    pub max_vmem_mb: Option<u64>,
    /// Landlock filesystem ceiling for sandboxed plugins (R9-03):
    /// `full` (no restriction), `read-only`, or `none`. Only enforced when
    /// `sandbox: true` on a Landlock-capable kernel.
    pub max_fs_access: crate::plugins::fsaccess::FsAccessMode,
    /// Read-only dirs/files granted to a restricted plugin (besides its own
    /// binary dir and system libs). Honored when `max_fs_access: read-only`.
    pub readonly_paths: Vec<PathBuf>,
    /// Writable dirs/files granted to a restricted plugin. Honored when
    /// `max_fs_access` is `read-only` or `none`.
    pub writable_paths: Vec<PathBuf>,
}

pub struct PluginProcess {
    pub plugin_id: String,
    pub pid: u32,
}

struct PluginEntry {
    config: PluginConfig,
    restart_count: u32,
    pid: u32,
    /// Host pid of the sandbox shim (`vyn __shim`), when the plugin runs
    /// sandboxed. Lifecycle signals go to the shim, which forwards them and
    /// stays alive to reap — signalling the plugin directly would break the
    /// shim's waitpid and orphan it inside the namespace.
    shim_pid: Option<u32>,
    /// Monotonic spawn-instance id. `ExitEvent`s carry it so a stale exit of
    /// an older spawn can never be attributed to a newer one (B1) and an
    /// explicit stop is never undone by an in-flight restart (B3).
    epoch: u64,
    /// Fired once the wait task has reaped the process tree. `stop_plugin`
    /// awaits it so "stopped" means actually exited, not just signalled (B2).
    exited: watch::Receiver<bool>,
}

impl PluginEntry {
    fn signal_target(&self) -> i32 {
        self.shim_pid.unwrap_or(self.pid) as i32
    }

    /// Await the wait task reaping the process tree. `changed()` resolves once
    /// a value newer than the initial `false` is published, even if that
    /// happened before this call — a fast exit never hangs the awaiter.
    async fn wait_for_exit(&mut self) {
        let _ = self.exited.changed().await;
    }
}

struct ExitEvent {
    plugin_id: String,
    /// Instance id of the spawn that exited. A mismatch against the registered
    /// entry identifies a stale event (B1).
    epoch: u64,
    pid: u32,
    success: bool,
}

pub struct PluginSupervisor {
    socket_path: String,
    /// Base dir for per-plugin writable state: each spawn gets
    /// `data_dir/plugins/<plugin_id>`, exposed to the plugin as
    /// `VEYRON_DATA_DIR`. `None` = no data dir granted.
    data_dir: Option<PathBuf>,
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
    /// Monotonic spawn-instance counter. Every spawn takes one id; `ExitEvent`s
    /// carry it so a stale exit can never be attributed to a newer instance (B1)
    /// and a manual stop can't be undone by an in-flight restart (B3).
    next_epoch: AtomicU64,
    /// plugin_id → epoch of its last explicit stop. An auto-restart decision
    /// still in flight for a stopped instance is dropped (B3).
    stopped_epochs: Arc<DashMap<String, u64>>,
    /// Base delay (ms) for exponential restart backoff: `base * 2^restart_count`.
    backoff_base_ms: u64,
    /// Ceiling (ms) for exponential restart backoff.
    backoff_max_ms: u64,
}

impl PluginSupervisor {
    pub fn new(socket_path: &str) -> Self {
        Self::with_log_lines(socket_path, 1000)
    }

    pub fn with_log_lines(socket_path: &str, max_log_lines: usize) -> Self {
        Self::with_events(socket_path, max_log_lines, None, None, 100, 30_000)
    }

    /// Grant every spawned plugin a writable per-plugin dir under `dir`
    /// (exposed as `VEYRON_DATA_DIR`) for its own persistent state.
    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.data_dir = Some(dir);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_events(
        socket_path: &str,
        max_log_lines: usize,
        event_bus: Option<Arc<EventBus>>,
        plugin_registry: Option<Arc<PluginRegistry>>,
        backoff_base_ms: u64,
        backoff_max_ms: u64,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel::<ExitEvent>(64);
        PluginSupervisor {
            socket_path: socket_path.to_string(),
            data_dir: None,
            backoff_base_ms,
            backoff_max_ms,
            entries: Arc::new(DashMap::new()),
            event_tx,
            event_rx: Arc::new(Mutex::new(event_rx)),
            log_buffers: Arc::new(DashMap::new()),
            max_log_lines,
            event_bus,
            plugin_registry,
            forced_restarts: Arc::new(DashMap::new()),
            stopped_counts: Arc::new(DashMap::new()),
            next_epoch: AtomicU64::new(0),
            stopped_epochs: Arc::new(DashMap::new()),
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
        self.spawn_internal(config, 0, None).await
    }

    async fn spawn_internal(
        &self,
        config: PluginConfig,
        restart_count: u32,
        replace_epoch: Option<u64>,
    ) -> Result<PluginProcess, VeyronError> {
        // B3: a manual start must never clobber a live entry. A supervised
        // restart carries a token (Some) — it replaces its own dead entry;
        // a route-level start has none and refuses while one is registered.
        if replace_epoch.is_none() && self.entries.contains_key(&config.plugin_id) {
            return Err(VeyronError::PluginAlreadyRunning(config.plugin_id.clone()));
        }

        let epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed) + 1;

        #[cfg(target_os = "linux")]
        let use_shim = config.sandbox;
        #[cfg(not(target_os = "linux"))]
        let use_shim = {
            if config.sandbox {
                warn!(
                    plugin_id = %config.plugin_id,
                    "sandbox requested, but pid-namespace isolation is linux-only — running unsandboxed",
                );
            }
            false
        };
        // sandboxed plugins run under a shim that places them in a private
        // PID namespace (R9-02, see plugins::shim) — the shim is our own
        // binary re-exec'd with the hidden __shim subcommand
        // Grant the plugin a writable data dir (VEYRON_DATA_DIR) for its own
        // persistent state. Created up front: a sandboxed plugin cannot mkdir
        // paths it can't see, and Landlock needs the dir in RW_PATHS.
        let mut writable_paths = config.writable_paths.clone();
        let mut plugin_data_dir: Option<PathBuf> = None;
        if let Some(data_dir) = &self.data_dir {
            let plugin_data = data_dir.join("plugins").join(&config.plugin_id);
            match std::fs::create_dir_all(&plugin_data) {
                Ok(()) => {
                    plugin_data_dir = Some(plugin_data.clone());
                    writable_paths.push(plugin_data);
                }
                Err(e) => {
                    warn!(plugin_id = %config.plugin_id, error = %e, "failed to create plugin data dir");
                }
            }
        }

        let mut cmd = if use_shim {
            let mut c = Command::new(sandbox_shim_bin());
            c.arg("__shim").arg(&config.binary_path);
            // the shim mirrors the supervisor's SIGTERM→SIGKILL grace: a
            // handler-less plugin (PID 1 of its namespace) drops SIGTERM, so
            // the shim escalates on this deadline instead of blocking waitpid
            if config.grace_seconds > 0 {
                c.env("VEYRON_SHIM_GRACE_SECS", config.grace_seconds.to_string());
            }
            // R9-03: pass the Landlock filesystem restriction down to the
            // shim, which applies it in the plugin's pre_exec (fail-closed).
            // `full` sends no vars — the shim then builds no ruleset.
            if config.max_fs_access != crate::plugins::fsaccess::FsAccessMode::Full {
                use crate::plugins::fsaccess;
                c.env("VEYRON_MAX_FS_ACCESS", config.max_fs_access.as_str())
                    .env(
                        "VEYRON_RO_PATHS",
                        fsaccess::join_paths_env(&config.readonly_paths),
                    )
                    .env(
                        "VEYRON_RW_PATHS",
                        fsaccess::join_paths_env(&writable_paths),
                    );
            }
            c
        } else {
            if config.max_fs_access != crate::plugins::fsaccess::FsAccessMode::Full {
                warn!(
                    plugin_id = %config.plugin_id,
                    "max_fs_access is only enforced for sandboxed plugins (sandbox: true) — running without filesystem restriction",
                );
            }
            Command::new(&config.binary_path)
        };
        cmd.args(&config.args)
            .env("VEYRON_SOCKET_PATH", &self.socket_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &plugin_data_dir {
            cmd.env("VEYRON_DATA_DIR", dir);
        }
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
        // R9-01: per-plugin process accounting via cgroup v2 `pids.max`. The
        // scope is prepared here (parent side) so a failure degrades to the
        // RLIMIT_NPROC fallback without killing the spawn; the child only
        // moves itself into the prepared scope in `pre_exec`.
        #[cfg(target_os = "linux")]
        let cgroup_path = crate::plugins::runner::prepare_pids_cgroup(&config.plugin_id, max_procs);
        #[cfg(not(target_os = "linux"))]
        let cgroup_path: Option<PathBuf> = None;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let sandbox = config.sandbox;
            let cgroup_for_pre_exec = cgroup_path.clone();
            unsafe {
                cmd.as_std_mut().pre_exec(move || {
                    if sandbox {
                        crate::plugins::runner::sandbox_pre_exec(
                            max_procs,
                            max_vmem_mb,
                            cgroup_for_pre_exec.as_deref(),
                        )
                    } else {
                        crate::plugins::runner::apply_resource_limits(
                            max_procs,
                            max_vmem_mb,
                            cgroup_for_pre_exec.as_deref(),
                        )
                    }
                });
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (max_procs, max_vmem_mb, &cgroup_path);
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

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                // spawn can fail before the child ever forks (EAGAIN on
                // fork, EMFILE on the stdout/stderr pipes) — the prepared
                // pids scope would leak forever because the watcher task
                // that normally cleans it up is never spawned.
                #[cfg(target_os = "linux")]
                if let Some(cg) = &cgroup_path {
                    crate::plugins::runner::cleanup_pids_cgroup(cg);
                }
                return Err(VeyronError::Io(e));
            }
        };

        let plugin_id = config.plugin_id.clone();

        let log_buf = Arc::new(Mutex::new(VecDeque::<String>::with_capacity(
            self.max_log_lines,
        )));
        self.log_buffers
            .insert(plugin_id.clone(), Arc::clone(&log_buf));
        let max_lines = self.max_log_lines;

        // With the shim, stdout is the pid channel: the first line is the
        // plugin's host pid, printed only after the plugin signalled
        // readiness. EOF or a timeout means the sandbox failed to come up —
        // fail the spawn so a plugin never runs unisolated.
        let (pid, shim_pid) = if use_shim {
            let mut lines = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
            let line = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
                .await
                .map_err(|_| {
                    VeyronError::Internal(
                        "sandbox shim did not report a plugin pid within 15s".into(),
                    )
                })?
                .map_err(VeyronError::Io)?;
            let plugin_pid = match line.as_deref().and_then(|l| l.trim().parse::<u32>().ok()) {
                Some(p) if p > 0 => p,
                _ => {
                    // the shim died before the plugin entered the sandbox
                    // (e.g. unprivileged user namespaces disabled) — kill the
                    // shim and clean up its pids scope
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    #[cfg(target_os = "linux")]
                    if let Some(cg) = &cgroup_path {
                        crate::plugins::runner::cleanup_pids_cgroup(cg);
                    }
                    return Err(VeyronError::Internal(format!(
                        "sandbox shim exited before the plugin started (line: {line:?})"
                    )));
                }
            };
            let shim_pid = child
                .id()
                .ok_or_else(|| VeyronError::Internal("no shim pid".into()))?;
            // leftover shim stdout (should be empty) drains like a normal stream
            let buf = Arc::clone(&log_buf);
            tokio::spawn(async move {
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut locked = buf.lock().await;
                    if locked.len() >= max_lines {
                        locked.pop_front();
                    }
                    locked.push_back(line);
                }
            });
            (plugin_pid, Some(shim_pid))
        } else {
            let pid = child
                .id()
                .ok_or_else(|| VeyronError::Internal("no pid".into()))?;
            if let Some(stdout) = child.stdout.take() {
                drain_to_log(stdout, Arc::clone(&log_buf), max_lines);
            }
            (pid, None)
        };

        if let Some(stderr) = child.stderr.take() {
            drain_to_log(stderr, Arc::clone(&log_buf), max_lines);
        }

        // Verify the child landed in its pids cgroup. The join happens in
        // `pre_exec` before exec, so `/proc/<pid>/cgroup` is authoritative by
        // the time spawn() returns; a mismatch means the RLIMIT_NPROC
        // fallback is in effect and the operator should know why.
        #[cfg(target_os = "linux")]
        if let Some(cg) = &cgroup_path {
            let expected = cg
                .strip_prefix("/sys/fs/cgroup")
                .unwrap_or(cg)
                .to_string_lossy()
                .into_owned();
            match std::fs::read_to_string(format!("/proc/{pid}/cgroup")) {
                Ok(contents) if contents.trim_end().ends_with(&expected) => {
                    info!(
                        plugin_id = %plugin_id,
                        cgroup = %expected,
                        "plugin joined per-plugin pids cgroup"
                    );
                }
                Ok(contents) => {
                    warn!(
                        plugin_id = %plugin_id,
                        cgroup = %expected,
                        actual = %contents.trim(),
                        "plugin did not join its pids cgroup — RLIMIT_NPROC fallback in effect"
                    );
                }
                Err(_) => {
                    // child already gone (e.g. an instantly-exiting test
                    // binary) — nothing to verify; the watcher cleans up
                }
            }
        }

        info!(plugin_id = %plugin_id, pid = pid, restart_count = restart_count, "plugin spawned");
        let (exited_tx, exited_rx) = watch::channel(false);
        self.entries.insert(
            plugin_id.clone(),
            PluginEntry {
                config,
                restart_count,
                pid,
                shim_pid,
                epoch,
                exited: exited_rx,
            },
        );

        let tx = self.event_tx.clone();
        let id = plugin_id.clone();
        #[cfg(target_os = "linux")]
        let cgroup_for_cleanup = cgroup_path.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // orphan-gap sweep before cleanup: a shim killed outright
            // (watchdog SIGKILL, SIGKILL deadline) never reaps the plugin, so
            // its death signal may never have been delivered — a still-living
            // plugin would keep the scope populated and the rmdir below fail
            if shim_pid.is_some() {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                if kill(Pid::from_raw(pid as i32), None).is_ok() {
                    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
                }
            }
            #[cfg(target_os = "linux")]
            if let Some(cg) = cgroup_for_cleanup {
                // a just-SIGKILLed zombie takes a beat to leave the cgroup;
                // retry briefly instead of leaking the scope dir. a fresh
                // spawn of the same plugin id reuses the scope either way
                for _ in 0..10 {
                    crate::plugins::runner::cleanup_pids_cgroup(&cg);
                    if !cg.exists() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            let success = status.map(|s| s.success()).unwrap_or(false);
            // B2: the tree is reaped and the scope is gone — publish the exit
            // so a stop awaiting `exited` resolves (a fast exit must not hang
            // the awaiter: changed() resolves on the already-published value).
            let _ = exited_tx.send(true);
            let _ = tx
                .send(ExitEvent {
                    plugin_id: id,
                    epoch,
                    pid,
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

        // B3: an explicit stop is terminal — record the instance so an
        // in-flight backoff restart can't resurrect the plugin.
        self.stopped_epochs
            .insert(plugin_id.to_string(), entry.1.epoch);
        // Explicit stop overrides any pending manual restart.
        self.forced_restarts.remove(plugin_id);
        let target = entry.1.signal_target();
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(target),
            nix::sys::signal::Signal::SIGTERM,
        );
        // B2: "stopped" means the process tree actually exited, not just
        // signalled. Escalate to SIGKILL on the configured deadline so a
        // SIGTERM-ignoring plugin can't hold the stop; the final bound keeps
        // even an unkillable process from hanging the caller forever.
        let mut entry = entry.1;
        let grace = if entry.config.grace_seconds > 0 {
            entry.config.grace_seconds
        } else {
            5
        };
        if tokio::time::timeout(Duration::from_secs(grace as u64), entry.wait_for_exit())
            .await
            .is_err()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(target),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = tokio::time::timeout(Duration::from_secs(10), entry.wait_for_exit()).await;
        }
        Ok(())
    }

    // Sends SIGTERM without removing the entry so monitor_loop restarts the plugin.
    // Marks the plugin for forced restart so it respawns even under a Never /
    // OnFailure policy or after max_restarts — a manual restart overrides policy.
    pub async fn restart_plugin(&self, plugin_id: &str) -> Result<(), VeyronError> {
        let target = self
            .entries
            .get(plugin_id)
            .map(|e| e.signal_target())
            .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

        self.forced_restarts.insert(plugin_id.to_string(), ());
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(target),
            nix::sys::signal::Signal::SIGTERM,
        );
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
            let pid = nix::unistd::Pid::from_raw(entry.value().signal_target());
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
        }

        let handles: Vec<_> = self
            .entries
            .iter()
            .map(|entry| {
                let target = entry.value().signal_target();
                let grace = entry.value().config.grace_seconds;
                let grace = if grace > 0 {
                    grace
                } else {
                    default_grace_seconds
                };
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(grace as u64)).await;
                    let _ = nix::sys::signal::kill(
                        nix::unistd::Pid::from_raw(target),
                        nix::sys::signal::Signal::SIGKILL,
                    );
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

            // B1: an exit is only actionable for the currently registered
            // instance — a stale event from an older spawn must not be
            // attributed to a newer one. B3: an exit after an explicit stop
            // must not restart anything.
            let is_current = self
                .entries
                .get(&event.plugin_id)
                .map(|e| e.epoch == event.epoch)
                .unwrap_or(false);
            let was_stopped = self
                .stopped_epochs
                .get(&event.plugin_id)
                .map(|s| *s == event.epoch)
                .unwrap_or(false);
            if !is_current || was_stopped {
                info!(
                    plugin_id = %event.plugin_id,
                    epoch = event.epoch,
                    is_current,
                    was_stopped,
                    "ignoring stale or stopped exit event"
                );
                continue;
            }

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
                pid = event.pid,
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
                    tokio::time::sleep(self.backoff_delay(new_count)).await;
                    // B3: an explicit stop during the backoff window cancels
                    // the restart — stop is terminal.
                    let stopped_during_backoff = self
                        .stopped_epochs
                        .get(&config.plugin_id)
                        .map(|s| *s == event.epoch)
                        .unwrap_or(false);
                    if stopped_during_backoff {
                        info!(
                            plugin_id = %config.plugin_id,
                            epoch = event.epoch,
                            "restart cancelled — plugin stopped during backoff"
                        );
                        continue;
                    }
                    let _ = self
                        .spawn_internal(config, new_count, Some(event.epoch))
                        .await;
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

            let supervised: Vec<(String, u32, Option<u32>)> = self
                .entries
                .iter()
                .map(|e| (e.key().clone(), e.value().pid, e.value().shim_pid))
                .collect();

            for (plugin_id, pid, shim_pid) in supervised {
                // --- T-07: per-plugin resource metrics (Linux only) ---
                // reads the plugin pid, not the shim's
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
                        let target = shim_pid.unwrap_or(pid) as i32;
                        let _ = nix::sys::signal::kill(
                            nix::unistd::Pid::from_raw(target),
                            nix::sys::signal::Signal::SIGKILL,
                        );
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
                            payload: payload.into(),
                            mac: None,
                        };
                        let _ = reg_entry.write_tx.send(out_frame(frame)).await;
                    }
                }
            }
        }
    }

    fn backoff_delay(&self, restart_count: u32) -> Duration {
        let ms = self
            .backoff_base_ms
            .saturating_mul(1u64 << restart_count.min(8));
        Duration::from_millis(ms.min(self.backoff_max_ms))
    }
}

/// Binary the supervisor re-execs as the sandbox shim: our own executable
/// (the hidden `__shim` subcommand), overridable via VEYRON_SHIM_BIN — the
/// unit-test harness binary does not handle `__shim`.
fn sandbox_shim_bin() -> PathBuf {
    std::env::var_os("VEYRON_SHIM_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vyn")))
}

/// Drain a child stream into the plugin's ring log buffer.
fn drain_to_log<S>(stream: S, buf: Arc<Mutex<VecDeque<String>>>, max_lines: usize)
where
    S: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let mut locked = buf.lock().await;
            if locked.len() >= max_lines {
                locked.pop_front();
            }
            locked.push_back(line);
        }
    });
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
