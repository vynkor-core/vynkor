use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::timeout;
use vynkor::events::bus::EventBus;
use vynkor::ipc::protocol::MessageRouter;
use vynkor::ipc::server::UdsServer;
use vynkor::plugins::registry::PluginRegistry;
use vynkor::proto::veyron::PluginManifest;
use vynkor_sdk::VeyronClient;

async fn start_kernel(socket_path: &'static str) {
    let registry = Arc::new(PluginRegistry::new());
    let event_bus = Arc::new(EventBus::new());
    let (router_tx, router_rx) = tokio::sync::mpsc::channel(64);
    let (_srv, _disc) = UdsServer::start(Path::new(socket_path), router_tx, 1024, 30, 64)
        .await
        .unwrap();
    tokio::spawn(MessageRouter::run(router_rx, registry, event_bus, None));
    // brief pause for socket to be ready
    tokio::time::sleep(Duration::from_millis(10)).await;
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn client_connects_to_kernel() {
    start_kernel("/tmp/veyron_sdk_connect.sock").await;
    let result = VeyronClient::connect("/tmp/veyron_sdk_connect.sock").await;
    assert!(result.is_ok(), "client must connect: {:?}", result.err());
}

#[tokio::test]
async fn client_registers_and_receives_accepted_ack() {
    start_kernel("/tmp/veyron_sdk_register.sock").await;
    let mut client = VeyronClient::connect("/tmp/veyron_sdk_register.sock")
        .await
        .unwrap();
    let ack = timeout(
        Duration::from_secs(2),
        client.register("sdk_plugin", PluginManifest::default()),
    )
    .await
    .expect("register timed out")
    .expect("register failed");

    assert!(ack.accepted, "ack must be accepted");
}

#[tokio::test]
async fn client_ping_returns_sub_second_duration() {
    start_kernel("/tmp/veyron_sdk_ping.sock").await;
    let mut client = VeyronClient::connect("/tmp/veyron_sdk_ping.sock")
        .await
        .unwrap();
    client
        .register("ping_plugin", PluginManifest::default())
        .await
        .unwrap();

    let rtt = timeout(Duration::from_secs(2), client.ping())
        .await
        .expect("ping timed out")
        .expect("ping failed");

    assert!(
        rtt < Duration::from_secs(1),
        "RTT must be < 1s, got {:?}",
        rtt
    );
}

#[tokio::test]
async fn client_subscribe_completes_without_error() {
    start_kernel("/tmp/veyron_sdk_sub.sock").await;
    let mut client = VeyronClient::connect("/tmp/veyron_sdk_sub.sock")
        .await
        .unwrap();
    client
        .register("sub_plugin", PluginManifest::default())
        .await
        .unwrap();

    let result = client
        .subscribe(vec!["alarm.fired".to_string(), "*".to_string()])
        .await;
    assert!(result.is_ok(), "subscribe must succeed: {:?}", result.err());
}

#[tokio::test]
async fn client_send_recv_roundtrip_via_broadcast() {
    start_kernel("/tmp/veyron_sdk_roundtrip.sock").await;

    // two clients: sender and receiver
    let mut sender = VeyronClient::connect("/tmp/veyron_sdk_roundtrip.sock")
        .await
        .unwrap();
    let mut receiver = VeyronClient::connect("/tmp/veyron_sdk_roundtrip.sock")
        .await
        .unwrap();

    // sender needs PERMISSION_IPC_SEND + ipc_targets listing the receiver
    sender
        .register(
            "sdk_sender",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["sdk_receiver".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    receiver
        .register("sdk_receiver", PluginManifest::default())
        .await
        .unwrap();

    use prost::Message;
    use vynkor::proto::veyron::{envelope, Envelope, Ping};
    let payload = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 42 })),
        ..Default::default()
    };
    let mut buf = vec![];
    payload.encode(&mut buf).unwrap();

    sender.send_raw("*", buf).await.unwrap();

    let received = timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    assert!(
        matches!(received.payload, Some(envelope::Payload::Ping(p)) if p.timestamp == 42),
        "unexpected payload: {:?}",
        received.payload
    );
}
