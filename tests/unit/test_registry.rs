use std::sync::Arc;
use tokio::sync::mpsc;
use veyron::plugins::registry::{PluginRegistry, PluginState};
use veyron::proto::veyron::PluginManifest;
use veyron::utils::errors::VeyronError;

fn dummy_write_tx() -> mpsc::Sender<veyron::ipc::connection::Outbound> {
    mpsc::channel::<veyron::ipc::connection::Outbound>(1).0
}

fn dummy_manifest() -> PluginManifest {
    PluginManifest {
        permissions: vec![],
        actions: vec![],
        events: vec![],
        ipc_targets: vec![],
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
fn register_rejects_invalid_plugin_ids() {
    let reg = PluginRegistry::new();

    let long = "x".repeat(33);
    let bad: Vec<&str> = vec![
        "",                            // empty
        "kernel",                      // reserved routing target
        "*",                           // reserved broadcast target
        r#"evil","admin":true,"x":""#, // JSON-injection attempt
        "has space",                   // disallowed char
        "tab\tthing",                  // control char
        &long,                         // exceeds 32-byte target field
    ];

    for id in bad {
        let res = reg.register(id.to_string(), 1, dummy_manifest(), dummy_write_tx());
        assert!(
            matches!(res, Err(VeyronError::InvalidPluginId(_))),
            "id {id:?} must be rejected, got {res:?}"
        );
    }

    // conn_id 1 was never consumed (all rejected pre-insert); a valid id registers
    reg.register(
        "ok.plugin-1_v2".to_string(),
        1,
        dummy_manifest(),
        dummy_write_tx(),
    )
    .expect("a well-formed id must register");
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
fn second_registration_on_same_conn_rejected_without_orphaning() {
    let reg = PluginRegistry::new();

    reg.register("first".to_string(), 7, dummy_manifest(), dummy_write_tx())
        .expect("first register must succeed");

    // same conn_id, different plugin_id — must be rejected
    let result = reg.register("second".to_string(), 7, dummy_manifest(), dummy_write_tx());
    assert!(
        matches!(result, Err(VeyronError::PluginAlreadyRegistered(_))),
        "re-registration on a live conn must be rejected, got {:?}",
        result
    );

    // "second" must not exist; "first" must still be intact and reachable by conn
    assert!(
        reg.get("second").is_none(),
        "rejected id must not be stored"
    );
    assert!(reg.get("first").is_some(), "original must survive");
    assert_eq!(
        reg.get_by_conn_id(7).map(|e| e.plugin_id),
        Some("first".to_string()),
        "conn must still map to the original plugin"
    );

    // disconnect cleans up fully — no orphan left behind
    reg.unregister("first");
    assert!(reg.get("first").is_none());
    assert!(reg.list().is_empty(), "no orphaned entries after cleanup");
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
fn concurrent_registration_of_same_plugin_id_has_exactly_one_winner() {
    // AUDIT M-08 regression: check-then-insert across by_plugin_id/by_conn_id
    // was only TOCTOU-safe because the router calls register() from a single
    // task. Hammer the same plugin_id from many threads/conn_ids concurrently
    // and assert the registry never ends up with more than one entry for it.
    let reg = Arc::new(PluginRegistry::new());
    let mut handles = vec![];

    for conn_id in 0u64..50 {
        let reg = Arc::clone(&reg);
        handles.push(std::thread::spawn(move || {
            reg.register(
                "contended".to_string(),
                conn_id,
                dummy_manifest(),
                dummy_write_tx(),
            )
            .is_ok()
        }));
    }

    let successes = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|ok| *ok)
        .count();

    assert_eq!(successes, 1, "exactly one registration must win the race");
    assert_eq!(reg.list().len(), 1);
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

fn manifest_with_actions(actions: &[&str]) -> PluginManifest {
    PluginManifest {
        actions: actions.iter().map(|s| s.to_string()).collect(),
        ..dummy_manifest()
    }
}

#[test]
fn find_action_provider_returns_not_found_when_no_provider() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather".to_string(),
        1,
        manifest_with_actions(&["get_forecast"]),
        dummy_write_tx(),
    )
    .unwrap();

    assert!(matches!(
        reg.find_action_provider("get_weather"),
        ActionLookup::NotFound
    ));
}

#[test]
fn find_action_provider_returns_found_for_single_provider() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather".to_string(),
        1,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();

    match reg.find_action_provider("get_weather") {
        ActionLookup::Found(entry) => assert_eq!(entry.plugin_id, "weather"),
        other => panic!("expected Found, got {other:?}"),
    }
}

#[test]
fn find_action_provider_returns_ambiguous_for_multiple_providers() {
    use veyron::plugins::registry::ActionLookup;

    let reg = PluginRegistry::new();
    reg.register(
        "weather-a".to_string(),
        1,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();
    reg.register(
        "weather-b".to_string(),
        2,
        manifest_with_actions(&["get_weather"]),
        dummy_write_tx(),
    )
    .unwrap();

    match reg.find_action_provider("get_weather") {
        ActionLookup::Ambiguous(mut ids) => {
            ids.sort();
            assert_eq!(ids, vec!["weather-a".to_string(), "weather-b".to_string()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}
