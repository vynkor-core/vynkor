//! K3: KernelCommand authorization for device connections uses the grants in
//! the device's record, re-read on every command.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

use prost::Message as _;
use vynkor::auth::device_store::DeviceStore;
use vynkor::auth::grants::{Grant, Preset};
use vynkor::events::bus::EventBus;
use vynkor::ipc::connection::Outbound;
use vynkor::ipc::framing::Frame;
use vynkor::ipc::messages::IncomingMessage;
use vynkor::ipc::protocol::MessageRouter;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::proto::vynkor::{
    envelope, CommandStatus, Envelope, KernelCommand, PluginManifest, PluginRegister,
};

const MASTER: &str = "k3-master-secret-0123456789abcdef";

fn kernel_frame(env: Envelope) -> Frame {
    let mut payload = Vec::new();
    env.encode(&mut payload).unwrap();
    let crc = crc32fast::hash(&payload);
    let mut t = [0u8; 32];
    t[..6].copy_from_slice(b"kernel");
    Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: t,
        crc32: crc,
        payload: payload.into(),
        mac: None,
    }
}

struct Fx {
    router_tx: mpsc::Sender<IncomingMessage>,
    store: Arc<DeviceStore>,
    _dir: tempfile::TempDir,
}

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DeviceStore::new(dir.path(), MASTER));
    let (tx, rx) = mpsc::channel::<IncomingMessage>(16);
    tokio::spawn(MessageRouter::run_with_context(
        rx,
        Arc::new(PluginRegistry::new()),
        Arc::new(EventBus::new()),
        None,
        std::time::Instant::now(),
        None,
        None,
        None, // mac_secret
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
        Some(Arc::clone(&store)),
        None,
        None,
        None,
    ));
    Fx {
        router_tx: tx,
        store,
        _dir: dir,
    }
}

struct Conn {
    conn_id: u64,
    write_tx: mpsc::Sender<Outbound>,
    rx: mpsc::Receiver<Outbound>,
}

async fn send(fx: &Fx, c: &Conn, env: Envelope) {
    fx.router_tx
        .send(IncomingMessage {
            conn_id: c.conn_id,
            frame: kernel_frame(env),
            write_tx: c.write_tx.clone(),
            session_key: Arc::new(Mutex::new(None)),
        })
        .await
        .unwrap();
}

async fn next_envelope(c: &mut Conn) -> Envelope {
    let item = timeout(Duration::from_secs(2), c.rx.recv())
        .await
        .expect("recv timed out")
        .expect("channel closed");
    match item {
        Outbound::Frame(f) => Envelope::decode(f.payload.as_ref()).unwrap(),
        _ => panic!("expected frame"),
    }
}

/// Register a connection. `device_id` empty = a plain local plugin. The
/// manifest claims KERNEL_ADMIN so tests prove it confers nothing on devices.
async fn register(fx: &Fx, conn_id: u64, plugin_id: &str, device_id: &str, admin: bool) -> Conn {
    let (write_tx, rx) = mpsc::channel::<Outbound>(16);
    let mut c = Conn {
        conn_id,
        write_tx,
        rx,
    };
    let manifest = PluginManifest {
        permissions: if admin {
            vec!["PERMISSION_KERNEL_ADMIN".to_string()]
        } else {
            vec![]
        },
        ..Default::default()
    };
    let env = Envelope {
        payload: Some(envelope::Payload::PluginRegister(PluginRegister {
            plugin_id: plugin_id.to_string(),
            manifest: Some(manifest),
            device_id: device_id.to_string(),
            ..Default::default()
        })),
        ..Default::default()
    };
    send(fx, &c, env).await;
    let ack = next_envelope(&mut c).await;
    assert!(
        matches!(ack.payload, Some(envelope::Payload::PluginRegisterAck(_))),
        "expected register ack, got {ack:?}"
    );
    c
}

async fn command(fx: &Fx, c: &mut Conn, name: &str) -> CommandStatus {
    let env = Envelope {
        payload: Some(envelope::Payload::KernelCommand(KernelCommand {
            command_id: format!("c-{name}"),
            command: name.to_string(),
            params_json: b"{}".to_vec(),
        })),
        ..Default::default()
    };
    send(fx, c, env).await;
    match next_envelope(c).await.payload {
        Some(envelope::Payload::KernelCommandAck(a)) => {
            CommandStatus::try_from(a.status).unwrap()
        }
        other => panic!("expected ack, got {other:?}"),
    }
}

#[tokio::test]
async fn device_kernel_command_respects_grants() {
    let fx = fx();
    fx.store
        .issue("guest-dev", "g", 3600, Some(&Preset::Guest.grants()))
        .unwrap();
    fx.store
        .issue("user-dev", "u", 3600, Some(&Preset::User.grants()))
        .unwrap();
    // admin claim in the manifest must not help a device
    let mut guest = register(&fx, 1, "guest-dev", "guest-dev", true).await;
    let mut user = register(&fx, 2, "user-dev", "user-dev", true).await;

    assert_eq!(
        command(&fx, &mut guest, "health_check").await,
        CommandStatus::CommandOk
    );
    assert_eq!(
        command(&fx, &mut guest, "list_plugins").await,
        CommandStatus::CommandPermissionDenied
    );
    assert_eq!(
        command(&fx, &mut user, "list_plugins").await,
        CommandStatus::CommandOk
    );
    assert_eq!(
        command(&fx, &mut user, "list_devices").await,
        CommandStatus::CommandPermissionDenied
    );
    assert_eq!(
        command(&fx, &mut user, "reload_config").await,
        CommandStatus::CommandPermissionDenied
    );
}

#[tokio::test]
async fn device_with_devices_manage_can_list_devices() {
    let fx = fx();
    let mut g = Preset::Guest.grants();
    g.insert(Grant::DevicesManage);
    fx.store.issue("mgr", "m", 3600, Some(&g)).unwrap();
    let mut c = register(&fx, 1, "mgr", "mgr", false).await;
    assert_eq!(
        command(&fx, &mut c, "list_devices").await,
        CommandStatus::CommandOk
    );
}

#[tokio::test]
async fn device_command_after_grant_revoked_is_denied() {
    let fx = fx();
    fx.store
        .issue("user-dev", "u", 3600, Some(&Preset::User.grants()))
        .unwrap();
    let mut c = register(&fx, 1, "user-dev", "user-dev", false).await;
    assert_eq!(
        command(&fx, &mut c, "list_plugins").await,
        CommandStatus::CommandOk
    );
    // same connection, grants changed from another process
    fx.store
        .set_grants("user-dev", &Preset::Guest.grants())
        .unwrap();
    assert_eq!(
        command(&fx, &mut c, "list_plugins").await,
        CommandStatus::CommandPermissionDenied
    );
}

#[tokio::test]
async fn plugin_connection_kernel_command_unchanged() {
    let fx = fx();
    let mut admin = register(&fx, 1, "p-admin", "", true).await;
    let mut plain = register(&fx, 2, "p-plain", "", false).await;
    assert_eq!(
        command(&fx, &mut admin, "list_devices").await,
        CommandStatus::CommandOk
    );
    assert_eq!(
        command(&fx, &mut plain, "list_devices").await,
        CommandStatus::CommandPermissionDenied
    );
    // read-only discovery stays open to any plugin
    assert_eq!(
        command(&fx, &mut plain, "list_plugins").await,
        CommandStatus::CommandOk
    );
}
