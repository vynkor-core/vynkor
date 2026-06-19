#![allow(dead_code)]

use crate::auth::permissions::{action_to_permission, check_permission};
use crate::events::bus::EventBus;
use crate::ipc::framing::{target_as_str, Frame};
use crate::ipc::messages::IncomingMessage;
use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::{
    envelope, ActionResponse, ActionStatus, Envelope, ErrorCode, ErrorMessage, Event,
    PluginRegisterAck, Pong,
};
use prost::Message;
use std::sync::Arc;
use tokio::sync::mpsc;

pub struct MessageRouter;

impl MessageRouter {
    pub async fn run(
        mut rx: mpsc::Receiver<IncomingMessage>,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
    ) {
        while let Some(msg) = rx.recv().await {
            let target = {
                let frame = &msg.frame;
                target_as_str(frame).to_string()
            };

            match target.as_str() {
                "kernel" => {
                    Self::handle_kernel_message(msg, &registry, &event_bus).await;
                }
                "*" => {
                    Self::broadcast(msg, &registry).await;
                }
                plugin_id => {
                    Self::forward(msg, plugin_id, &registry).await;
                }
            }
        }
    }

    async fn handle_kernel_message(
        msg: IncomingMessage,
        registry: &PluginRegistry,
        event_bus: &EventBus,
    ) {
        let envelope = match Envelope::decode(msg.frame.payload.as_slice()) {
            Ok(e) => e,
            Err(_) => {
                Self::send_error(
                    &msg.write_tx,
                    ErrorCode::ErrDeserialization,
                    "decode failed",
                )
                .await;
                return;
            }
        };

        // Allow PluginRegister from unregistered senders; all others require registration
        let is_register = matches!(envelope.payload, Some(envelope::Payload::PluginRegister(_)));
        if !is_register && !registry.is_registered(msg.conn_id) {
            Self::send_error(&msg.write_tx, ErrorCode::ErrNotRegistered, "not registered").await;
            return;
        }

        match envelope.payload {
            Some(envelope::Payload::PluginRegister(reg)) => {
                let plugin_id = reg.plugin_id.clone();
                let manifest = reg.manifest.unwrap_or_default();
                let result = registry.register(
                    plugin_id.clone(),
                    msg.conn_id,
                    manifest,
                    msg.write_tx.clone(),
                );

                let ack = match &result {
                    Ok(()) => PluginRegisterAck {
                        accepted: true,
                        reject_reason: String::new(),
                        granted_permissions: vec![],
                    },
                    Err(e) => PluginRegisterAck {
                        accepted: false,
                        reject_reason: e.to_string(),
                        granted_permissions: vec![],
                    },
                };

                let response = Envelope {
                    payload: Some(envelope::Payload::PluginRegisterAck(ack)),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, response).await;

                if result.is_ok() {
                    event_bus
                        .publish(
                            Event {
                                event_id: format!("sys-joined-{plugin_id}"),
                                event_type: "system.plugin_joined".to_string(),
                                payload_json: format!(r#"{{"plugin_id":"{plugin_id}"}}"#)
                                    .into_bytes(),
                                retry_count: 0,
                            },
                            registry,
                        )
                        .await;
                }
            }

            Some(envelope::Payload::Ping(ping)) => {
                let pong = Envelope {
                    payload: Some(envelope::Payload::Pong(Pong {
                        original_timestamp: ping.timestamp,
                        server_timestamp: 0,
                    })),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, pong).await;
            }

            Some(envelope::Payload::Subscribe(sub)) => {
                if let Some(entry) = registry.get_by_conn_id(msg.conn_id) {
                    event_bus.subscribe(&entry.plugin_id, sub.event_types);
                }
            }

            Some(envelope::Payload::Unsubscribe(unsub)) => {
                if let Some(entry) = registry.get_by_conn_id(msg.conn_id) {
                    event_bus.unsubscribe(&entry.plugin_id, unsub.event_types);
                }
            }

            Some(envelope::Payload::ActionRequest(req)) => {
                let action_id = req.action_id.clone();
                let sender_id = registry
                    .get_by_conn_id(msg.conn_id)
                    .map(|e| e.plugin_id.clone())
                    .unwrap_or_default();

                let status = match action_to_permission(&req.action) {
                    None => ActionStatus::ActionNotFound,
                    Some(perm) => match check_permission(registry, &sender_id, perm) {
                        Ok(()) => ActionStatus::ActionOk,
                        Err(_) => ActionStatus::ActionPermissionDeny,
                    },
                };

                let response = Envelope {
                    payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                        action_id,
                        status: status as i32,
                        data_json: vec![],
                        error: if status == ActionStatus::ActionOk {
                            String::new()
                        } else {
                            format!("{:?}", status)
                        },
                    })),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, response).await;
            }

            _ => {
                Self::send_error(&msg.write_tx, ErrorCode::ErrUnknown, "unhandled message").await;
            }
        }
    }

    async fn forward(msg: IncomingMessage, plugin_id: &str, registry: &PluginRegistry) {
        if !registry.is_registered(msg.conn_id) {
            Self::send_error(&msg.write_tx, ErrorCode::ErrNotRegistered, "not registered").await;
            return;
        }

        match registry.get(plugin_id) {
            Some(entry) => {
                let _ = entry.write_tx.send(msg.frame).await;
            }
            None => {
                Self::send_error(&msg.write_tx, ErrorCode::ErrUnknown, "plugin not found").await;
            }
        }
    }

    async fn broadcast(msg: IncomingMessage, registry: &PluginRegistry) {
        let entries = registry.list();
        for entry in entries {
            if entry.conn_id == msg.conn_id {
                continue; // skip sender
            }
            // clone frame payload for each recipient
            let frame = Frame {
                magic: msg.frame.magic,
                flags: msg.frame.flags,
                length: msg.frame.length,
                target: msg.frame.target,
                crc32: msg.frame.crc32,
                payload: msg.frame.payload.clone(),
            };
            let _ = entry.write_tx.send(frame).await;
        }
    }

    async fn send_envelope(tx: &mpsc::Sender<Frame>, env: Envelope) {
        let mut payload = Vec::new();
        if env.encode(&mut payload).is_err() {
            return;
        }
        let crc = crc32fast::hash(&payload);
        let frame = Frame {
            magic: 0x5652,
            flags: 0,
            length: payload.len() as u32,
            target: {
                let mut t = [0u8; 32];
                t[..6].copy_from_slice(b"client");
                t
            },
            crc32: crc,
            payload,
        };
        let _ = tx.send(frame).await;
    }

    async fn send_error(tx: &mpsc::Sender<Frame>, code: ErrorCode, message: &str) {
        let env = Envelope {
            payload: Some(envelope::Payload::Error(ErrorMessage {
                code: code as i32,
                message: message.to_string(),
                details: String::new(),
            })),
            ..Default::default()
        };
        Self::send_envelope(tx, env).await;
    }
}
