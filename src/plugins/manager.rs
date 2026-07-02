use std::sync::Arc;

use crate::plugins::registry::{PluginEntry, PluginRegistry};
use crate::plugins::supervisor::{PluginConfig, PluginProcess, PluginSupervisor};
use crate::utils::errors::VeyronError;

/// High-level plugin lifecycle API: combines supervisor (process control)
/// and registry (connection state) into one access point.
pub struct PluginManager {
    supervisor: Arc<PluginSupervisor>,
    registry: Arc<PluginRegistry>,
}

impl PluginManager {
    pub fn new(supervisor: Arc<PluginSupervisor>, registry: Arc<PluginRegistry>) -> Self {
        Self {
            supervisor,
            registry,
        }
    }

    pub fn list(&self) -> Vec<PluginEntry> {
        self.registry.list()
    }

    pub fn get(&self, plugin_id: &str) -> Option<PluginEntry> {
        self.registry.get(plugin_id)
    }

    pub async fn start(&self, config: PluginConfig) -> Result<PluginProcess, VeyronError> {
        self.supervisor.spawn_plugin(config).await
    }

    /// Stops the supervised OS process (best-effort) and removes the IPC registration.
    pub async fn stop(&self, plugin_id: &str) -> Result<(), VeyronError> {
        let _ = self.supervisor.stop_plugin(plugin_id).await;
        self.registry.unregister(plugin_id);
        Ok(())
    }

    pub async fn restart(&self, plugin_id: &str) -> Result<(), VeyronError> {
        self.supervisor.restart_plugin(plugin_id).await
    }

    pub fn is_supervised(&self, plugin_id: &str) -> bool {
        self.supervisor.is_running(plugin_id)
    }

    pub fn is_connected(&self, plugin_id: &str) -> bool {
        self.registry.get(plugin_id).is_some()
    }

    pub async fn logs(&self, plugin_id: &str, n: usize) -> Vec<String> {
        self.supervisor.get_logs(plugin_id, n).await
    }
}
