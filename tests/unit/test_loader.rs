use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use vynkor::plugins::loader::{topo_sort, PluginLoader};
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::utils::config::PluginDef;

fn make_manager(socket: &str) -> Arc<PluginManager> {
    let sup = Arc::new(PluginSupervisor::new(socket));
    let reg = Arc::new(PluginRegistry::new());
    Arc::new(PluginManager::new(sup, reg))
}

fn sleep_def(id: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: "/bin/sleep".to_string(),
        restart: "never".to_string(),
        max_restarts: 0,
        args: vec!["60".to_string()],
        env: vec![],
        sandbox: false,
        grace_seconds: 5,
        max_procs: None,
        max_vmem_mb: None,
        permissions: vec![],
        ..Default::default()
    }
}

fn bad_binary_def(id: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: "/nonexistent/binary".to_string(),
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

#[tokio::test]
async fn load_all_empty_list_spawns_nothing() {
    let manager = make_manager("/tmp/veyron_loader_empty.sock");
    PluginLoader::load_all(&[], &manager, None).await;
    assert_eq!(manager.list().len(), 0, "empty list must spawn nothing");
}

#[tokio::test]
async fn load_all_spawns_valid_plugin() {
    let manager = make_manager("/tmp/veyron_loader_spawn.sock");
    PluginLoader::load_all(&[sleep_def("autoload-sleep")], &manager, None).await;
    sleep(Duration::from_millis(50)).await;
    assert!(
        manager.is_supervised("autoload-sleep"),
        "plugin declared in config must be supervised after load_all"
    );
    manager.stop("autoload-sleep").await.ok();
}

#[tokio::test]
async fn load_all_bad_binary_does_not_panic_and_skips() {
    let manager = make_manager("/tmp/veyron_loader_bad.sock");
    // Must not panic; must log a warning and skip
    PluginLoader::load_all(&[bad_binary_def("bad-plugin")], &manager, None).await;
    assert!(
        !manager.is_supervised("bad-plugin"),
        "failed spawn must not appear as supervised"
    );
}

#[tokio::test]
async fn load_all_continues_after_bad_binary() {
    let manager = make_manager("/tmp/veyron_loader_continue.sock");
    let defs = vec![bad_binary_def("bad-first"), sleep_def("good-second")];
    PluginLoader::load_all(&defs, &manager, None).await;
    sleep(Duration::from_millis(50)).await;
    assert!(
        manager.is_supervised("good-second"),
        "valid plugin after a bad one must still be spawned"
    );
    manager.stop("good-second").await.ok();
}

#[tokio::test]
async fn load_all_spawns_multiple_plugins() {
    let manager = make_manager("/tmp/veyron_loader_multi.sock");
    let defs = vec![sleep_def("multi-a"), sleep_def("multi-b")];
    PluginLoader::load_all(&defs, &manager, None).await;
    sleep(Duration::from_millis(50)).await;
    assert!(
        manager.is_supervised("multi-a"),
        "multi-a must be supervised"
    );
    assert!(
        manager.is_supervised("multi-b"),
        "multi-b must be supervised"
    );
    manager.stop("multi-a").await.ok();
    manager.stop("multi-b").await.ok();
}

#[test]
fn plugin_def_restart_policy_defaults_to_on_failure() {
    // Config default: restart = "on-failure"
    let yaml = r#"
id: test-plugin
binary: /bin/sleep
"#;
    let def: PluginDef = serde_yaml::from_str(yaml).expect("parse must succeed");
    assert_eq!(def.restart, "on-failure");
    assert_eq!(def.max_restarts, 5);
    assert!(def.args.is_empty());
    assert!(def.env.is_empty());
    assert!(!def.sandbox);
}

#[test]
fn config_yaml_plugins_section_parses_correctly() {
    let yaml = r#"
port: 8000
log_level: info
pid_file: /tmp/t.pid
log_file: /tmp/t.log
data_dir: /tmp/t
allow_no_auth: true
plugins:
  - id: echo
    binary: /usr/bin/echo
    restart: always
    max_restarts: 3
    args:
      - hello
    env:
      - FOO=bar
    sandbox: false
  - id: sleep-worker
    binary: /bin/sleep
    args:
      - "30"
"#;
    let cfg: vynkor::utils::config::Config = serde_yaml::from_str(yaml).expect("config must parse");
    assert_eq!(cfg.plugins.len(), 2);

    let echo = &cfg.plugins[0];
    assert_eq!(echo.id, "echo");
    assert_eq!(echo.binary, "/usr/bin/echo");
    assert_eq!(echo.restart, "always");
    assert_eq!(echo.max_restarts, 3);
    assert_eq!(echo.args, vec!["hello"]);
    assert_eq!(echo.env, vec!["FOO=bar"]);
    assert!(!echo.sandbox);

    let worker = &cfg.plugins[1];
    assert_eq!(worker.id, "sleep-worker");
    assert_eq!(worker.restart, "on-failure"); // default
    assert_eq!(worker.max_restarts, 5); // default
    assert_eq!(worker.args, vec!["30"]);
}

#[test]
fn config_yaml_without_plugins_section_defaults_to_empty() {
    let yaml = r#"
port: 8000
log_level: info
pid_file: /tmp/t.pid
log_file: /tmp/t.log
data_dir: /tmp/t
allow_no_auth: true
"#;
    let cfg: vynkor::utils::config::Config = serde_yaml::from_str(yaml).expect("config must parse");
    assert!(
        cfg.plugins.is_empty(),
        "missing plugins: key must default to empty list"
    );
}

// ── T-05: dependency ordering ─────────────────────────────────────────────

fn mk_def(id: &str) -> PluginDef {
    PluginDef {
        id: id.to_string(),
        binary: format!("/bin/{id}"),
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

#[test]
fn topo_sort_no_deps_preserves_order() {
    let defs = vec![mk_def("a"), mk_def("b"), mk_def("c")];
    let deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let refused: HashSet<&str> = HashSet::new();
    let order = topo_sort(&defs, &deps, &refused).expect("no cycle");
    // All three present, no requirement on relative order beyond completeness.
    assert_eq!(order.len(), 3);
    assert!(order.contains(&"a"));
    assert!(order.contains(&"b"));
    assert!(order.contains(&"c"));
}

#[test]
fn topo_sort_dep_before_dependant() {
    // b requires a → a must appear before b
    let defs = vec![mk_def("b"), mk_def("a")]; // declared: b first
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    deps.insert("b", vec!["a"]);
    let refused: HashSet<&str> = HashSet::new();
    let order = topo_sort(&defs, &deps, &refused).expect("no cycle");
    assert_eq!(order.len(), 2);
    let pos_a = order.iter().position(|&x| x == "a").unwrap();
    let pos_b = order.iter().position(|&x| x == "b").unwrap();
    assert!(pos_a < pos_b, "dep 'a' must load before dependant 'b'");
}

#[test]
fn topo_sort_chain_ordered() {
    // c requires b, b requires a → order: a, b, c
    let defs = vec![mk_def("c"), mk_def("b"), mk_def("a")];
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    deps.insert("c", vec!["b"]);
    deps.insert("b", vec!["a"]);
    let refused: HashSet<&str> = HashSet::new();
    let order = topo_sort(&defs, &deps, &refused).expect("no cycle");
    assert_eq!(order.len(), 3);
    let pos = |id: &str| order.iter().position(|&x| x == id).unwrap();
    assert!(pos("a") < pos("b"), "a before b");
    assert!(pos("b") < pos("c"), "b before c");
}

#[test]
fn topo_sort_detects_simple_cycle() {
    // a requires b, b requires a → cycle
    let defs = vec![mk_def("a"), mk_def("b")];
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    deps.insert("a", vec!["b"]);
    deps.insert("b", vec!["a"]);
    let refused: HashSet<&str> = HashSet::new();
    let cycle = topo_sort(&defs, &deps, &refused).expect_err("must detect cycle");
    assert!(
        cycle.contains(&"a") && cycle.contains(&"b"),
        "both in cycle"
    );
}

#[test]
fn topo_sort_detects_three_node_cycle() {
    // a→b→c→a
    let defs = vec![mk_def("a"), mk_def("b"), mk_def("c")];
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    deps.insert("a", vec!["b"]);
    deps.insert("b", vec!["c"]);
    deps.insert("c", vec!["a"]);
    let refused: HashSet<&str> = HashSet::new();
    let cycle = topo_sort(&defs, &deps, &refused).expect_err("must detect cycle");
    assert_eq!(cycle.len(), 3);
}

#[test]
fn topo_sort_refused_nodes_excluded() {
    // a, b, c — b refused. Result should contain a and c only.
    let defs = vec![mk_def("a"), mk_def("b"), mk_def("c")];
    let deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut refused: HashSet<&str> = HashSet::new();
    refused.insert("b");
    let order = topo_sort(&defs, &deps, &refused).expect("no cycle");
    assert_eq!(order.len(), 2);
    assert!(
        !order.contains(&"b"),
        "refused node must not appear in order"
    );
}
