use crate::ipc::connection::Outbound;
use crate::proto::veyron::{PermissionType, PluginManifest};
use crate::utils::errors::VeyronError;
use dashmap::DashMap;
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum PluginState {
    Registered,
}

/// Kernel-local device record (D-02). Kernel runs `veyron-wire` 0.2.2 (proto
/// v1.5) with no `DeviceInfo` message yet — D-03 bumps to 0.2.3 and swaps this
/// for the wire type; kept shape-compatible so the swap is mechanical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub device_id: String,
    /// free-form until D-03 wires the `DeviceOs` enum
    pub os: String,
    pub arch: String,
    pub os_version: String,
    pub capabilities: Vec<String>,
    /// unix millis of the last ping/pong (reuses `pong_times`)
    pub last_seen: u64,
    pub state: DeviceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    Online,
    Offline,
}

impl DeviceInfo {
    fn new(device_id: &str, now_ms: u64) -> Self {
        DeviceInfo {
            device_id: device_id.to_string(),
            os: String::new(),
            arch: String::new(),
            os_version: String::new(),
            capabilities: Vec::new(),
            last_seen: now_ms,
            state: DeviceState::Online,
        }
    }
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
#[derive(Clone)]
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
    /// Mirrors the originating `ActionRequest.streaming` (R6-02/R6-04).
    /// Distinguishes "this ActionResponse{OK} accepts a long-lived session,
    /// don't evict" from "this ActionResponse completes an ordinary action,
    /// evict as always" — both share the same ActionResponse wire message.
    pub streaming: bool,
    /// R6-04: flips true on the provider's first ActionResponse{OK} for a
    /// streaming request. False entries are still subject to the R5-07
    /// deadline sweep (`sweep_expired_actions`); true entries are exempt
    /// from it and instead governed by `sweep_idle_sessions`.
    pub session_accepted: bool,
    /// R6-04: last time an ActionRequestChunk/ActionResponseChunk was seen
    /// in either direction for this entry. Updated by `touch_pending_action`.
    /// Only meaningful once `session_accepted` is true.
    pub last_activity: Instant,
}

#[derive(Debug, Clone)]
pub struct PluginEntry {
    pub plugin_id: String,
    pub conn_id: u64,
    pub manifest: PluginManifest,
    pub write_tx: mpsc::Sender<Outbound>,
    pub registered_at: u64,
    pub state: PluginState,
    /// owning device (D-02); "local" for host plugins that don't declare one
    pub device_id: String,
    /// owning user (D-02); "default" for single-user deployments
    pub user_id: String,
}

pub struct PluginRegistry {
    by_plugin_id: DashMap<String, PluginEntry>,
    by_conn_id: DashMap<u64, String>,
    pong_times: DashMap<String, Instant>,
    /// one record per device_id (D-02); `last_seen` advances on ping/pong
    devices: DashMap<String, DeviceInfo>,
    pending_actions: DashMap<String, PendingAction>,
    /// Manifest v2 per-action requirements: provider plugin_id → (action name →
    /// required PermissionType). Populated at load time from the manifest; the
    /// router consults it before the legacy hardcoded map.
    action_requirements: DashMap<String, HashMap<String, PermissionType>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        PluginRegistry {
            by_plugin_id: DashMap::new(),
            by_conn_id: DashMap::new(),
            pong_times: DashMap::new(),
            devices: DashMap::new(),
            pending_actions: DashMap::new(),
            action_requirements: DashMap::new(),
        }
    }

    pub fn register(
        &self,
        plugin_id: String,
        conn_id: u64,
        manifest: PluginManifest,
        write_tx: mpsc::Sender<Outbound>,
        device_id: &str,
        user_id: &str,
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

        // D-02: host plugins (no device identity on the wire until D-03) fall
        // back to the local single-user deployment.
        let device_id = if device_id.is_empty() {
            "local"
        } else {
            device_id
        };
        let user_id = if user_id.is_empty() {
            "default"
        } else {
            user_id
        };

        let registered_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let now_ms = unix_millis();

        let entry = PluginEntry {
            plugin_id: plugin_id.clone(),
            conn_id,
            manifest,
            write_tx,
            registered_at,
            state: PluginState::Registered,
            device_id: device_id.to_string(),
            user_id: user_id.to_string(),
        };

        conn_slot.insert(plugin_id.clone());
        self.pong_times.insert(plugin_id, Instant::now());
        // a registering plugin proves its device is alive — upsert the record
        match self.devices.entry(device_id.to_string()) {
            Entry::Occupied(mut occ) => {
                occ.get_mut().last_seen = now_ms;
                occ.get_mut().state = DeviceState::Online;
            }
            Entry::Vacant(v) => {
                v.insert(DeviceInfo::new(device_id, now_ms));
            }
        }
        plugin_slot.insert(entry);
        Ok(())
    }

    pub fn unregister(&self, plugin_id: &str) {
        if let Some((_, entry)) = self.by_plugin_id.remove(plugin_id) {
            self.by_conn_id.remove(&entry.conn_id);
            self.pong_times.remove(plugin_id);
            self.clear_action_requirements(plugin_id);
            // D-02: a device is offline once none of its plugins remain
            let device_id = &entry.device_id;
            let still_registered = self.by_plugin_id.iter().any(|e| e.device_id == *device_id);
            if !still_registered {
                if let Some(mut dev) = self.devices.get_mut(device_id) {
                    dev.state = DeviceState::Offline;
                }
            }
        }
    }

    pub fn record_pong(&self, plugin_id: &str) {
        self.pong_times
            .insert(plugin_id.to_string(), Instant::now());
        // D-02: a pong proves the owning device is alive — advance last_seen
        if let Some(entry) = self.by_plugin_id.get(plugin_id) {
            if let Some(mut dev) = self.devices.get_mut(&entry.device_id) {
                dev.last_seen = unix_millis();
                dev.state = DeviceState::Online;
            }
        }
    }

    pub fn last_pong(&self, plugin_id: &str) -> Option<Instant> {
        self.pong_times.get(plugin_id).map(|t| *t)
    }

    pub fn get(&self, plugin_id: &str) -> Option<PluginEntry> {
        self.by_plugin_id.get(plugin_id).map(|e| e.clone())
    }

    /// Device record for `device_id`, if any plugin ever registered from it.
    pub fn get_device(&self, device_id: &str) -> Option<DeviceInfo> {
        self.devices.get(device_id).map(|d| d.clone())
    }

    /// All known devices (D-02), for the discovery surface (D-04).
    pub fn list_devices(&self) -> Vec<DeviceInfo> {
        self.devices.iter().map(|d| d.value().clone()).collect()
    }

    /// Manifest v2: store the provider-declared per-action permission
    /// requirements for `plugin_id`. Called at load time from the manifest.
    pub fn set_action_requirements(
        &self,
        plugin_id: String,
        requirements: HashMap<String, PermissionType>,
    ) {
        self.action_requirements.insert(plugin_id, requirements);
    }

    /// Manifest v2: the permission a caller must hold to invoke `action` on
    /// `plugin_id`, if the provider declared one. `None` = no data-driven
    /// requirement (the router then falls back to the legacy hardcoded map).
    pub fn action_requirement(&self, plugin_id: &str, action: &str) -> Option<PermissionType> {
        self.action_requirements
            .get(plugin_id)
            .and_then(|m| m.get(action).copied())
    }

    pub fn clear_action_requirements(&self, plugin_id: &str) {
        self.action_requirements.remove(plugin_id);
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

    /// R6-04: resolve an inbound `ActionResponse` against its pending
    /// action, atomically (single DashMap shard lock via `entry()`, same
    /// TOCTOU-safety concern `register()` already documents). If the
    /// response is `ACTION_OK` for a `streaming` request, the session is
    /// *accepted in place* — `session_accepted` flips true, `last_activity`
    /// resets, and the entry is NOT removed; the (now-updated) clone is
    /// returned so the caller can still forward the accepting response.
    /// Every other case (non-streaming, or a streaming request that
    /// errored) evicts exactly as `take_pending_action_if_provider` always
    /// has. Returns `None` if `internal_id` doesn't exist or wasn't routed
    /// to `provider_id` (response-spoofing guard, unchanged).
    pub fn resolve_action_response(
        &self,
        internal_id: &str,
        provider_id: &str,
        status_ok: bool,
    ) -> Option<PendingAction> {
        use dashmap::mapref::entry::Entry;
        match self.pending_actions.entry(internal_id.to_string()) {
            Entry::Occupied(mut occ) => {
                if occ.get().provider_id != provider_id {
                    return None;
                }
                if status_ok && occ.get().streaming {
                    occ.get_mut().session_accepted = true;
                    occ.get_mut().last_activity = Instant::now();
                    Some(occ.get().clone())
                } else {
                    Some(occ.remove())
                }
            }
            Entry::Vacant(_) => None,
        }
    }

    /// R6-04: bump `last_activity` for an in-flight (found-or-not) pending
    /// action. Called on every `ActionRequestChunk`/`ActionResponseChunk`
    /// forwarded in either direction, so `sweep_idle_sessions` only fires on
    /// genuinely idle sessions. No-op if the entry doesn't exist (e.g. a
    /// late chunk for an already-evicted action — the chunk-forwarding arms
    /// already warn-and-drop that case separately).
    pub fn touch_pending_action(&self, internal_id: &str) {
        if let Some(mut entry) = self.pending_actions.get_mut(internal_id) {
            entry.last_activity = Instant::now();
        }
    }

    /// R6-04: evict and return `(internal_id, PendingAction)` for every
    /// *accepted* session idle longer than `idle_timeout` as of `now`.
    /// Mirrors `sweep_expired_actions`'s shape but filters on
    /// `session_accepted` + `last_activity` instead of `deadline` — the two
    /// sweeps are intentionally disjoint (a `PendingAction` is subject to
    /// exactly one of them at any time).
    pub fn sweep_idle_sessions(
        &self,
        now: Instant,
        idle_timeout: Duration,
    ) -> Vec<(String, PendingAction)> {
        let idle_keys: Vec<String> = self
            .pending_actions
            .iter()
            .filter(|e| e.session_accepted && now.duration_since(e.last_activity) > idle_timeout)
            .map(|e| e.key().clone())
            .collect();

        idle_keys
            .into_iter()
            .filter_map(|k| self.take_pending_action(&k).map(|p| (k, p)))
            .collect()
    }

    /// Evict and return all pending actions whose deadline has passed as of
    /// `now`. R6-04: an accepted streaming session is exempt — its
    /// `deadline` only ever governed the accept/reject window, and it's
    /// legitimately expected to outlive that once accepted. See
    /// `sweep_idle_sessions` for the sweep that applies post-acceptance.
    pub fn sweep_expired_actions(&self, now: Instant) -> Vec<PendingAction> {
        let expired_keys: Vec<String> = self
            .pending_actions
            .iter()
            .filter(|e| !e.session_accepted && e.deadline <= now)
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

    /// Read-only lookup by internal id — does not remove the entry. Used by
    /// R6-02 chunk forwarding, which needs to peek `provider_id`/
    /// `requester_write_tx` repeatedly across many chunks without consuming
    /// the pending-action slot (only the terminal `ActionResponse` or an
    /// abort removes it).
    pub fn get_pending_action(&self, internal_id: &str) -> Option<PendingAction> {
        self.pending_actions.get(internal_id).map(|e| e.clone())
    }

    /// Reverse lookup: the requester only ever knows its own `action_id`
    /// (the kernel translates to an internal id that only the provider
    /// sees), so inbound `ActionRequestChunk`s from the requester must be
    /// correlated by `(requester_id, original_action_id)` instead. A scan,
    /// bounded by total in-flight actions like `count_pending_actions_for` —
    /// but unlike that method (called once per action setup), this runs once
    /// per inbound request chunk, so its cost is O(chunks × in-flight
    /// actions) for a streaming upload, not O(1) per action. Accepted for
    /// now since fixing it would require a wire change letting the kernel
    /// hand the internal id back to the requester; revisit if a high-
    /// throughput streaming consumer materializes.
    pub fn find_pending_internal_id(
        &self,
        requester_id: &str,
        original_action_id: &str,
    ) -> Option<String> {
        self.pending_actions
            .iter()
            .find(|e| e.requester_id == requester_id && e.original_action_id == original_action_id)
            .map(|e| e.key().clone())
    }
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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
