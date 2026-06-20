use crate::ipc::framing::Frame;
use crate::proto::veyron::PluginManifest;
use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum PluginState {
    Registered,
}

#[derive(Clone)]
pub struct PluginEntry {
    pub plugin_id: String,
    pub conn_id: u64,
    pub manifest: PluginManifest,
    pub write_tx: mpsc::Sender<Frame>,
    pub registered_at: u64,
    pub state: PluginState,
}

pub struct PluginRegistry {
    by_plugin_id: DashMap<String, PluginEntry>,
    by_conn_id: DashMap<u64, String>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        PluginRegistry {
            by_plugin_id: DashMap::new(),
            by_conn_id: DashMap::new(),
        }
    }

    pub fn register(
        &self,
        plugin_id: String,
        conn_id: u64,
        manifest: PluginManifest,
        write_tx: mpsc::Sender<Frame>,
    ) -> Result<(), VeyronError> {
        if self.by_plugin_id.contains_key(&plugin_id) {
            return Err(VeyronError::PluginAlreadyRegistered(plugin_id));
        }

        let registered_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry = PluginEntry {
            plugin_id: plugin_id.clone(),
            conn_id,
            manifest,
            write_tx,
            registered_at,
            state: PluginState::Registered,
        };

        self.by_conn_id.insert(conn_id, plugin_id.clone());
        self.by_plugin_id.insert(plugin_id, entry);
        Ok(())
    }

    pub fn unregister(&self, plugin_id: &str) {
        if let Some((_, entry)) = self.by_plugin_id.remove(plugin_id) {
            self.by_conn_id.remove(&entry.conn_id);
        }
    }

    pub fn get(&self, plugin_id: &str) -> Option<PluginEntry> {
        self.by_plugin_id.get(plugin_id).map(|e| e.clone())
    }

    pub fn list(&self) -> Vec<PluginEntry> {
        self.by_plugin_id
            .iter()
            .map(|e| e.value().clone())
            .collect()
    }

    pub fn is_registered(&self, conn_id: u64) -> bool {
        self.by_conn_id.contains_key(&conn_id)
    }

    pub fn get_by_conn_id(&self, conn_id: u64) -> Option<PluginEntry> {
        let plugin_id = self.by_conn_id.get(&conn_id)?;
        self.by_plugin_id.get(plugin_id.value()).map(|e| e.clone())
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}
