use crate::ipc::framing::Frame;
use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::{envelope, Envelope, Event};
use dashmap::DashMap;
use prost::Message;
use std::collections::HashSet;

pub struct EventBus {
    // event_type → set of plugin_ids
    subscriptions: DashMap<String, HashSet<String>>,
    // plugin_id → set of event_types (for fast unsubscribe_all)
    by_plugin: DashMap<String, HashSet<String>>,
}

impl EventBus {
    pub fn new() -> Self {
        EventBus {
            subscriptions: DashMap::new(),
            by_plugin: DashMap::new(),
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
        let mut targets: HashSet<String> = HashSet::new();

        // direct subscribers
        if let Some(subs) = self.subscriptions.get(&event.event_type) {
            targets.extend(subs.iter().cloned());
        }
        // wildcard subscribers
        if let Some(wildcards) = self.subscriptions.get("*") {
            targets.extend(wildcards.iter().cloned());
        }

        if targets.is_empty() {
            return;
        }

        let env = Envelope {
            payload: Some(envelope::Payload::Event(event)),
            ..Default::default()
        };
        let mut payload = Vec::new();
        if env.encode(&mut payload).is_err() {
            return;
        }

        for plugin_id in targets {
            if let Some(entry) = registry.get(&plugin_id) {
                let frame = build_frame(&payload, &plugin_id);
                let _ = entry.write_tx.send(frame).await;
            }
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
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
