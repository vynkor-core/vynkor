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
