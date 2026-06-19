use std::sync::Arc;
use tokio::sync::mpsc;
use veyron::auth::permissions::{action_to_permission, check_permission};
use veyron::plugins::registry::PluginRegistry;
use veyron::proto::veyron::{PermissionType, PluginManifest};

fn registry_with(plugin_id: &str, permissions: Vec<&str>) -> Arc<PluginRegistry> {
    let registry = Arc::new(PluginRegistry::new());
    let (tx, _rx) = mpsc::channel(1);
    let manifest = PluginManifest {
        permissions: permissions.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    registry
        .register(plugin_id.to_string(), 1, manifest, tx)
        .unwrap();
    registry
}

#[test]
fn plugin_with_network_permission_passes_check() {
    let registry = registry_with("net_plugin", vec!["PERMISSION_NETWORK"]);
    let result = check_permission(&registry, "net_plugin", PermissionType::PermissionNetwork);
    assert!(result.is_ok());
}

#[test]
fn plugin_without_network_permission_is_denied() {
    let registry = registry_with("bare_plugin", vec![]);
    let result = check_permission(&registry, "bare_plugin", PermissionType::PermissionNetwork);
    assert!(result.is_err());
}

#[test]
fn unknown_plugin_returns_not_found_error() {
    let registry = Arc::new(PluginRegistry::new());
    let result = check_permission(&registry, "ghost", PermissionType::PermissionNetwork);
    assert!(result.is_err());
}

#[test]
fn action_http_get_maps_to_network_permission() {
    assert_eq!(
        action_to_permission("http_get"),
        Some(PermissionType::PermissionNetwork)
    );
}

#[test]
fn action_read_file_maps_to_files_read() {
    assert_eq!(
        action_to_permission("read_file"),
        Some(PermissionType::PermissionFilesRead)
    );
}

#[test]
fn action_write_file_maps_to_files_write() {
    assert_eq!(
        action_to_permission("write_file"),
        Some(PermissionType::PermissionFilesWrite)
    );
}

#[test]
fn unknown_action_maps_to_none() {
    assert_eq!(action_to_permission("fly_to_moon"), None);
}
