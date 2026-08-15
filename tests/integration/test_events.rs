use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{envelope, ActionRisk, ActionSpec, Event, PluginManifest};
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
async fn joined_event_carries_device_fields() {
    // D-04: system.plugin_joined must enrich its payload with the newcomer's
    // device identity so discovery subscribers can key on device without a
    // follow-up /devices call.
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_joined_dev.sock", 19204).await;

    let mut observer = VeyronClient::connect("/tmp/veyron_integ_joined_dev.sock")
        .await
        .unwrap();
    observer
        .register("observer", PluginManifest::default())
        .await
        .unwrap();
    observer.subscribe(vec!["*".to_string()]).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    // a device agent registers with its identity off the wire (v1.6 fields)
    let mut newcomer = VeyronClient::connect("/tmp/veyron_integ_joined_dev.sock")
        .await
        .unwrap();
    let reg = veyron::proto::veyron::PluginRegister {
        plugin_id: "device-agent".to_string(),
        manifest: Some(PluginManifest::default()),
        device_id: "phone-1".to_string(),
        os: veyron::proto::veyron::DeviceOs::Android as i32,
        arch: "aarch64".to_string(),
        os_version: "14".to_string(),
        capabilities: vec!["geo".to_string(), "battery".to_string()],
        ..Default::default()
    };
    let env = veyron::proto::veyron::Envelope {
        payload: Some(envelope::Payload::PluginRegister(reg)),
        ..Default::default()
    };
    newcomer.send("kernel", env).await.unwrap();

    let received = timeout(Duration::from_secs(2), observer.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match received.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "system.plugin_joined");
            let payload = String::from_utf8(e.payload_json).expect("joined payload must be JSON");
            assert!(
                payload.contains("\"plugin_id\":\"device-agent\""),
                "payload={payload}"
            );
            assert!(
                payload.contains("\"device_id\":\"phone-1\""),
                "payload={payload}"
            );
            assert!(payload.contains("\"os\":\"android\""), "payload={payload}");
            assert!(
                payload.contains("\"capabilities\":[\"geo\",\"battery\"]"),
                "payload={payload}"
            );
        }
        other => panic!("expected system.plugin_joined, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn joined_event_carries_action_specs() {
    // D-08: system.plugin_joined must surface the newcomer's tool schema
    // (action_specs) so an AI subscriber can enumerate callable actions from
    // the event alone, without a follow-up get_manifest round-trip.
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_joined_specs.sock", 19205).await;

    let mut observer = VeyronClient::connect("/tmp/veyron_integ_joined_specs.sock")
        .await
        .unwrap();
    observer
        .register("observer", PluginManifest::default())
        .await
        .unwrap();
    observer.subscribe(vec!["*".to_string()]).await.unwrap();

    tokio::time::sleep(Duration::from_millis(20)).await;

    let mut newcomer = VeyronClient::connect("/tmp/veyron_integ_joined_specs.sock")
        .await
        .unwrap();
    let manifest = PluginManifest {
        actions: vec!["weather.get".to_string()],
        action_specs: vec![ActionSpec {
            name: "weather.get".to_string(),
            description: "current conditions for a city".to_string(),
            params_schema: r#"{"type":"object","properties":{"city":{"type":"string"}}}"#
                .to_string(),
            risk: ActionRisk::Low as i32,
            requires_confirmation: false,
        }],
        ..Default::default()
    };
    newcomer.register("weather", manifest).await.unwrap();

    let received = timeout(Duration::from_secs(2), observer.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match received.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "system.plugin_joined");
            let payload = String::from_utf8(e.payload_json).expect("joined payload must be JSON");
            assert!(
                payload.contains("\"plugin_id\":\"weather\""),
                "payload={payload}"
            );
            assert!(
                payload.contains("\"name\":\"weather.get\""),
                "payload={payload}"
            );
            assert!(
                payload.contains("\"description\":\"current conditions for a city\""),
                "payload={payload}"
            );
            assert!(
                payload.contains("\"params_schema\":\"{\\\"type\\\":\\\"object\\\""),
                "payload={payload}"
            );
            assert!(payload.contains("\"risk\":\"low\""), "payload={payload}");
            assert!(
                payload.contains("\"requires_confirmation\":false"),
                "payload={payload}"
            );
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

#[tokio::test]
async fn two_plugins_publishing_same_event_type_land_on_distinct_namespaces() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_evpub_ns.sock", 19702).await;

    let publish_manifest = PluginManifest {
        permissions: vec!["PERMISSION_EVENT_PUBLISH".to_string()],
        ..Default::default()
    };

    let mut network = VeyronClient::connect("/tmp/veyron_integ_evpub_ns.sock")
        .await
        .unwrap();
    network
        .register("network", publish_manifest.clone())
        .await
        .unwrap();

    let mut weather = VeyronClient::connect("/tmp/veyron_integ_evpub_ns.sock")
        .await
        .unwrap();
    weather.register("weather", publish_manifest).await.unwrap();

    let mut subscriber = VeyronClient::connect("/tmp/veyron_integ_evpub_ns.sock")
        .await
        .unwrap();
    subscriber
        .register("evpub-ns-subscriber", PluginManifest::default())
        .await
        .unwrap();
    // Only subscribes to network's namespace, not weather's.
    subscriber
        .subscribe(vec!["plugin.network.request_completed".to_string()])
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(30)).await;

    for client in [&mut network, &mut weather] {
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
        let _ = timeout(Duration::from_secs(2), client.recv())
            .await
            .expect("ack recv timed out")
            .expect("ack recv failed");
    }

    let delivered = timeout(Duration::from_secs(2), subscriber.recv())
        .await
        .expect("event recv timed out")
        .expect("event recv failed");
    match delivered.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "plugin.network.request_completed");
        }
        other => panic!("expected Event, got: {:?}", other),
    }

    // weather's event on a namespace nobody subscribed to must never arrive.
    let never_received = timeout(Duration::from_millis(300), subscriber.recv()).await;
    assert!(
        never_received.is_err(),
        "subscriber must not receive plugin.weather.request_completed \
         when only subscribed to plugin.network.request_completed"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn sdk_publish_event_returns_ack_and_delivers_to_subscriber() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_evpub_sdk.sock", 19703).await;

    let mut publisher = VeyronClient::connect("/tmp/veyron_integ_evpub_sdk.sock")
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

    let mut subscriber = VeyronClient::connect("/tmp/veyron_integ_evpub_sdk.sock")
        .await
        .unwrap();
    subscriber
        .register("evpub-sdk-subscriber", PluginManifest::default())
        .await
        .unwrap();
    subscriber
        .subscribe(vec!["plugin.network.request_completed".to_string()])
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(30)).await;

    let ack = timeout(
        Duration::from_secs(2),
        publisher.publish_event("request_completed", br#"{"status":200}"#, 2000),
    )
    .await
    .expect("timed out")
    .expect("publish_event failed");

    assert_eq!(
        ack.status,
        veyron::proto::veyron::EventPublishStatus::EventPublishOk as i32
    );
    assert!(!ack.event_id.is_empty());

    let delivered = timeout(Duration::from_secs(2), subscriber.recv())
        .await
        .expect("event recv timed out")
        .expect("event recv failed");
    match delivered.payload {
        Some(envelope::Payload::Event(e)) => {
            assert_eq!(e.event_type, "plugin.network.request_completed");
        }
        other => panic!("expected Event, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}
