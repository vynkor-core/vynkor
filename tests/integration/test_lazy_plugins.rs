use futures_util::future::BoxFuture;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use vynkor::events::bus::EventBus;
use vynkor::ipc::protocol::MessageRouter;
use vynkor::ipc::server::UdsServer;
use vynkor::plugins::lazy::{LazyStarter, PluginStarter};
use vynkor::plugins::loader::PluginLoader;
use vynkor::plugins::manager::PluginManager;
use vynkor::plugins::registry::DormantPlugin;
use vynkor::plugins::registry::{ActionLookup, PluginRegistry};
use vynkor::plugins::supervisor::PluginSupervisor;
use vynkor::proto::vynkor::{
    envelope, ActionRequest, ActionResponse, ActionSpec, ActionStatus, Envelope, PermissionType,
    PluginManifest,
};
use vynkor::utils::config::PluginDef;
use vynkor::utils::errors::VynkorError;
use vynkor_sdk::VynkorClient;

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

// ── wake on first call ──────────────────────────────────────────────────────

const LAZY_ID: &str = "lazy-echo";

/// stands in for ManagerStarter: "spawning" connects an in-process client that
/// registers the plugin id and echoes action requests
struct TestStarter {
    sock: String,
    registers: bool,
    register_delay: Duration,
    starts: AtomicUsize,
    stops: AtomicUsize,
}

impl PluginStarter for TestStarter {
    fn start<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.registers {
                let sock = self.sock.clone();
                let id = plugin_id.to_string();
                let delay = self.register_delay;
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let mut c = VynkorClient::connect(&sock).await.unwrap();
                    c.register(
                        &id,
                        PluginManifest {
                            actions: vec!["lazy_echo".to_string()],
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                    while let Ok(env) = c.recv().await {
                        if let Some(envelope::Payload::ActionRequest(r)) = env.payload {
                            let resp = Envelope {
                                message_id: env.message_id,
                                payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                                    action_id: r.action_id,
                                    status: ActionStatus::ActionOk as i32,
                                    data_json: r.params_json,
                                    error: String::new(),
                                })),
                                ..Default::default()
                            };
                            c.send("kernel", resp).await.unwrap();
                        }
                    }
                });
            }
            Ok(())
        })
    }

    fn stop<'a>(&'a self, _plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>> {
        Box::pin(async move {
            self.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct Harness {
    sock: String,
    starter: Arc<TestStarter>,
    registry: Arc<PluginRegistry>,
    _dir: tempfile::TempDir,
}

/// uds server + router + LazyStarter wired by hand, with the lazy plugin
/// recorded dormant and `lazy_echo` requiring `required`
async fn harness(
    registers: bool,
    register_delay: Duration,
    start_timeout: Duration,
    required: Option<PermissionType>,
) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("k.sock").to_string_lossy().into_owned();
    let registry = Arc::new(PluginRegistry::new());
    registry.register_dormant(DormantPlugin {
        plugin_id: LAZY_ID.to_string(),
        action_specs: vec![ActionSpec {
            name: "lazy_echo".to_string(),
            ..Default::default()
        }],
        idle_stop: Duration::from_secs(300),
    });
    if let Some(perm) = required {
        registry.set_action_requirements(
            LAZY_ID.to_string(),
            [("lazy_echo".to_string(), perm)].into_iter().collect(),
        );
    }
    let starter = Arc::new(TestStarter {
        sock: sock.clone(),
        registers,
        register_delay,
        starts: AtomicUsize::new(0),
        stops: AtomicUsize::new(0),
    });

    let (router_tx, router_rx) = tokio::sync::mpsc::channel(64);
    let (lazy_tx, lazy_rx) = tokio::sync::mpsc::channel(64);
    UdsServer::start(std::path::Path::new(&sock), router_tx.clone(), 64, 30, 16)
        .await
        .unwrap();
    tokio::spawn(MessageRouter::run_with_context(
        router_rx,
        Arc::clone(&registry),
        Arc::new(EventBus::new()),
        None,
        Instant::now(),
        None,
        None,
        None,
        false,
        None,
        None,
        None,
        None,
        30_000,
        16,
        8192,
        None,
        60,
        None,
        None,
        None,
        Some(lazy_tx),
    ));
    let lazy = LazyStarter::new(
        Arc::clone(&registry),
        starter.clone(),
        router_tx,
        start_timeout,
    );
    tokio::spawn(lazy.run(lazy_rx));
    Harness {
        sock,
        starter,
        registry,
        _dir: dir,
    }
}

async fn caller(h: &Harness) -> VynkorClient {
    let mut c = VynkorClient::connect(&h.sock).await.unwrap();
    c.register("caller", PluginManifest::default())
        .await
        .unwrap();
    c
}

async fn call(c: &mut VynkorClient, action_id: &str, params: &str) {
    let req = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: action_id.to_string(),
            action: "lazy_echo".to_string(),
            params_json: params.as_bytes().to_vec(),
            ..Default::default()
        })),
        ..Default::default()
    };
    c.send("kernel", req).await.unwrap();
}
type Reply = ActionResponse;

async fn reply(c: &mut VynkorClient) -> Reply {
    let env = c.recv_timeout(Duration::from_secs(5)).await.unwrap();
    match env.payload {
        Some(envelope::Payload::ActionResponse(r)) => r,
        other => panic!("expected ActionResponse, got {other:?}"),
    }
}

#[tokio::test]
async fn first_call_wakes_dormant_plugin() {
    let h = harness(true, Duration::ZERO, Duration::from_secs(5), None).await;
    let mut c = caller(&h).await;
    call(&mut c, "a1", "{\"x\":1}").await;

    let r = reply(&mut c).await;
    assert_eq!(r.action_id, "a1");
    assert_eq!(
        r.status,
        ActionStatus::ActionOk as i32,
        "error: {}",
        r.error
    );
    assert_eq!(r.data_json, b"{\"x\":1}");
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn concurrent_first_calls_spawn_once() {
    // registration lags so every call is queued while the plugin is waking
    let h = harness(
        true,
        Duration::from_millis(300),
        Duration::from_secs(5),
        None,
    )
    .await;
    let mut c = caller(&h).await;
    for i in 0..5 {
        call(&mut c, &format!("a{i}"), &format!("{{\"n\":{i}}}")).await;
    }

    for i in 0..5 {
        let r = reply(&mut c).await;
        assert_eq!(r.action_id, format!("a{i}"), "replies out of order");
        assert_eq!(
            r.status,
            ActionStatus::ActionOk as i32,
            "error: {}",
            r.error
        );
        assert_eq!(r.data_json, format!("{{\"n\":{i}}}").into_bytes());
    }
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn wake_timeout_fails_every_queued_call() {
    let h = harness(false, Duration::ZERO, Duration::from_millis(300), None).await;
    let mut c = caller(&h).await;
    let t0 = Instant::now();
    for i in 0..3 {
        call(&mut c, &format!("a{i}"), "{}").await;
    }

    for i in 0..3 {
        let r = reply(&mut c).await;
        assert_eq!(r.action_id, format!("a{i}"));
        assert_eq!(r.status, ActionStatus::ActionError as i32);
        assert_eq!(r.error, format!("lazy plugin {LAZY_ID} failed to start"));
    }
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "failed after {:?}",
        t0.elapsed()
    );
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 1);
    // the half-started plugin is stopped; stop runs after the replies
    let deadline = Instant::now() + Duration::from_secs(5);
    while h.starter.stops.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.starter.stops.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn denied_caller_does_not_wake() {
    let h = harness(
        true,
        Duration::ZERO,
        Duration::from_secs(5),
        Some(PermissionType::PermissionNetwork),
    )
    .await;
    let mut c = caller(&h).await;
    call(&mut c, "a1", "{}").await;

    let r = reply(&mut c).await;
    assert_eq!(r.status, ActionStatus::ActionPermissionDeny as i32);
    // reply is sent by the router before any wake could be queued, so a
    // spawn would already be visible once the router has moved on; ping
    // round-trips through the same router task as a barrier
    c.ping().await.unwrap();
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 0);
    assert!(h.registry.get(LAZY_ID).is_none());
}
