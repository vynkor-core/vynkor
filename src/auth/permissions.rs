use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::PermissionType;
use crate::utils::errors::VeyronError;

/// Maps a kernel-routed action name to the permission both its provider and
/// its requester must have declared (T-19: checking the provider alone lets
/// an unprivileged plugin launder the action through a permitted provider).
/// Actions not listed here are unrestricted (R5-07: declared the action is
/// authorization enough, no requester check). New sensitive actions must be
/// added here — this is the deny-by-omission escape hatch closed, not
/// opened, by adding an entry.
pub fn required_permission_for_action(action: &str) -> Option<PermissionType> {
    match action {
        "http_request" => Some(PermissionType::PermissionNetwork),
        _ => None,
    }
}

// lowercases and strips the PERMISSION_ prefix so manifests can declare either
// the documented lowercase form (network) or the proto name (PERMISSION_NETWORK)
fn normalize_permission(s: &str) -> String {
    s.strip_prefix("PERMISSION_")
        .unwrap_or(s)
        .to_ascii_lowercase()
}

pub fn check_permission(
    registry: &PluginRegistry,
    plugin_id: &str,
    required: PermissionType,
) -> Result<(), VeyronError> {
    let entry = registry
        .get(plugin_id)
        .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

    let required_norm = normalize_permission(required.as_str_name());
    if entry
        .manifest
        .permissions
        .iter()
        .any(|p| normalize_permission(p) == required_norm)
    {
        Ok(())
    } else {
        Err(VeyronError::PermissionDenied(format!(
            "{plugin_id} lacks {}",
            required.as_str_name()
        )))
    }
}

/// Gate peer-to-peer IPC: a plugin may only unicast to another plugin if it
/// declared `PERMISSION_IPC_SEND`. Default-deny — undeclared senders are rejected.
pub fn check_ipc_send(registry: &PluginRegistry, plugin_id: &str) -> Result<(), VeyronError> {
    check_permission(registry, plugin_id, PermissionType::PermissionIpcSend)
}

/// Gate per-target IPC (T-04): sender must list the target in `ipc_targets`.
/// Empty allowlist = deny-all, even with PERMISSION_IPC_SEND.
pub fn check_ipc_target(
    registry: &PluginRegistry,
    sender_id: &str,
    target_id: &str,
) -> Result<(), VeyronError> {
    let entry = registry
        .get(sender_id)
        .ok_or_else(|| VeyronError::PluginNotFound(sender_id.to_string()))?;

    if entry.manifest.ipc_targets.iter().any(|t| t == target_id) {
        Ok(())
    } else {
        Err(VeyronError::PermissionDenied(format!(
            "{sender_id} ipc_targets does not include {target_id}"
        )))
    }
}
