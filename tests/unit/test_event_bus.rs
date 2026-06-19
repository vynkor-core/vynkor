use prost::Message;
use std::sync::Arc;
use tokio::sync::mpsc;
use veyron::events::bus::EventBus;
use veyron::ipc::framing::Frame;
use veyron::plugins::registry::PluginRegistry;
use veyron::proto::veyron::{envelope, Envelope, Event, PluginManifest};

fn make_registry() -> Arc<PluginRegistry> {
    Arc::new(PluginRegistry::new())
}

fn register_plugin(
    registry: &PluginRegistry,
    plugin_id: &str,
    conn_id: u64,
) -> mpsc::Receiver<Frame> {
    let (tx, rx) = mpsc::channel(16);
    registry
        .register(
            plugin_id.to_string(),
            conn_id,
            PluginManifest::default(),
            tx,
        )
        .unwrap();
    rx
}

fn make_event(event_type: &str) -> Event {
    Event {
        event_id: "ev-1".to_string(),
        event_type: event_type.to_string(),
        payload_json: b"{}".to_vec(),
        retry_count: 0,
    }
}

fn decode_event_from_frame(frame: Frame) -> Event {
    let env = Envelope::decode(frame.payload.as_slice()).expect("decode envelope");
    match env.payload {
        Some(envelope::Payload::Event(e)) => e,
        other => panic!("expected Event, got {:?}", other),
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn publish_delivers_event_to_subscriber() {
    let bus = EventBus::new();
    let registry = make_registry();
    let mut rx = register_plugin(&registry, "listener", 1);

    bus.subscribe("listener", vec!["alarm.fired".to_string()]);
    bus.publish(make_event("alarm.fired"), &registry).await;

    let frame = rx.recv().await.expect("frame must arrive");
    let ev = decode_event_from_frame(frame);
    assert_eq!(ev.event_type, "alarm.fired");
}

#[tokio::test]
async fn unsubscribe_stops_delivery() {
    let bus = EventBus::new();
    let registry = make_registry();
    let mut rx = register_plugin(&registry, "listener", 1);

    bus.subscribe("listener", vec!["alarm.fired".to_string()]);
    bus.unsubscribe("listener", vec!["alarm.fired".to_string()]);
    bus.publish(make_event("alarm.fired"), &registry).await;

    // channel should be empty — nothing arrives
    assert!(
        rx.try_recv().is_err(),
        "no frame expected after unsubscribe"
    );
}

#[tokio::test]
async fn wildcard_subscriber_receives_all_events() {
    let bus = EventBus::new();
    let registry = make_registry();
    let mut rx = register_plugin(&registry, "watcher", 1);

    bus.subscribe("watcher", vec!["*".to_string()]);
    bus.publish(make_event("system.plugin_joined"), &registry)
        .await;
    bus.publish(make_event("alarm.fired"), &registry).await;

    let f1 = rx.recv().await.expect("first event");
    let f2 = rx.recv().await.expect("second event");
    assert_eq!(
        decode_event_from_frame(f1).event_type,
        "system.plugin_joined"
    );
    assert_eq!(decode_event_from_frame(f2).event_type, "alarm.fired");
}

#[tokio::test]
async fn unsubscribe_all_removes_all_subscriptions() {
    let bus = EventBus::new();
    let registry = make_registry();
    let mut rx = register_plugin(&registry, "p", 1);

    bus.subscribe("p", vec!["a".to_string(), "b".to_string()]);
    bus.unsubscribe_all("p");
    bus.publish(make_event("a"), &registry).await;
    bus.publish(make_event("b"), &registry).await;

    assert!(rx.try_recv().is_err(), "no frames after unsubscribe_all");
}

#[tokio::test]
async fn publish_to_unsubscribed_event_does_not_panic() {
    let bus = EventBus::new();
    let registry = make_registry();
    // no subscribers registered
    bus.publish(make_event("ghost.event"), &registry).await;
    // reaching here without panic = pass
}

#[tokio::test]
async fn multiple_subscribers_all_receive_event() {
    let bus = EventBus::new();
    let registry = make_registry();
    let mut rx1 = register_plugin(&registry, "p1", 1);
    let mut rx2 = register_plugin(&registry, "p2", 2);

    bus.subscribe("p1", vec!["tick".to_string()]);
    bus.subscribe("p2", vec!["tick".to_string()]);
    bus.publish(make_event("tick"), &registry).await;

    assert!(rx1.recv().await.is_some(), "p1 must get event");
    assert!(rx2.recv().await.is_some(), "p2 must get event");
}
