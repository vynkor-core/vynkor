use crate::auth::permissions::normalize_permission;
use crate::events::bus::EventBus;
use crate::plugins::manager::PluginManager;
use crate::plugins::supervisor::{PluginConfig, RestartPolicy};
use crate::utils::config::PluginDef;
use crate::utils::errors::VeyronError;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};
// manifest types come from veyron-wire — the single implementation shared
// with vynm since V-01/V-02.
use veyron_wire::manifest::{validate_manifest, InstallManifest};

pub struct PluginLoader;

impl PluginLoader {
    /// Build the supervisor's `PluginConfig` from a config.yaml plugin entry.
    /// Shared by the boot-time loader and the HTTP `POST /plugins/:id/start` route.
    pub fn config_from_def(def: &PluginDef) -> PluginConfig {
        let policy = match def.restart.as_str() {
            "always" => RestartPolicy::Always,
            "never" => RestartPolicy::Never,
            _ => RestartPolicy::OnFailure,
        };
        PluginConfig {
            plugin_id: def.id.clone(),
            binary_path: PathBuf::from(&def.binary),
            args: def.args.clone(),
            env: def.env.clone(),
            restart_policy: policy,
            max_restarts: def.max_restarts,
            sandbox: def.sandbox,
            grace_seconds: def.grace_seconds,
            max_procs: def.max_procs,
            max_vmem_mb: def.max_vmem_mb,
            max_fs_access: def
                .max_fs_access
                .as_deref()
                .and_then(crate::plugins::fsaccess::FsAccessMode::parse)
                .unwrap_or_else(|| {
                    if let Some(value) = def.max_fs_access.as_deref() {
                        if !value.is_empty() && value != "full" {
                            warn!(id = %def.id, value, "unknown max_fs_access — falling back to full (no filesystem restriction)");
                        }
                    }
                    crate::plugins::fsaccess::FsAccessMode::Full
                }),
            readonly_paths: def.readonly_paths.clone(),
            writable_paths: def.writable_paths.clone(),
        }
    }

    /// Spawn all plugins declared in `config.yaml` under the `plugins:` key.
    /// Respects `requires` declarations in `plugin.json`: deps are loaded first.
    /// Circular dependencies are refused. Missing deps (not in config) refuse the
    /// dependant. If `event_bus` is provided, plugins subscribe to declared events.
    pub async fn load_all(
        defs: &[PluginDef],
        manager: &Arc<PluginManager>,
        event_bus: Option<&Arc<EventBus>>,
    ) {
        if defs.is_empty() {
            return;
        }
        info!("loading {} plugin(s) from config", defs.len());

        // Validate manifests up front and collect dependency info.
        let mut manifests: HashMap<&str, Option<InstallManifest>> = HashMap::new();
        let mut refused: HashSet<&str> = HashSet::new();

        for def in defs {
            match validate_plugin_def(def) {
                Ok(m) => {
                    if let Some(manifest) = &m {
                        let mut requirements = HashMap::new();
                        if let Some(actions) = &manifest.actions {
                            for spec in actions {
                                if let Some(perm) = spec.permission() {
                                    if let Some(pt) =
                                        crate::auth::permissions::resolve_permission(perm)
                                    {
                                        requirements.insert(spec.name().to_string(), pt);
                                    }
                                }
                            }
                        }
                        if !requirements.is_empty() {
                            manager
                                .registry()
                                .set_action_requirements(def.id.clone(), requirements);
                        }
                    }
                    manifests.insert(&def.id, m);
                }
                Err(e) => {
                    warn!(id = %def.id, error = %e, "refusing to load plugin — skipping");
                    refused.insert(&def.id);
                }
            }
        }

        // Build id→index map for config-declared plugins.
        let id_set: HashSet<&str> = defs.iter().map(|d| d.id.as_str()).collect();

        // Resolve `requires` for each validated plugin: missing deps refuse the plugin.
        let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
        for def in defs {
            if refused.contains(def.id.as_str()) {
                continue;
            }
            let req_list: Vec<&str> = manifests
                .get(def.id.as_str())
                .and_then(|m| m.as_ref())
                .map(|m| m.requires.iter().map(String::as_str).collect())
                .unwrap_or_default();

            for dep in &req_list {
                if !id_set.contains(dep) {
                    warn!(
                        id = %def.id,
                        dep,
                        "refusing plugin — required dep is not in config"
                    );
                    refused.insert(&def.id);
                    break;
                }
            }
            if !refused.contains(def.id.as_str()) {
                deps.insert(&def.id, req_list);
            }
        }

        // Detect cycles via DFS and build topological order.
        let order = match topo_sort(defs, &deps, &refused) {
            Ok(o) => o,
            Err(cycle) => {
                for id in &cycle {
                    warn!(id, "refusing plugin — circular dependency detected");
                    refused.insert(id);
                }
                // Fall back to the non-cycle nodes in config order.
                defs.iter()
                    .map(|d| d.id.as_str())
                    .filter(|id| !refused.contains(id))
                    .collect()
            }
        };

        // Spawn in dependency order.
        for id in order {
            let def = match defs.iter().find(|d| d.id == id) {
                Some(d) => d,
                None => continue,
            };
            if refused.contains(id) {
                continue;
            }

            info!(id = %def.id, binary = %def.binary, "spawning plugin");
            let config = Self::config_from_def(def);
            match manager.start(config).await {
                Ok(proc) => {
                    info!(id = %def.id, pid = proc.pid, "plugin spawned");
                    if let Some(Some(manifest)) = manifests.get(def.id.as_str()) {
                        if let Some(actions) = &manifest.actions {
                            if !actions.is_empty() {
                                info!(id = %def.id, ?actions, "plugin declares handled actions");
                            }
                        }
                        if let Some(bus) = event_bus {
                            let events: Vec<String> =
                                manifest.events.as_deref().unwrap_or(&[]).to_vec();
                            if !events.is_empty() {
                                bus.subscribe(&def.id, events);
                                info!(id = %def.id, "subscribed to manifest-declared events");
                            }
                        }
                    }
                }
                Err(e) => warn!(id = %def.id, error = %e, "failed to spawn plugin — skipping"),
            }
        }
    }
}

/// Kahn's algorithm topological sort.
/// Returns ordered list of IDs (deps first) or Err(cycle_ids) on cycle detection.
pub fn topo_sort<'a>(
    defs: &'a [PluginDef],
    deps: &HashMap<&'a str, Vec<&'a str>>,
    refused: &HashSet<&str>,
) -> Result<Vec<&'a str>, Vec<&'a str>> {
    let active: Vec<&str> = defs
        .iter()
        .map(|d| d.id.as_str())
        .filter(|id| !refused.contains(id))
        .collect();

    // in-degree count for each active node.
    let mut in_degree: HashMap<&str, usize> = active.iter().map(|id| (*id, 0)).collect();
    for id in &active {
        if let Some(req) = deps.get(id) {
            for dep in req {
                if !refused.contains(dep) {
                    // dep → id edge: id's in-degree goes up
                    *in_degree.entry(id).or_default() += 1;
                }
            }
        }
    }

    // Reverse adjacency: dep → list of nodes that depend on dep.
    let mut rev: HashMap<&str, Vec<&str>> = HashMap::new();
    for id in &active {
        if let Some(req) = deps.get(id) {
            for dep in req {
                if !refused.contains(dep) {
                    rev.entry(dep).or_default().push(id);
                }
            }
        }
    }

    let mut queue: VecDeque<&str> = active
        .iter()
        .filter(|id| in_degree.get(*id).copied().unwrap_or(0) == 0)
        .copied()
        .collect();
    let mut order = Vec::new();

    while let Some(id) = queue.pop_front() {
        order.push(id);
        if let Some(dependants) = rev.get(id) {
            for dep in dependants {
                let cnt = in_degree.entry(dep).or_default();
                if *cnt > 0 {
                    *cnt -= 1;
                }
                if *cnt == 0 {
                    queue.push_back(dep);
                }
            }
        }
    }

    if order.len() < active.len() {
        // Cycle: nodes not in `order` are involved.
        let in_order: HashSet<&&str> = order.iter().collect();
        let cycle: Vec<&str> = active
            .iter()
            .filter(|id| !in_order.contains(id))
            .copied()
            .collect();
        Err(cycle)
    } else {
        Ok(order)
    }
}

/// Validate plugin.json from the plugin's binary directory before spawning.
/// Returns the parsed manifest if present, or None if no plugin.json exists.
/// If plugin.json is present it must pass kernel compatibility and permission checks.
pub fn validate_plugin_def(def: &PluginDef) -> Result<Option<InstallManifest>, VeyronError> {
    let binary = PathBuf::from(&def.binary);
    let plugin_dir = match binary.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => return Ok(None),
    };

    let manifest_path = plugin_dir.join("plugin.json");
    if !manifest_path.exists() {
        return Ok(None);
    }

    let kernel_ver = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| VeyronError::Internal(format!("kernel version parse: {e}")))?;

    let manifest = validate_manifest(
        &manifest_path,
        Some(&kernel_ver),
        crate::auth::permissions::resolve_permission,
    )?;

    // Cross-check manifest permissions against config-granted permissions.
    // An empty def.permissions list means the operator placed no restrictions.
    // Normalize both sides (N2): config.yaml may list the lowercase form while
    // plugin.json declares the PERMISSION_* proto name.
    if !def.permissions.is_empty() {
        for perm in &manifest.permissions {
            if !def
                .permissions
                .iter()
                .any(|g| normalize_permission(g) == normalize_permission(perm))
            {
                return Err(VeyronError::PermissionDenied(format!(
                    "Plugin '{}' requests permission '{}' which is not granted in config",
                    def.id, perm
                )));
            }
        }
    }

    Ok(Some(manifest))
}
