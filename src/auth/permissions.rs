use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::PermissionType;
use crate::utils::errors::VeyronError;

pub fn check_permission(
    registry: &PluginRegistry,
    plugin_id: &str,
    required: PermissionType,
) -> Result<(), VeyronError> {
    let entry = registry
        .get(plugin_id)
        .ok_or_else(|| VeyronError::PluginNotFound(plugin_id.to_string()))?;

    let required_str = required.as_str_name();
    if entry.manifest.permissions.iter().any(|p| p == required_str) {
        Ok(())
    } else {
        Err(VeyronError::PermissionDenied(format!(
            "{plugin_id} lacks {required_str}"
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

pub fn action_to_permission(action: &str) -> Option<PermissionType> {
    match action {
        "http_get" | "http_post" | "http_put" | "http_delete" | "http_patch" => {
            Some(PermissionType::PermissionNetwork)
        }
        "read_file" | "list_dir" => Some(PermissionType::PermissionFilesRead),
        "write_file" | "delete_file" => Some(PermissionType::PermissionFilesWrite),
        "get_cpu" | "get_memory" | "get_disk" => Some(PermissionType::PermissionSystem),
        "play_audio" | "record_audio" => Some(PermissionType::PermissionAudio),
        "send_notification" => Some(PermissionType::PermissionNotify),
        "set_timer" | "create_alarm" => Some(PermissionType::PermissionScheduler),
        "browser_navigate" | "browser_screenshot" => Some(PermissionType::PermissionBrowser),
        _ => None,
    }
}
