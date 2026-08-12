use std::fs;
use std::os::unix::fs::symlink;
use std::time::Duration;

use tempfile::tempdir;
use tokio::time::sleep;

use std::sync::Arc;
use veyron::plugins::loader::{validate_plugin_def, PluginLoader};
use veyron::plugins::manager::PluginManager;
use veyron::plugins::registry::PluginRegistry;
use veyron::plugins::supervisor::PluginSupervisor;
use veyron::utils::config::PluginDef;

fn make_manager(socket: &str) -> Arc<PluginManager> {
    let sup = Arc::new(PluginSupervisor::new(socket));
    let reg = Arc::new(PluginRegistry::new());
    Arc::new(PluginManager::new(sup, reg))
}

fn def_with_binary(id: &str, binary: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: binary.to_string(),
        restart: "never".to_string(),
        max_restarts: 0,
        args: vec![],
        env: vec![],
        sandbox: false,
        grace_seconds: 5,
        max_procs: None,
        max_vmem_mb: None,
        permissions: vec![],
        ..Default::default()
    }
}

fn write_manifest(dir: &std::path::Path, json: &str) {
    fs::write(dir.join("plugin.json"), json).unwrap();
}

// Unit test: plugin.json with min: "99.0.0" → refused, error returned
#[test]
fn incompatible_min_kernel_refused() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "future-plugin",
            "version": "1.0.0",
            "permissions": [],
            "binary": "future-plugin",
            "kernel_compatibility_range": {"min": "99.0.0", "max": "*"}
        }"#,
    );
    // Place binary path inside tmp so parent dir resolves to tmp
    let binary_path = tmp.path().join("future-plugin").display().to_string();
    let def = def_with_binary("future-plugin", &binary_path);
    let err = validate_plugin_def(&def).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel >= 99.0.0"),
        "expected compat error, got: {msg}"
    );
}

// Unit test: plugin.json with unknown permission "teleport" → refused
#[test]
fn unknown_permission_refused() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "bad-perm-plugin",
            "version": "1.0.0",
            "permissions": ["teleport"],
            "binary": "bad-perm-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let binary_path = tmp.path().join("bad-perm-plugin").display().to_string();
    let def = def_with_binary("bad-perm-plugin", &binary_path);
    let err = validate_plugin_def(&def).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown permission") && msg.contains("teleport"),
        "expected unknown permission error, got: {msg}"
    );
}

// Unit test: valid plugin.json within range → Ok
#[test]
fn valid_manifest_passes() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "good-plugin",
            "version": "1.0.0",
            "permissions": ["network"],
            "binary": "good-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let binary_path = tmp.path().join("good-plugin").display().to_string();
    let def = def_with_binary("good-plugin", &binary_path);
    assert!(validate_plugin_def(&def).is_ok());
}

// Unit test: config denies a permission the manifest requests → refused
#[test]
fn config_permission_restriction_enforced() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "restricted-plugin",
            "version": "1.0.0",
            "permissions": ["audio_stream", "network"],
            "binary": "restricted-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let binary_path = tmp.path().join("restricted-plugin").display().to_string();
    let mut def = def_with_binary("restricted-plugin", &binary_path);
    // Operator grants only "network", not "audio_stream"
    def.permissions = vec!["network".to_string()];
    let err = validate_plugin_def(&def).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("audio_stream") && msg.contains("not granted in config"),
        "expected permission denied, got: {msg}"
    );
}

// Integration test: one invalid + one valid plugin → valid loads, invalid skipped, kernel stays up
#[tokio::test]
async fn invalid_plugin_skipped_valid_loads() {
    let tmp = tempdir().unwrap();

    // Invalid plugin: min kernel 99.0.0
    let invalid_dir = tmp.path().join("invalid-plugin");
    fs::create_dir_all(&invalid_dir).unwrap();
    write_manifest(
        &invalid_dir,
        r#"{
            "plugin_id": "invalid-plugin",
            "version": "1.0.0",
            "permissions": [],
            "binary": "invalid-plugin",
            "kernel_compatibility_range": {"min": "99.0.0", "max": "*"}
        }"#,
    );
    // symlink a real binary into the invalid plugin dir
    symlink("/bin/sleep", invalid_dir.join("invalid-plugin")).unwrap();

    // Valid plugin: compatible manifest + real sleep binary
    let valid_dir = tmp.path().join("good-plugin");
    fs::create_dir_all(&valid_dir).unwrap();
    write_manifest(
        &valid_dir,
        r#"{
            "plugin_id": "good-plugin",
            "version": "1.0.0",
            "permissions": [],
            "binary": "good-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    symlink("/bin/sleep", valid_dir.join("good-plugin")).unwrap();

    let defs = vec![
        PluginDef {
            id: "invalid-plugin".to_string(),
            binary: invalid_dir.join("invalid-plugin").display().to_string(),
            restart: "never".to_string(),
            max_restarts: 0,
            args: vec!["60".to_string()],
            env: vec![],
            sandbox: false,
            grace_seconds: 1,
            max_procs: None,
            max_vmem_mb: None,
            permissions: vec![],
            ..Default::default()
        },
        PluginDef {
            id: "good-plugin".to_string(),
            binary: valid_dir.join("good-plugin").display().to_string(),
            restart: "never".to_string(),
            max_restarts: 0,
            args: vec!["60".to_string()],
            env: vec![],
            sandbox: false,
            grace_seconds: 1,
            max_procs: None,
            max_vmem_mb: None,
            permissions: vec![],
            ..Default::default()
        },
    ];

    let manager = make_manager("/tmp/veyron_manifest_enforcement.sock");
    PluginLoader::load_all(&defs, &manager, None).await;
    sleep(Duration::from_millis(100)).await;

    assert!(
        !manager.is_supervised("invalid-plugin"),
        "invalid plugin (min: 99.0.0) must be skipped"
    );
    assert!(
        manager.is_supervised("good-plugin"),
        "valid plugin must be supervised after load_all"
    );

    manager.stop("good-plugin").await.ok();
}

// Unit test (N2): config.yaml lowercase form satisfies a manifest declaring the
// PERMISSION_* proto name; a genuinely un-granted permission is still refused.
#[test]
fn config_lowercase_perm_matches_manifest_proto_form() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "cross-form-plugin",
            "version": "1.0.0",
            "permissions": ["PERMISSION_NETWORK"],
            "binary": "cross-form-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let binary_path = tmp.path().join("cross-form-plugin").display().to_string();
    let mut def = def_with_binary("cross-form-plugin", &binary_path);
    // operator grants the documented lowercase form only
    def.permissions = vec!["network".to_string()];
    assert!(
        validate_plugin_def(&def).is_ok(),
        "lowercase config form must satisfy the PERMISSION_ prefix manifest form (N2)"
    );

    // negative control: a permission outside the grant is still refused
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "cross-form-plugin",
            "version": "1.0.0",
            "permissions": ["PERMISSION_KERNEL_ADMIN"],
            "binary": "cross-form-plugin",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let err = validate_plugin_def(&def).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("PERMISSION_KERNEL_ADMIN") && msg.contains("not granted in config"),
        "expected permission denied, got: {msg}"
    );
}
