use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{envelope, Event, PluginManifest};
use veyron_sdk::VeyronClient;

#[tokio::test]
async fn subscribed_plugin_receives_event_via_event_bus() {
    let (shutdown_tx, registry, event_bus) =
        start_kernel("/tmp/veyron_integ_events.sock", 19202).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_events.sock")
        .await
        .unwrap();
    client
        .register("event_watcher", PluginManifest::default())
        .await
        .unwrap();

    // subscribe via SDK (routes through router to event_bus)
    client
        .subscribe(vec!["alarm.fired".to_string()])
        .await
        .unwrap();

    // wait for subscribe to propagate to event_bus
    tokio::time::sleep(Duration::from_millis(30)).await;

    // publish via the shared event_bus
    event_bus
        .publish(
            Event {
                event_id: "ev-1".to_string(),
                event_type: "alarm.fired".to_string(),
                payload_json: b"{}".to_vec(),
                retry_count: 0,
            },
            &registry,
        )
        .await;

    let received = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    assert!(
        matches!(
            received.payload,
            Some(envelope::Payload::Event(ref e)) if e.event_type == "alarm.fired"
        ),
        "expected alarm.fired event, got: {:?}",
        received.payload
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn wildcard_subscriber_receives_system_plugin_joined() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_joined.sock", 19203).await;

    let mut observer = VeyronClient::connect("/tmp/veyron_integ_joined.sock")
        .await
        .unwrap();
    observer
        .register("observer", PluginManifest::default())
        .await
        .unwrap();
    observer.subscribe(vec!["*".to_string()]).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    // newcomer registers — triggers system.plugin_joined
    let mut newcomer = VeyronClient::connect("/tmp/veyron_integ_joined.sock")
        .await
        .unwrap();
    newcomer
        .register("newcomer", PluginManifest::default())
        .await
        .unwrap();

    let received = timeout(Duration::from_secs(2), observer.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match received.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "system.plugin_joined");
        }
        other => panic!("expected system.plugin_joined, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn publish_without_permission_is_denied() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_evpub_denied.sock", 19700).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_evpub_denied.sock")
        .await
        .unwrap();
    client
        .register("evpub-unprivileged", PluginManifest::default())
        .await
        .unwrap();

    let env = veyron::proto::veyron::Envelope {
        payload: Some(envelope::Payload::EventPublish(
            veyron::proto::veyron::EventPublish {
                event_type: "request_completed".to_string(),
                payload_json: b"{}".to_vec(),
            },
        )),
        ..Default::default()
    };
    client.send("kernel", env).await.unwrap();

    let received = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match received.payload {
        Some(envelope::Payload::EventPublishAck(ack)) => {
            assert_eq!(
                ack.status,
                veyron::proto::veyron::EventPublishStatus::EventPublishPermissionDeny as i32
            );
        }
        other => panic!("expected EventPublishAck, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn publish_with_permission_namespaces_and_delivers_to_subscriber() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_evpub_ok.sock", 19701).await;

    let mut publisher = VeyronClient::connect("/tmp/veyron_integ_evpub_ok.sock")
        .await
        .unwrap();
    publisher
        .register(
            "network",
            PluginManifest {
                permissions: vec!["PERMISSION_EVENT_PUBLISH".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut subscriber = VeyronClient::connect("/tmp/veyron_integ_evpub_ok.sock")
        .await
        .unwrap();
    subscriber
        .register("evpub-subscriber", PluginManifest::default())
        .await
        .unwrap();
    subscriber
        .subscribe(vec!["plugin.network.request_completed".to_string()])
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(30)).await;

    let env = veyron::proto::veyron::Envelope {
        payload: Some(envelope::Payload::EventPublish(
            veyron::proto::veyron::EventPublish {
                event_type: "request_completed".to_string(),
                payload_json: br#"{"status":200}"#.to_vec(),
            },
        )),
        ..Default::default()
    };
    publisher.send("kernel", env).await.unwrap();

    let ack_received = timeout(Duration::from_secs(2), publisher.recv())
        .await
        .expect("ack recv timed out")
        .expect("ack recv failed");
    let event_id = match ack_received.payload {
        Some(envelope::Payload::EventPublishAck(ack)) => {
            assert_eq!(
                ack.status,
                veyron::proto::veyron::EventPublishStatus::EventPublishOk as i32
            );
            assert!(!ack.event_id.is_empty());
            ack.event_id
        }
        other => panic!("expected EventPublishAck, got: {:?}", other),
    };

    let delivered = timeout(Duration::from_secs(2), subscriber.recv())
        .await
        .expect("event recv timed out")
        .expect("event recv failed");
    match delivered.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "plugin.network.request_completed");
            assert_eq!(e.payload_json, br#"{"status":200}"#);
            assert_eq!(e.retry_count, 0);
            assert_eq!(e.event_id, event_id);
        }
        other => panic!("expected Event, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}
