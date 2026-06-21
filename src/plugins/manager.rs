use std::sync::Arc;

use crate::plugins::registry::PluginRegistry;
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
        Self { supervisor, registry }
    }

    pub async fn start(&self, config: PluginConfig) -> Result<PluginProcess, VeyronError> {
        self.supervisor.spawn_plugin(config).await
    }

    pub async fn stop(&self, plugin_id: &str) -> Result<(), VeyronError> {
        self.supervisor.stop_plugin(plugin_id).await
    }

    pub async fn restart(&self, plugin_id: &str) -> Result<(), VeyronError> {
        self.supervisor.restart_plugin(plugin_id).await
    }

    /// True if the plugin has a supervised OS process.
    pub fn is_supervised(&self, plugin_id: &str) -> bool {
        self.supervisor.is_running(plugin_id)
    }

    /// True if the plugin is registered on the IPC socket.
    pub fn is_connected(&self, plugin_id: &str) -> bool {
        self.registry.get(plugin_id).is_some()
    }

    pub async fn logs(&self, plugin_id: &str, n: usize) -> Vec<String> {
        self.supervisor.get_logs(plugin_id, n).await
    }
}
