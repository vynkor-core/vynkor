use crate::plugins::manager::PluginManager;
use crate::plugins::supervisor::{PluginConfig, RestartPolicy};
use crate::utils::config::PluginDef;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

pub struct PluginLoader;

impl PluginLoader {
    /// Spawn all plugins declared in `config.yaml` under the `plugins:` key.
    /// Failures are logged and skipped — a bad binary path won't prevent the
    /// kernel from serving other plugins.
    pub async fn load_all(defs: &[PluginDef], manager: &Arc<PluginManager>) {
        if defs.is_empty() {
            return;
        }
        info!("loading {} plugin(s) from config", defs.len());
        for def in defs {
            info!(id = %def.id, binary = %def.binary, "spawning plugin");
            let policy = match def.restart.as_str() {
                "always" => RestartPolicy::Always,
                "never" => RestartPolicy::Never,
                _ => RestartPolicy::OnFailure,
            };
            let config = PluginConfig {
                plugin_id: def.id.clone(),
                binary_path: PathBuf::from(&def.binary),
                args: def.args.clone(),
                env: def.env.clone(),
                restart_policy: policy,
                max_restarts: def.max_restarts,
                sandbox: def.sandbox,
                grace_seconds: def.grace_seconds,
            };
            match manager.start(config).await {
                Ok(proc) => info!(id = %def.id, pid = proc.pid, "plugin spawned"),
                Err(e) => warn!(id = %def.id, error = %e, "failed to spawn plugin — skipping"),
            }
        }
    }
}
