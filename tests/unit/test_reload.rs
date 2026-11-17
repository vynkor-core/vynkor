//! plugins.d hot-load: the Reloader starts, stops and restarts real
//! processes as the plugin list changes, and the shared defs follow.

use std::collections::HashMap;
use std::sync::Arc;

use vynkor::events::bus::EventBus;
use vynkor::plugins::lazy::ManagerStarter;
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::reload::{Reloader, SharedDefs};
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::utils::config::PluginDef;

fn sleep_def(id: &str, secs: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: "/bin/sleep".to_string(),
        args: vec![secs.to_string()],
        restart: "never".to_string(),
        ..Default::default()
    }
}

fn reloader(defs: &SharedDefs) -> (Arc<PluginManager>, Reloader) {
    let registry = Arc::new(PluginRegistry::new());
    let supervisor = Arc::new(PluginSupervisor::new("/tmp/vynkor_test_reload.sock"));
    let manager = Arc::new(PluginManager::new(supervisor, registry));
    let lazy = Arc::new(ManagerStarter::new(Arc::clone(&manager), HashMap::new()));
    let r = Reloader::new(
        Arc::clone(&manager),
        Arc::new(EventBus::new()),
        defs.clone(),
        lazy,
        300,
        None,
    );
    (manager, r)
}

#[tokio::test]
async fn reload_starts_added_stops_removed_restarts_changed() {
    let defs = SharedDefs::default();
    let (manager, r) = reloader(&defs);

    let d = r
        .apply(vec![sleep_def("rl-a", "60"), sleep_def("rl-b", "60")])
        .await;
    assert_eq!(d.added, ["rl-a", "rl-b"]);
    assert!(manager.is_supervised("rl-a") && manager.is_supervised("rl-b"));
    assert!(defs.get("rl-b").is_some(), "the api sees the new set");

    // same list again: nothing happens
    assert!(r
        .apply(vec![sleep_def("rl-a", "60"), sleep_def("rl-b", "60")])
        .await
        .is_empty());

    // b gone, a changed (new args): b stops, a runs again
    let d = r.apply(vec![sleep_def("rl-a", "61")]).await;
    assert_eq!(
        (d.removed.clone(), d.changed.clone()),
        (vec!["rl-b".to_string()], vec!["rl-a".to_string()])
    );
    assert!(!manager.is_supervised("rl-b"));
    assert!(manager.is_supervised("rl-a"));
    assert_eq!(defs.get("rl-a").unwrap().args, ["61"]);

    r.apply(vec![]).await;
    assert!(!manager.is_supervised("rl-a"));
}

#[tokio::test]
async fn reload_without_a_config_file_is_an_error() {
    let (_m, r) = reloader(&SharedDefs::default());
    assert!(r.reload().await.is_err());
}

#[tokio::test]
async fn reload_reads_dropins_from_the_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("config.yaml");
    std::fs::write(&cfg, "plugins: []\n").unwrap();
    let defs = SharedDefs::default();
    let registry = Arc::new(PluginRegistry::new());
    let supervisor = Arc::new(PluginSupervisor::new("/tmp/vynkor_test_reload2.sock"));
    let manager = Arc::new(PluginManager::new(supervisor, registry));
    let lazy = Arc::new(ManagerStarter::new(Arc::clone(&manager), HashMap::new()));
    let r = Reloader::new(
        Arc::clone(&manager),
        Arc::new(EventBus::new()),
        defs.clone(),
        lazy,
        300,
        Some(cfg.to_string_lossy().into_owned()),
    );
    assert!(r.reload().await.unwrap().is_empty());

    // an app drops a plugin in plugins.d, then asks for a reload
    std::fs::create_dir_all(dir.path().join("plugins.d")).unwrap();
    std::fs::write(
        dir.path().join("plugins.d/rl-drop.yaml"),
        "id: rl-drop\nbinary: /bin/sleep\nargs: [\"60\"]\nrestart: never\n",
    )
    .unwrap();
    let d = r.reload().await.unwrap();
    assert_eq!(d.added, ["rl-drop"]);
    assert!(manager.is_supervised("rl-drop"));

    std::fs::remove_file(dir.path().join("plugins.d/rl-drop.yaml")).unwrap();
    let d = r.reload().await.unwrap();
    assert_eq!(d.removed, ["rl-drop"]);
    assert!(!manager.is_supervised("rl-drop"));
}
