#![allow(dead_code)]

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
        "ai_complete" | "ai_embed" => Some(PermissionType::PermissionAi),
        "set_timer" | "create_alarm" => Some(PermissionType::PermissionScheduler),
        "browser_navigate" | "browser_screenshot" => Some(PermissionType::PermissionBrowser),
        _ => None,
    }
}
