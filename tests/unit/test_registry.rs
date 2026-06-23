use std::sync::Arc;
use tokio::sync::mpsc;
use veyron::ipc::framing::Frame;
use veyron::plugins::registry::{PluginRegistry, PluginState};
use veyron::proto::veyron::PluginManifest;
use veyron::utils::errors::VeyronError;

fn dummy_write_tx() -> mpsc::Sender<Frame> {
    mpsc::channel::<Frame>(1).0
}

fn dummy_manifest() -> PluginManifest {
    PluginManifest {
        permissions: vec![],
        actions: vec![],
        events: vec![],
        needs_gpu: false,
        priority: 1,
    }
}

#[test]
fn register_then_get_returns_entry() {
    let reg = PluginRegistry::new();

    reg.register(
        "weather".to_string(),
        42,
        dummy_manifest(),
        dummy_write_tx(),
    )
    .expect("register must succeed");

    let entry = reg.get("weather").expect("get must return entry");
    assert_eq!(entry.plugin_id, "weather");
    assert_eq!(entry.conn_id, 42);
    assert!(matches!(entry.state, PluginState::Registered));
}

#[test]
fn duplicate_plugin_id_rejected() {
    let reg = PluginRegistry::new();

    reg.register("dup".to_string(), 1, dummy_manifest(), dummy_write_tx())
        .expect("first register must succeed");

    let result = reg.register("dup".to_string(), 2, dummy_manifest(), dummy_write_tx());

    assert!(
        matches!(result, Err(VeyronError::PluginAlreadyRegistered(_))),
        "expected PluginAlreadyRegistered, got {:?}",
        result
    );
}

#[test]
fn unregister_removes_from_both_indexes() {
    let reg = PluginRegistry::new();

    reg.register("gone".to_string(), 99, dummy_manifest(), dummy_write_tx())
        .unwrap();

    reg.unregister("gone");

    assert!(
        reg.get("gone").is_none(),
        "get must return None after unregister"
    );
    assert!(
        !reg.is_registered(99),
        "is_registered must return false after unregister"
    );
}

#[test]
fn list_returns_all_registered_plugins() {
    let reg = PluginRegistry::new();

    reg.register("a".to_string(), 1, dummy_manifest(), dummy_write_tx())
        .unwrap();
    reg.register("b".to_string(), 2, dummy_manifest(), dummy_write_tx())
        .unwrap();
    reg.register("c".to_string(), 3, dummy_manifest(), dummy_write_tx())
        .unwrap();

    let list = reg.list();
    assert_eq!(list.len(), 3);

    let ids: Vec<&str> = list.iter().map(|e| e.plugin_id.as_str()).collect();
    assert!(ids.contains(&"a"));
    assert!(ids.contains(&"b"));
    assert!(ids.contains(&"c"));
}

#[test]
fn is_registered_returns_true_for_known_conn_id() {
    let reg = PluginRegistry::new();

    reg.register("ping".to_string(), 77, dummy_manifest(), dummy_write_tx())
        .unwrap();

    assert!(reg.is_registered(77));
    assert!(!reg.is_registered(999));
}

#[test]
fn get_by_conn_id_returns_correct_entry() {
    let reg = PluginRegistry::new();

    reg.register("alarm".to_string(), 55, dummy_manifest(), dummy_write_tx())
        .unwrap();

    let entry = reg.get_by_conn_id(55).expect("must find by conn_id");
    assert_eq!(entry.plugin_id, "alarm");
    assert_eq!(entry.conn_id, 55);
}

#[test]
fn get_by_conn_id_returns_none_for_unknown() {
    let reg = PluginRegistry::new();
    assert!(reg.get_by_conn_id(0).is_none());
}

#[test]
fn registry_is_thread_safe() {
    let reg = Arc::new(PluginRegistry::new());
    let mut handles = vec![];

    for i in 0u64..10 {
        let reg = Arc::clone(&reg);
        handles.push(std::thread::spawn(move || {
            let id = format!("plugin_{}", i);
            let _ = reg.register(id, i, dummy_manifest(), dummy_write_tx());
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(reg.list().len(), 10);
}

#[test]
fn entry_has_registered_at_timestamp() {
    let reg = PluginRegistry::new();

    reg.register("ts".to_string(), 1, dummy_manifest(), dummy_write_tx())
        .unwrap();

    let entry = reg.get("ts").unwrap();
    // registered_at should be non-zero (Unix timestamp in seconds)
    assert!(entry.registered_at > 0, "registered_at must be set");
}
