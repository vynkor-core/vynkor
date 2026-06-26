use crate::events::store::EventStore;
use crate::ipc::connection::out_frame;
use crate::ipc::framing::Frame;
use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::{envelope, Envelope, Event};
use dashmap::DashMap;
use metrics::counter;
use prost::Message;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// Event delivery is fire-and-forget: if a subscriber does not drain its write
/// channel within this window, the event is dropped rather than blocking the
/// publisher (and every other subscriber on the same publish).
const EVENT_SEND_TIMEOUT: Duration = Duration::from_millis(50);

pub struct EventBus {
    // event_type → set of plugin_ids
    subscriptions: DashMap<String, HashSet<String>>,
    // plugin_id → set of event_types (for fast unsubscribe_all)
    by_plugin: DashMap<String, HashSet<String>>,
    store: Option<Arc<EventStore>>,
}

impl EventBus {
    pub fn new() -> Self {
        EventBus {
            subscriptions: DashMap::new(),
            by_plugin: DashMap::new(),
            store: None,
        }
    }

    pub fn with_store(store: Arc<EventStore>) -> Self {
        EventBus {
            subscriptions: DashMap::new(),
            by_plugin: DashMap::new(),
            store: Some(store),
        }
    }

    pub fn subscribe(&self, plugin_id: &str, event_types: Vec<String>) {
        for event_type in &event_types {
            self.subscriptions
                .entry(event_type.clone())
                .or_default()
                .insert(plugin_id.to_string());
        }
        self.by_plugin
            .entry(plugin_id.to_string())
            .or_default()
            .extend(event_types);
    }

    pub fn unsubscribe(&self, plugin_id: &str, event_types: Vec<String>) {
        for event_type in &event_types {
            if let Some(mut subs) = self.subscriptions.get_mut(event_type) {
                subs.remove(plugin_id);
            }
        }
        if let Some(mut types) = self.by_plugin.get_mut(plugin_id) {
            for t in &event_types {
                types.remove(t);
            }
        }
    }

    pub fn unsubscribe_all(&self, plugin_id: &str) {
        if let Some((_, types)) = self.by_plugin.remove(plugin_id) {
            for event_type in types {
                if let Some(mut subs) = self.subscriptions.get_mut(&event_type) {
                    subs.remove(plugin_id);
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn subscribers(&self, event_type: &str) -> Vec<String> {
        self.subscriptions
            .get(event_type)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub async fn publish(&self, event: Event, registry: &PluginRegistry) {
        if let Some(store) = &self.store {
            store.persist(&event);
        }
        self.deliver(event, registry).await;
    }

    /// Deliver event to subscribers without persisting to store. Used by retry worker.
    pub async fn redeliver(&self, event: Event, registry: &PluginRegistry) {
        self.deliver(event, registry).await;
    }

    async fn deliver(&self, event: Event, registry: &PluginRegistry) {
        let mut targets: HashSet<String> = HashSet::new();

        if let Some(subs) = self.subscriptions.get(&event.event_type) {
            targets.extend(subs.iter().cloned());
        }
        if let Some(wildcards) = self.subscriptions.get("*") {
            targets.extend(wildcards.iter().cloned());
        }

        if targets.is_empty() {
            debug!(event_type = %event.event_type, "event published with no subscribers");
            return;
        }

        let event_type = event.event_type.clone();
        let env = Envelope {
            payload: Some(envelope::Payload::Event(event)),
            ..Default::default()
        };
        let mut payload = Vec::new();
        if env.encode(&mut payload).is_err() {
            return;
        }

        for plugin_id in targets {
            match registry.get(&plugin_id) {
                Some(entry) => {
                    let frame = build_frame(&payload, &plugin_id);
                    // Bounded send: a slow subscriber must not stall the publisher.
                    match tokio::time::timeout(
                        EVENT_SEND_TIMEOUT,
                        entry.write_tx.send(out_frame(frame)),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            // receiver dropped — plugin is disconnecting
                            counter!("events_dropped_total", "reason" => "channel_closed")
                                .increment(1);
                        }
                        Err(_) => {
                            warn!(
                                plugin_id = %plugin_id,
                                event_type = %event_type,
                                "event dropped: subscriber write channel full"
                            );
                            counter!("events_dropped_total", "reason" => "slow_subscriber")
                                .increment(1);
                        }
                    }
                }
                None => {
                    warn!(
                        plugin_id = %plugin_id,
                        event_type = %event_type,
                        "event dropped: subscriber not in registry"
                    );
                    counter!("events_dropped_total", "reason" => "unregistered").increment(1);
                }
            }
        }
    }
}

pub async fn run_retry_worker(
    store: Arc<EventStore>,
    bus: Arc<EventBus>,
    registry: Arc<PluginRegistry>,
) {
    const MAX_RETRIES: u32 = 5;
    // Keep terminal (delivered/dead) events around for a while for debugging,
    // then drop them so events.db does not grow without bound.
    const RETENTION_SECS: u64 = 3600;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let pending = store.pending_older_than(10);
        for event in pending {
            let event_id = event.event_id.clone();
            store.increment_retry_or_dead(&event_id, MAX_RETRIES);
            bus.redeliver(event, &registry).await;
        }
        let pruned = store.prune(RETENTION_SECS);
        if pruned > 0 {
            debug!(count = pruned, "EventStore: pruned terminal events");
        }
    }
}

fn build_frame(payload: &[u8], target: &str) -> Frame {
    let crc = crc32fast::hash(payload);
    let mut t = [0u8; 32];
    let bytes = target.as_bytes();
    let len = bytes.len().min(32);
    t[..len].copy_from_slice(&bytes[..len]);
    Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: t,
        crc32: crc,
        payload: payload.to_vec(),
        mac: None,
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
