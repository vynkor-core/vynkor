use crate::marketplace::installer::validate_manifest;
use crate::plugins::manager::PluginManager;
use crate::plugins::supervisor::{PluginConfig, RestartPolicy};
use crate::utils::config::PluginDef;
use crate::utils::errors::VeyronError;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

pub struct PluginLoader;

impl PluginLoader {
    /// Spawn all plugins declared in `config.yaml` under the `plugins:` key.
    /// Failures are logged and skipped — a bad binary path or invalid plugin.json
    /// won't prevent the kernel from serving other plugins.
    pub async fn load_all(defs: &[PluginDef], manager: &Arc<PluginManager>) {
        if defs.is_empty() {
            return;
        }
        info!("loading {} plugin(s) from config", defs.len());
        for def in defs {
            if let Err(e) = validate_plugin_def(def) {
                warn!(id = %def.id, error = %e, "refusing to load plugin — skipping");
                continue;
            }
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

/// Validate plugin.json from the plugin's binary directory before spawning.
///
/// If plugin.json is absent the plugin is allowed through (no manifest = no constraint).
/// If plugin.json is present it must pass kernel compatibility and permission checks.
pub fn validate_plugin_def(def: &PluginDef) -> Result<(), VeyronError> {
    let binary = PathBuf::from(&def.binary);
    let plugin_dir = match binary.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return Ok(()),
    };

    let manifest_path = plugin_dir.join("plugin.json");
    if !manifest_path.exists() {
        return Ok(());
    }

    let kernel_ver = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| VeyronError::Internal(format!("kernel version parse: {e}")))?;

    let manifest = validate_manifest(&manifest_path, &kernel_ver)?;

    // Cross-check manifest permissions against config-granted permissions.
    // An empty def.permissions list means the operator placed no restrictions.
    if !def.permissions.is_empty() {
        for perm in &manifest.permissions {
            if !def.permissions.contains(perm) {
                return Err(VeyronError::PermissionDenied(format!(
                    "Plugin '{}' requests permission '{}' which is not granted in config",
                    def.id, perm
                )));
            }
        }
    }

    Ok(())
}
