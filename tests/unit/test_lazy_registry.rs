use std::time::Duration;
use vynkor::plugins::registry::{ActionLookup, DormantPlugin, PluginRegistry};
use vynkor::proto::vynkor::{ActionSpec, PluginManifest};
use vynkor::utils::config::{Config, PluginDef};

fn dormant(id: &str, actions: &[&str]) -> DormantPlugin {
    DormantPlugin {
        plugin_id: id.to_string(),
        action_specs: actions
            .iter()
            .map(|a| ActionSpec {
                name: a.to_string(),
                ..Default::default()
            })
            .collect(),
        idle_stop: Duration::from_secs(600),
    }
}

fn live(reg: &PluginRegistry, id: &str, conn: u64, actions: &[&str]) {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let manifest = PluginManifest {
        actions: actions.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    reg.register(id.to_string(), conn, manifest, tx, "", "")
        .unwrap();
}

#[test]
fn dormant_action_resolves_to_dormant() {
    let reg = PluginRegistry::new();
    reg.register_dormant(dormant("telegram", &["tg_x"]));
    match reg.find_action_provider("tg_x") {
        ActionLookup::Dormant(id) => assert_eq!(id, "telegram"),
        other => panic!("expected Dormant, got {other:?}"),
    }
    assert!(matches!(
        reg.find_action_provider("other"),
        ActionLookup::NotFound
    ));
    assert!(reg.is_lazy("telegram"));
    assert!(!reg.is_lazy("nope"));
    assert_eq!(reg.get_dormant("telegram").unwrap().plugin_id, "telegram");
    assert_eq!(reg.list_dormant().len(), 1);
}

#[test]
fn live_registration_wins_over_dormant() {
    let reg = PluginRegistry::new();
    reg.register_dormant(dormant("telegram", &["tg_x"]));
    live(&reg, "telegram", 1, &["tg_x"]);
    match reg.find_action_provider("tg_x") {
        ActionLookup::Found(e) => assert_eq!(e.plugin_id, "telegram"),
        other => panic!("expected Found, got {other:?}"),
    }
    reg.unregister("telegram");
    match reg.find_action_provider("tg_x") {
        ActionLookup::Dormant(id) => assert_eq!(id, "telegram"),
        other => panic!("expected Dormant, got {other:?}"),
    }
    assert!(reg.get_dormant("telegram").is_some());
}

#[test]
fn dormant_collision_with_live_provider_is_ambiguous() {
    let reg = PluginRegistry::new();
    reg.register_dormant(dormant("a", &["x"]));
    live(&reg, "b", 1, &["x"]);
    match reg.find_action_provider("x") {
        ActionLookup::Ambiguous(mut ids) => {
            ids.sort();
            assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn dormant_collision_between_dormant_plugins_is_ambiguous() {
    let reg = PluginRegistry::new();
    reg.register_dormant(dormant("a", &["x"]));
    reg.register_dormant(dormant("b", &["x"]));
    assert!(matches!(
        reg.find_action_provider("x"),
        ActionLookup::Ambiguous(_)
    ));
}

#[test]
fn re_registering_dormant_replaces_old_actions() {
    let reg = PluginRegistry::new();
    reg.register_dormant(dormant("a", &["old"]));
    reg.register_dormant(dormant("a", &["new"]));
    assert!(matches!(
        reg.find_action_provider("old"),
        ActionLookup::NotFound
    ));
    assert!(matches!(
        reg.find_action_provider("new"),
        ActionLookup::Dormant(_)
    ));
}

#[test]
fn plugin_def_lazy_defaults() {
    let def: PluginDef = serde_yaml::from_str("id: a\nbinary: /bin/true\n").unwrap();
    assert!(!def.lazy);
    assert_eq!(def.idle_stop_secs, None);
    let def: PluginDef =
        serde_yaml::from_str("id: a\nbinary: /bin/true\nlazy: true\nidle_stop_secs: 5\n").unwrap();
    assert!(def.lazy);
    assert_eq!(def.idle_stop_secs, Some(5));
}

#[test]
fn config_lazy_defaults() {
    let cfg: Config = serde_yaml::from_str("{}").unwrap();
    assert_eq!(cfg.lazy_idle_stop_secs, 600);
    assert_eq!(cfg.lazy_start_timeout_ms, 10_000);
    let cfg = Config::default();
    assert_eq!(cfg.lazy_idle_stop_secs, 600);
    assert_eq!(cfg.lazy_start_timeout_ms, 10_000);
}
