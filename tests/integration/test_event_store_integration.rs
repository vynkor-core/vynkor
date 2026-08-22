use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::time::timeout;
use vynkor::events::bus::EventBus;
use vynkor::events::store::EventStore;
use vynkor::kernel::Kernel;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::proto::vynkor::{envelope, Envelope, EventAck, PluginManifest};
use vynkor::utils::config::Config;
use vynkor_sdk::VynkorClient;

fn store_config(socket: &str, port: u16, data_dir: &Path) -> Config {
    Config {
        socket_path: socket.to_string(),
        port,
        pid_file: "/tmp/vynkor_integ_es.pid".into(),
        log_file: "/tmp/vynkor_integ_es.log".into(),
        allow_no_auth: true,
        data_dir: data_dir.to_path_buf(),
        ..Config::default()
    }
}

async fn start_kernel_with_store(socket: &str, port: u16, data_dir: &Path) -> oneshot::Sender<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    let cfg = store_config(socket, port, data_dir);
    let registry = Arc::new(PluginRegistry::new());
    let event_bus = Arc::new(EventBus::new());

    tokio::spawn(async move {
        Kernel::run_with_components(cfg, registry, event_bus, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });

    tokio::time::sleep(Duration::from_millis(40)).await;
    shutdown_tx
}

fn tmp_data_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vynkor_es_integ_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // S2: set 0o700 so the ownership check in EventStore::new passes
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}

// ── Integration: kernel system event is persisted to the store as pending ────
//
// system.plugin_joined events are published through the kernel's internal store-
// backed EventBus, so they must appear in the EventStore as pending until acked.

#[tokio::test]
async fn kernel_system_event_is_persisted_to_store_as_pending() {
    let data_dir = tmp_data_dir("persist_pending");
    let shutdown_tx =
        start_kernel_with_store("/tmp/vynkor_es_persist.sock", 19300, &data_dir).await;

    let mut joiner = VynkorClient::connect("/tmp/vynkor_es_persist.sock")
        .await
        .unwrap();
    joiner
        .register("joiner_persist", PluginManifest::default())
        .await
        .unwrap();

    // Give the kernel time to persist the system.plugin_joined event.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // Open a second EventStore handle on the same DB file to inspect state.
    let inspector = EventStore::new(&data_dir).expect("inspector store must open");
    let pending = inspector.pending_older_than(0);
    let ids: Vec<&str> = pending.iter().map(|e| e.event_id.as_str()).collect();
    assert!(
        ids.iter().any(|id| id.contains("joiner_persist")),
        "system.plugin_joined for joiner_persist must be pending in store; got: {ids:?}"
    );

    let _ = shutdown_tx.send(());
}

// ── Integration: EventAck sent by plugin marks the event as delivered ─────────
//
// Plugin subscribes to *, receives system.plugin_joined, sends EventAck back.
// Kernel's MessageRouter calls store.mark_delivered() on receipt of the Ack.

#[tokio::test]
async fn event_ack_from_plugin_marks_event_delivered() {
    let data_dir = tmp_data_dir("ack_delivered");
    let shutdown_tx = start_kernel_with_store("/tmp/vynkor_es_ack.sock", 19301, &data_dir).await;

    // observer subscribes to all events so it receives system.plugin_joined
    let mut observer = VynkorClient::connect("/tmp/vynkor_es_ack.sock")
        .await
        .unwrap();
    observer
        .register("observer_ack", PluginManifest::default())
        .await
        .unwrap();
    observer.subscribe(vec!["*".to_string()]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;

    // acker registers → kernel publishes + persists system.plugin_joined for it
    let mut acker = VynkorClient::connect("/tmp/vynkor_es_ack.sock")
        .await
        .unwrap();
    acker
        .register("acker_plugin", PluginManifest::default())
        .await
        .unwrap();

    // observer receives the system.plugin_joined event
    let received = timeout(Duration::from_secs(2), observer.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    let event_id = match &received.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "system.plugin_joined");
            e.event_id.clone()
        }
        other => panic!("expected system.plugin_joined, got: {other:?}"),
    };

    // send EventAck to kernel
    observer
        .send(
            "kernel",
            Envelope {
                payload: Some(envelope::Payload::EventAck(EventAck {
                    event_id: event_id.clone(),
                })),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // give kernel time to process the ack
    tokio::time::sleep(Duration::from_millis(60)).await;

    let inspector = EventStore::new(&data_dir).expect("inspector store must open");
    let pending = inspector.pending_older_than(0);
    let still_pending = pending.iter().any(|e| e.event_id == event_id);
    assert!(
        !still_pending,
        "acked event '{event_id}' must not remain pending; pending: {:?}",
        pending.iter().map(|e| &e.event_id).collect::<Vec<_>>()
    );

    let _ = shutdown_tx.send(());
}
