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

#[derive(Debug, Clone)]
pub enum ActionLookup {
    NotFound,
    Found(PluginEntry),
    /// Colliding plugin ids, for the caller to log.
    Ambiguous(Vec<String>),
}

/// A kernel-routed action awaiting the provider's reply. Keyed in
/// `PluginRegistry::pending_actions` by a kernel-minted internal id (not the
/// requester's own `action_id`, which is only unique per-process and could
/// collide across two different plugin connections).
pub struct PendingAction {
    pub requester_write_tx: mpsc::Sender<Outbound>,
    pub original_action_id: String,
    pub requester_id: String,
    pub deadline: Instant,
    /// plugin_id of the provider this action was routed to. Checked against
    /// the sender's identity before an `ActionResponse` is allowed to
    /// consume this slot, so an unrelated registered plugin can't spoof or
    /// steal the response for an action it wasn't routed.
    pub provider_id: String,
}

#[derive(Debug, Clone)]
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
    pending_actions: DashMap<String, PendingAction>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        PluginRegistry {
            by_plugin_id: DashMap::new(),
            by_conn_id: DashMap::new(),
            pong_times: DashMap::new(),
            pending_actions: DashMap::new(),
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

    /// Scan registered plugins for one whose `manifest.actions` declares
    /// `action`. Ambiguity (>1 declarer) is surfaced rather than resolved —
    /// picking a winner would hide a deploy misconfiguration.
    pub fn find_action_provider(&self, action: &str) -> ActionLookup {
        let matches: Vec<PluginEntry> = self
            .by_plugin_id
            .iter()
            .filter(|e| e.manifest.actions.iter().any(|a| a == action))
            .map(|e| e.value().clone())
            .collect();

        match matches.len() {
            0 => ActionLookup::NotFound,
            1 => ActionLookup::Found(matches.into_iter().next().unwrap()),
            _ => ActionLookup::Ambiguous(matches.into_iter().map(|e| e.plugin_id).collect()),
        }
    }

    pub fn register_pending_action(&self, internal_id: String, pending: PendingAction) {
        self.pending_actions.insert(internal_id, pending);
    }

    pub fn take_pending_action(&self, internal_id: &str) -> Option<PendingAction> {
        self.pending_actions.remove(internal_id).map(|(_, v)| v)
    }

    /// Atomically remove and return the pending action for `internal_id`
    /// only if it was routed to `provider_id`. If the entry exists but was
    /// routed to a different provider, it is left in place (so the real
    /// provider's later response can still consume it) and `None` is
    /// returned — this is what prevents a non-provider plugin from
    /// spoofing or stealing another provider's in-flight response slot.
    pub fn take_pending_action_if_provider(
        &self,
        internal_id: &str,
        provider_id: &str,
    ) -> Option<PendingAction> {
        self.pending_actions
            .remove_if(internal_id, |_, pending| pending.provider_id == provider_id)
            .map(|(_, v)| v)
    }

    /// Evict and return all pending actions whose deadline has passed as of `now`.
    pub fn sweep_expired_actions(&self, now: Instant) -> Vec<PendingAction> {
        let expired_keys: Vec<String> = self
            .pending_actions
            .iter()
            .filter(|e| e.deadline <= now)
            .map(|e| e.key().clone())
            .collect();

        expired_keys
            .into_iter()
            .filter_map(|k| self.take_pending_action(&k))
            .collect()
    }

    /// Count in-flight pending actions for a given `(requester_id, provider_id)`
    /// pair (R6-03). Used to enforce the per-caller concurrency cap against a
    /// shared provider. A scan, not a maintained counter — bounded by total
    /// kernel-wide in-flight actions, which `sweep_expired_actions` already
    /// keeps bounded, and can't desync the way a separately incremented/
    /// decremented counter could across the three existing removal sites.
    pub fn count_pending_actions_for(&self, requester_id: &str, provider_id: &str) -> u32 {
        self.pending_actions
            .iter()
            .filter(|e| e.requester_id == requester_id && e.provider_id == provider_id)
            .count() as u32
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
