use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use vynkor::plugins::loader::PluginLoader;
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::{ActionLookup, PluginRegistry};
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::utils::config::PluginDef;

fn make_manager() -> Arc<PluginManager> {
    let sup = Arc::new(PluginSupervisor::new("/tmp/vynkor_lazy_plugins.sock"));
    Arc::new(PluginManager::new(sup, Arc::new(PluginRegistry::new())))
}

/// plugin dir with a dummy executable and the given plugin.json
fn lazy_def(dir: &Path, id: &str, manifest: serde_json::Value) -> PluginDef {
    let bin = dir.join("plugin.sh");
    std::fs::write(&bin, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.join("plugin.json"), manifest.to_string()).unwrap();
    PluginDef {
        id: id.to_string(),
        binary: bin.to_string_lossy().into_owned(),
        restart: "never".to_string(),
        max_restarts: 0,
        sandbox: false,
        grace_seconds: 1,
        lazy: true,
        ..Default::default()
    }
}

fn manifest(id: &str, extra: serde_json::Value) -> serde_json::Value {
    let mut m = serde_json::json!({
        "plugin_id": id,
        "version": "1.0.0",
        "binary": "plugin.sh",
        "permissions": [],
        "kernel_compatibility_range": {"min": "0.0.1", "max": "99.0.0"},
        "actions": [{"name": "lazy_echo", "description": "echo", "risk": "low"}],
    });
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    m
}

#[tokio::test]
async fn lazy_plugin_is_not_spawned_at_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let def = lazy_def(
        tmp.path(),
        "lazy-a",
        manifest("lazy-a", serde_json::json!({})),
    );
    let manager = make_manager();
    let cfgs = PluginLoader::load_all(&[def], &manager, None, 300).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let reg = manager.registry();
    assert!(reg.get("lazy-a").is_none());
    assert!(!manager.is_supervised("lazy-a"), "must not be spawned");
    let d = reg.get_dormant("lazy-a").expect("dormant record");
    assert_eq!(d.action_specs[0].name, "lazy_echo");
    assert_eq!(d.action_specs[0].description, "echo");
    assert_eq!(d.idle_stop, Duration::from_secs(300));
    assert!(matches!(
        reg.find_action_provider("lazy_echo"),
        ActionLookup::Dormant(id) if id == "lazy-a"
    ));
    assert!(cfgs.contains_key("lazy-a"), "config kept for the waker");
}

#[tokio::test]
async fn lazy_idle_stop_prefers_plugin_override() {
    let tmp = tempfile::tempdir().unwrap();
    let mut def = lazy_def(
        tmp.path(),
        "lazy-b",
        manifest("lazy-b", serde_json::json!({})),
    );
    def.idle_stop_secs = Some(7);
    let manager = make_manager();
    PluginLoader::load_all(&[def], &manager, None, 300).await;
    assert_eq!(
        manager.registry().get_dormant("lazy-b").unwrap().idle_stop,
        Duration::from_secs(7)
    );
}

#[tokio::test]
async fn lazy_with_events_loads_eagerly() {
    let tmp = tempfile::tempdir().unwrap();
    let def = lazy_def(
        tmp.path(),
        "lazy-ev",
        manifest("lazy-ev", serde_json::json!({"events": ["x.y"]})),
    );
    let manager = make_manager();
    let cfgs = PluginLoader::load_all(&[def], &manager, None, 300).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(manager.is_supervised("lazy-ev"), "must be spawned at boot");
    assert!(manager.registry().get_dormant("lazy-ev").is_none());
    assert!(!cfgs.contains_key("lazy-ev"));
    manager.stop("lazy-ev").await.ok();
}

#[tokio::test]
async fn lazy_without_actions_loads_eagerly() {
    let tmp = tempfile::tempdir().unwrap();
    let def = lazy_def(
        tmp.path(),
        "lazy-na",
        manifest("lazy-na", serde_json::json!({"actions": []})),
    );
    let manager = make_manager();
    let cfgs = PluginLoader::load_all(&[def], &manager, None, 300).await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(manager.is_supervised("lazy-na"), "unwakeable, so eager");
    assert!(manager.registry().get_dormant("lazy-na").is_none());
    assert!(!cfgs.contains_key("lazy-na"));
    manager.stop("lazy-na").await.ok();
}
