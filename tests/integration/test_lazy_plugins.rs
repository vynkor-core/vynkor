use futures_util::future::BoxFuture;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
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

#[derive(Clone)]
struct Opts {
    /// the started plugin connects and registers; false = never registers
    registers: bool,
    /// ... after this long
    register_delay: Duration,
    /// the registered manifest declares `lazy_echo`
    declares_action: bool,
    /// permission the plugin holds once registered
    provider_perm: Option<&'static str>,
    start_timeout: Duration,
    poll_interval: Option<Duration>,
    /// how long `stop` takes
    stop_delay: Duration,
    /// permission `lazy_echo` requires of callers
    required: Option<PermissionType>,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            registers: true,
            register_delay: Duration::ZERO,
            declares_action: true,
            provider_perm: None,
            start_timeout: Duration::from_secs(5),
            poll_interval: None,
            stop_delay: Duration::ZERO,
            required: None,
        }
    }
}

/// stands in for ManagerStarter: "spawning" connects an in-process client that
/// registers the plugin id and echoes action requests
struct TestStarter {
    sock: String,
    opts: Opts,
    starts: AtomicUsize,
    stops: AtomicUsize,
    in_stop: AtomicBool,
    /// a start began while a stop was still running
    overlapped: AtomicBool,
    /// drops the running plugin's connection (a crash / exit)
    kill: Arc<Notify>,
}

impl PluginStarter for TestStarter {
    fn start<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if self.in_stop.load(Ordering::SeqCst) {
                self.overlapped.store(true, Ordering::SeqCst);
            }
            if self.opts.registers {
                let sock = self.sock.clone();
                let id = plugin_id.to_string();
                let opts = self.opts.clone();
                let kill = Arc::clone(&self.kill);
                tokio::spawn(async move {
                    tokio::time::sleep(opts.register_delay).await;
                    let mut c = VynkorClient::connect(&sock).await.unwrap();
                    c.register(
                        &id,
                        PluginManifest {
                            actions: if opts.declares_action {
                                vec!["lazy_echo".to_string()]
                            } else {
                                vec![]
                            },
                            permissions: opts.provider_perm.iter().map(|p| p.to_string()).collect(),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                    loop {
                        let env = tokio::select! {
                            e = c.recv() => match e { Ok(e) => e, Err(_) => break },
                            _ = kill.notified() => break,
                        };
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
            self.in_stop.store(true, Ordering::SeqCst);
            tokio::time::sleep(self.opts.stop_delay).await;
            self.stops.fetch_add(1, Ordering::SeqCst);
            self.in_stop.store(false, Ordering::SeqCst);
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
/// recorded dormant
async fn harness(opts: Opts) -> Harness {
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
    if let Some(perm) = opts.required {
        registry.set_action_requirements(
            LAZY_ID.to_string(),
            [("lazy_echo".to_string(), perm)].into_iter().collect(),
        );
    }
    let starter = Arc::new(TestStarter {
        sock: sock.clone(),
        opts: opts.clone(),
        starts: AtomicUsize::new(0),
        stops: AtomicUsize::new(0),
        in_stop: AtomicBool::new(false),
        overlapped: AtomicBool::new(false),
        kill: Arc::new(Notify::new()),
    });

    let (router_tx, router_rx) = tokio::sync::mpsc::channel(256);
    let (lazy_tx, lazy_rx) = tokio::sync::mpsc::channel(256);
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
    let mut lazy = LazyStarter::new(
        Arc::clone(&registry),
        starter.clone(),
        router_tx,
        opts.start_timeout,
    );
    if let Some(p) = opts.poll_interval {
        lazy = lazy.with_poll_interval(p);
    }
    tokio::spawn(lazy.run(lazy_rx));
    Harness {
        sock,
        starter,
        registry,
        _dir: dir,
    }
}

async fn caller_as(h: &Harness, id: &str, perms: &[&str]) -> VynkorClient {
    let mut c = VynkorClient::connect(&h.sock).await.unwrap();
    c.register(
        id,
        PluginManifest {
            permissions: perms.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    c
}

async fn caller(h: &Harness) -> VynkorClient {
    caller_as(h, "caller", &[]).await
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

async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn first_call_wakes_dormant_plugin() {
    let h = harness(Opts::default()).await;
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
    let h = harness(Opts {
        register_delay: Duration::from_millis(300),
        ..Opts::default()
    })
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
    let h = harness(Opts {
        registers: false,
        start_timeout: Duration::from_millis(300),
        ..Opts::default()
    })
    .await;
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
    wait_until("stop", || h.starter.stops.load(Ordering::SeqCst) == 1).await;
}

#[tokio::test]
async fn denied_caller_does_not_wake() {
    let h = harness(Opts {
        required: Some(PermissionType::PermissionNetwork),
        ..Opts::default()
    })
    .await;
    let mut c = caller(&h).await;
    call(&mut c, "a1", "{}").await;

    let r = reply(&mut c).await;
    assert_eq!(r.status, ActionStatus::ActionPermissionDeny as i32);
    // ping round-trips through the same router task as a barrier
    c.ping().await.unwrap();
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 0);
    assert!(h.registry.get(LAZY_ID).is_none());
}

#[tokio::test]
async fn requirement_survives_plugin_exit() {
    // pre-existing bug fixed alongside: unregister cleared the requirement,
    // so after a woken plugin exited an unprivileged caller could wake it
    let h = harness(Opts {
        required: Some(PermissionType::PermissionNetwork),
        provider_perm: Some("PERMISSION_NETWORK"),
        ..Opts::default()
    })
    .await;
    let mut allowed = caller_as(&h, "allowed", &["PERMISSION_NETWORK"]).await;
    call(&mut allowed, "a1", "{}").await;
    let r = reply(&mut allowed).await;
    assert_eq!(
        r.status,
        ActionStatus::ActionOk as i32,
        "error: {}",
        r.error
    );

    // the plugin's connection drops and its registration goes with it
    h.starter.kill.notify_one();
    wait_until("plugin unregistered", || {
        h.registry.unregister_if_dead(LAZY_ID);
        h.registry.get(LAZY_ID).is_none()
    })
    .await;

    let mut denied = caller(&h).await;
    call(&mut denied, "a2", "{}").await;
    let r = reply(&mut denied).await;
    assert_eq!(r.status, ActionStatus::ActionPermissionDeny as i32);
    denied.ping().await.unwrap();
    assert_eq!(
        h.starter.starts.load(Ordering::SeqCst),
        1,
        "denied caller woke it"
    );
}

#[tokio::test]
async fn plugin_registering_without_the_action_answers_error_not_loop() {
    let h = harness(Opts {
        declares_action: false,
        ..Opts::default()
    })
    .await;
    let mut c = caller(&h).await;
    call(&mut c, "a1", "{}").await;

    let r = reply(&mut c).await;
    assert_eq!(r.action_id, "a1");
    assert_eq!(r.status, ActionStatus::ActionNotFound as i32);
    assert!(r.error.contains("does not provide"), "error: {}", r.error);
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn call_after_registration_does_not_overtake_queued_calls() {
    // slow poll keeps the queue undrained after the plugin has registered
    let h = harness(Opts {
        register_delay: Duration::from_millis(100),
        poll_interval: Some(Duration::from_millis(600)),
        ..Opts::default()
    })
    .await;
    let mut c = caller(&h).await;
    for i in 0..3 {
        call(&mut c, &format!("q{i}"), "{}").await;
    }
    wait_until("plugin registered", || h.registry.get(LAZY_ID).is_some()).await;
    call(&mut c, "late", "{}").await;

    for want in ["q0", "q1", "q2", "late"] {
        let r = reply(&mut c).await;
        assert_eq!(r.action_id, want, "replies out of order");
        assert_eq!(
            r.status,
            ActionStatus::ActionOk as i32,
            "error: {}",
            r.error
        );
    }
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn call_during_stop_waits_for_the_stop() {
    let h = harness(Opts {
        registers: false,
        start_timeout: Duration::from_millis(200),
        stop_delay: Duration::from_millis(400),
        ..Opts::default()
    })
    .await;
    let mut c = caller(&h).await;
    call(&mut c, "a1", "{}").await;
    let r = reply(&mut c).await;
    assert_eq!(r.error, format!("lazy plugin {LAZY_ID} failed to start"));

    // the stop of the first attempt is still running
    call(&mut c, "a2", "{}").await;
    let r = reply(&mut c).await;
    assert_eq!(r.action_id, "a2");
    assert_eq!(r.error, format!("lazy plugin {LAZY_ID} failed to start"));
    assert_eq!(h.starter.starts.load(Ordering::SeqCst), 2);
    assert!(
        !h.starter.overlapped.load(Ordering::SeqCst),
        "start ran while a stop was in flight"
    );
}

#[tokio::test]
async fn wake_queue_is_capped() {
    let h = harness(Opts {
        registers: false,
        start_timeout: Duration::from_secs(2),
        ..Opts::default()
    })
    .await;
    let mut c = caller(&h).await;
    for i in 0..66 {
        call(&mut c, &format!("a{i}"), "{}").await;
    }

    // overflow is answered at once, ahead of the timeout failures
    let r = reply(&mut c).await;
    assert_eq!(r.action_id, "a64");
    assert_eq!(r.status, ActionStatus::ActionError as i32);
    assert_eq!(r.error, "lazy start queue full");
}
