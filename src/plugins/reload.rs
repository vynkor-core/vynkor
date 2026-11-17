//! plugin set reload: re-read config.yaml (plugins.d included) and bring the
//! running set in line — start what is new, stop what is gone, restart what
//! changed. Lifecycle only; triggered by SIGHUP or an embedder's notify.

use crate::events::bus::EventBus;
use crate::plugins::lazy::ManagerStarter;
use crate::plugins::loader::PluginLoader;
use crate::plugins::manager::PluginManager;
use crate::utils::config::PluginDef;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use tokio::sync::Notify;
use tracing::{info, warn};

/// the plugin defs the kernel runs with; the api's start route and the
/// router's permission clamp read them, a reload swaps them
#[derive(Clone, Default, Debug)]
pub struct SharedDefs(Arc<RwLock<Vec<PluginDef>>>);

impl From<Vec<PluginDef>> for SharedDefs {
    fn from(defs: Vec<PluginDef>) -> Self {
        Self(Arc::new(RwLock::new(defs)))
    }
}

impl SharedDefs {
    pub fn snapshot(&self) -> Vec<PluginDef> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn get(&self, id: &str) -> Option<PluginDef> {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .find(|d| d.id == id)
            .cloned()
    }

    /// the operator's permission allowlist for `id`; None = not declared
    pub fn permissions_of(&self, id: &str) -> Option<Vec<String>> {
        self.get(id).map(|d| d.permissions)
    }

    pub fn replace(&self, defs: Vec<PluginDef>) {
        *self.0.write().unwrap_or_else(|p| p.into_inner()) = defs;
    }
}

/// what a reload changed, by plugin id
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PluginsDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl PluginsDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// ids added, removed and changed between two plugin lists; each sorted
pub fn diff(old: &[PluginDef], new: &[PluginDef]) -> PluginsDiff {
    let old_by: HashMap<&str, &PluginDef> = old.iter().map(|d| (d.id.as_str(), d)).collect();
    let new_by: HashMap<&str, &PluginDef> = new.iter().map(|d| (d.id.as_str(), d)).collect();
    let mut out = PluginsDiff::default();
    for (id, def) in &new_by {
        match old_by.get(id) {
            None => out.added.push(id.to_string()),
            Some(prev) if prev != def => out.changed.push(id.to_string()),
            Some(_) => {}
        }
    }
    for id in old_by.keys() {
        if !new_by.contains_key(id) {
            out.removed.push(id.to_string());
        }
    }
    out.added.sort();
    out.removed.sort();
    out.changed.sort();
    out
}

pub struct Reloader {
    manager: Arc<PluginManager>,
    event_bus: Arc<EventBus>,
    defs: SharedDefs,
    lazy: Arc<ManagerStarter>,
    lazy_idle_stop_secs: u64,
    config_file: Option<String>,
    // one reload at a time: two would race on the same ids
    running: tokio::sync::Mutex<()>,
}

impl Reloader {
    pub fn new(
        manager: Arc<PluginManager>,
        event_bus: Arc<EventBus>,
        defs: SharedDefs,
        lazy: Arc<ManagerStarter>,
        lazy_idle_stop_secs: u64,
        config_file: Option<String>,
    ) -> Self {
        Self {
            manager,
            event_bus,
            defs,
            lazy,
            lazy_idle_stop_secs,
            config_file,
            running: tokio::sync::Mutex::new(()),
        }
    }

    /// read the config file again and apply its plugin list
    pub async fn reload(&self) -> anyhow::Result<PluginsDiff> {
        let path = self
            .config_file
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("no config file to reload plugins from"))?;
        let cfg = crate::utils::config::load_config(path)?;
        Ok(self.apply(cfg.plugins).await)
    }

    /// stop removed and changed plugins, then start added and changed ones in
    /// dependency order (deps already running count as met)
    pub async fn apply(&self, new: Vec<PluginDef>) -> PluginsDiff {
        let _one = self.running.lock().await;
        let d = diff(&self.defs.snapshot(), &new);
        if d.is_empty() {
            return d;
        }
        for id in d.removed.iter().chain(&d.changed) {
            self.stop(id).await;
        }
        self.defs.replace(new.clone());
        let start: HashSet<String> = d.added.iter().chain(&d.changed).cloned().collect();
        if !start.is_empty() {
            let lazy = PluginLoader::load_some(
                &new,
                Some(&start),
                &self.manager,
                Some(&self.event_bus),
                self.lazy_idle_stop_secs,
            )
            .await;
            for (id, config) in lazy {
                self.lazy.insert(id, config);
            }
        }
        info!(added = ?d.added, removed = ?d.removed, changed = ?d.changed, "plugins reloaded");
        d
    }

    async fn stop(&self, id: &str) {
        self.lazy.remove(id);
        self.manager.registry().remove_dormant(id);
        if self.manager.is_supervised(id) || self.manager.is_connected(id) {
            if let Err(e) = self.manager.stop(id).await {
                warn!(id, error = %e, "stopping plugin on reload");
            }
        }
        self.event_bus.unsubscribe_all(id);
    }

    /// reload on every notify until the task is aborted
    pub async fn run(self: Arc<Self>, trigger: Arc<Notify>) {
        loop {
            trigger.notified().await;
            if let Err(e) = self.reload().await {
                warn!("plugin reload failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(id: &str, binary: &str) -> PluginDef {
        PluginDef {
            id: id.into(),
            binary: binary.into(),
            ..Default::default()
        }
    }

    #[test]
    fn diff_names_added_removed_and_changed() {
        let old = vec![def("a", "/a"), def("b", "/b"), def("c", "/c")];
        let new = vec![def("a", "/a"), def("b", "/b2"), def("d", "/d")];
        let d = diff(&old, &new);
        assert_eq!(d.added, ["d"]);
        assert_eq!(d.removed, ["c"]);
        assert_eq!(d.changed, ["b"]);
        assert!(diff(&new, &new).is_empty());
    }

    #[test]
    fn shared_defs_swap_is_seen_by_every_clone() {
        let defs = SharedDefs::from(vec![def("a", "/a")]);
        let view = defs.clone();
        assert!(view.get("b").is_none());
        let mut b = def("b", "/b");
        b.permissions = vec!["PERMISSION_STORAGE".into()];
        defs.replace(vec![b]);
        assert!(view.get("a").is_none());
        assert_eq!(
            view.permissions_of("b").unwrap(),
            ["PERMISSION_STORAGE".to_string()]
        );
    }
}
