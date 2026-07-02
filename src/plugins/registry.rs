use crate::ipc::connection::Outbound;
use crate::proto::veyron::PluginManifest;
use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
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
    pub write_tx: mpsc::Sender<Outbound>,
    pub registered_at: u64,
    pub state: PluginState,
}

pub struct PluginRegistry {
    by_plugin_id: DashMap<String, PluginEntry>,
    by_conn_id: DashMap<u64, String>,
    pong_times: DashMap<String, Instant>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        PluginRegistry {
            by_plugin_id: DashMap::new(),
            by_conn_id: DashMap::new(),
            pong_times: DashMap::new(),
        }
    }

    pub fn register(
        &self,
        plugin_id: String,
        conn_id: u64,
        manifest: PluginManifest,
        write_tx: mpsc::Sender<Outbound>,
    ) -> Result<(), VeyronError> {
        use dashmap::mapref::entry::Entry;

        validate_plugin_id(&plugin_id)?;

        // AUDIT M-08: reserve both slots via `entry()` — which holds the
        // shard lock for the call — rather than a separate contains_key then
        // insert. The prior check-then-insert was only TOCTOU-safe because
        // the router happens to call register() from a single task; entry()
        // makes that true regardless of caller concurrency.
        //
        // One registration per connection. Without this, a connection that
        // sends a second PluginRegister with a different id would overwrite
        // its by_conn_id mapping and orphan the first entry — it would leak
        // in by_plugin_id forever (disconnect only cleans the id the conn
        // maps to).
        let conn_slot = match self.by_conn_id.entry(conn_id) {
            Entry::Occupied(_) => {
                return Err(VeyronError::PluginAlreadyRegistered(format!(
                    "connection {conn_id} already has a registered plugin"
                )))
            }
            Entry::Vacant(v) => v,
        };

        let plugin_slot = match self.by_plugin_id.entry(plugin_id.clone()) {
            Entry::Occupied(_) => return Err(VeyronError::PluginAlreadyRegistered(plugin_id)),
            Entry::Vacant(v) => v,
        };

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

        conn_slot.insert(plugin_id.clone());
        self.pong_times.insert(plugin_id, Instant::now());
        plugin_slot.insert(entry);
        Ok(())
    }

    pub fn unregister(&self, plugin_id: &str) {
        if let Some((_, entry)) = self.by_plugin_id.remove(plugin_id) {
            self.by_conn_id.remove(&entry.conn_id);
            self.pong_times.remove(plugin_id);
        }
    }

    pub fn record_pong(&self, plugin_id: &str) {
        self.pong_times
            .insert(plugin_id.to_string(), Instant::now());
    }

    pub fn last_pong(&self, plugin_id: &str) -> Option<Instant> {
        self.pong_times.get(plugin_id).map(|t| *t)
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

/// Validate an incoming plugin id. Rejecting bad ids at registration prevents:
/// JSON injection (ids are embedded into event payloads), routing confusion
/// (reserved "kernel"/"*" targets), and silent truncation (ids must fit the
/// 32-byte frame target field).
pub fn validate_plugin_id(id: &str) -> Result<(), VeyronError> {
    const MAX_LEN: usize = 32; // frame target field width

    if id.is_empty() {
        return Err(VeyronError::InvalidPluginId("must not be empty".into()));
    }
    if id.len() > MAX_LEN {
        return Err(VeyronError::InvalidPluginId(format!(
            "too long ({} bytes, max {MAX_LEN})",
            id.len()
        )));
    }
    if id == "kernel" || id == "*" {
        return Err(VeyronError::InvalidPluginId(format!("'{id}' is reserved")));
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(VeyronError::InvalidPluginId(
            "only ASCII letters, digits, '.', '-', '_' are allowed".into(),
        ));
    }
    Ok(())
}
