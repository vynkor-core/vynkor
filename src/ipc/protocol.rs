use crate::auth::jwt::JwtValidator;
use crate::auth::permissions::{check_ipc_send, check_ipc_target, check_permission};
use crate::events::bus::EventBus;
use crate::events::store::EventStore;
use crate::ipc::connection::{out_frame, Outbound};
use crate::ipc::framing::{target_as_str, Frame, FLAG_RAW_BINARY};
use crate::ipc::messages::IncomingMessage;
use crate::kernel::commands::{CommandHandler, CommandOutcome};
use crate::plugins::registry::{ActionLookup, PendingAction, PluginRegistry};
use crate::proto::veyron::{
    envelope, ActionRequest, ActionResponse, ActionStatus, Envelope, ErrorCode, ErrorMessage,
    Event, KernelCommandAck, PermissionType, PluginRegisterAck, Pong,
};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use metrics::{counter, histogram};
use prost::Message;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};

static MSG_SEQ: AtomicU64 = AtomicU64::new(0);
static ACTION_CORRELATION_SEQ: AtomicU64 = AtomicU64::new(0);

/// Max time the router will wait to hand a frame to a plugin's write channel
/// (capacity-bounded). A plugin that does not drain its channel must not stall
/// the single-threaded router for everyone else — its frame is dropped instead.
const WRITE_SEND_TIMEOUT: Duration = Duration::from_millis(50);

pub struct MessageRouter;

impl MessageRouter {
    pub async fn run(
        rx: mpsc::Receiver<IncomingMessage>,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
        jwt_validator: Option<Arc<JwtValidator>>,
    ) {
        let defaults = crate::utils::config::Config::default();
        Self::run_with_context(
            rx,
            registry,
            event_bus,
            jwt_validator,
            Instant::now(),
            None,
            None,
            None,
            None,
            defaults.action_timeout_ms,
            defaults.max_conn_errors,
            defaults.max_tracked_error_conns,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_context(
        mut rx: mpsc::Receiver<IncomingMessage>,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
        jwt_validator: Option<Arc<JwtValidator>>,
        start_time: Instant,
        config_path: Option<String>,
        event_store: Option<Arc<EventStore>>,
        mac_secret: Option<Arc<Vec<u8>>>,
        ipc_rate_limit_rps: Option<u32>,
        action_timeout_ms: u32,
        max_conn_errors: u32,
        max_tracked_error_conns: usize,
    ) {
        // Per-connection protocol-error budget. A connection that produces a burst
        // of malformed/denied/unhandled messages (which each generate an error
        // response) gets throttled: further messages are dropped without a reply,
        // capping the amplification a single misbehaving plugin can cause (VULN-007).
        // A successful message resets the budget, so transient errors don't accrue.
        let mut error_counts: HashMap<u64, u32> = HashMap::new();

        // Per-connection IPC send rate limiter keyed by conn_id.
        let ipc_limiter: Option<Arc<DefaultKeyedRateLimiter<u64>>> =
            ipc_rate_limit_rps.and_then(|rps| {
                NonZeroU32::new(rps).map(|r| Arc::new(RateLimiter::keyed(Quota::per_second(r))))
            });

        // conn_ids are monotonically assigned and never reused, so without
        // periodic eviction this keyed state grows for the life of the
        // process (AUDIT M-01). Evict idle keys on the same cadence as the
        // error-budget map prune.
        let mut prune_tick = tokio::time::interval(Duration::from_secs(60));
        prune_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let msg = tokio::select! {
                biased;
                _ = prune_tick.tick() => {
                    if let Some(limiter) = &ipc_limiter {
                        limiter.retain_recent();
                    }
                    for expired in registry.sweep_expired_actions(Instant::now()) {
                        let response = Envelope {
                            payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                                action_id: expired.original_action_id,
                                status: ActionStatus::ActionTimeout as i32,
                                data_json: vec![],
                                error: "action timed out".to_string(),
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&expired.requester_write_tx, response).await;
                    }
                    continue;
                }
                msg = rx.recv() => match msg {
                    Some(msg) => msg,
                    None => break,
                },
            };
            let conn_id = msg.conn_id;

            // Per-plugin IPC rate limit: send ERR_RATE_LIMITED without disconnecting.
            if let Some(limiter) = &ipc_limiter {
                if limiter.check_key(&conn_id).is_err() {
                    counter!("ipc_send_denied_total").increment(1);
                    Self::send_error(
                        &msg.write_tx,
                        ErrorCode::ErrRateLimited,
                        "IPC rate limit exceeded",
                    )
                    .await;
                    continue;
                }
            }

            let target = match target_as_str(&msg.frame) {
                Some(t) => t.to_string(),
                None => {
                    let raw_hex: String = msg
                        .frame
                        .target
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    warn!(conn_id, raw_target = %raw_hex, "frame target is not valid UTF-8");
                    Self::send_error(
                        &msg.write_tx,
                        ErrorCode::ErrUnknown,
                        "invalid UTF-8 in frame target",
                    )
                    .await;
                    continue;
                }
            };

            if error_counts.get(&conn_id).copied().unwrap_or(0) >= max_conn_errors {
                counter!("ipc_throttled_messages_total").increment(1);
                continue;
            }

            let errored = match target.as_str() {
                "kernel" => {
                    counter!("messages_routed_total", "routing" => "kernel").increment(1);
                    Self::handle_kernel_message(
                        msg,
                        &registry,
                        &event_bus,
                        &jwt_validator,
                        start_time,
                        config_path.as_deref(),
                        event_store.as_deref(),
                        &mac_secret,
                        action_timeout_ms,
                    )
                    .await
                }
                "*" => {
                    counter!("messages_routed_total", "routing" => "broadcast").increment(1);
                    Self::broadcast(msg, &registry).await
                }
                plugin_id => {
                    counter!("messages_routed_total", "routing" => "forward").increment(1);
                    Self::forward(msg, plugin_id, &registry).await
                }
            };

            if errored {
                if error_counts.len() >= max_tracked_error_conns {
                    error_counts.retain(|cid, _| registry.is_registered(*cid));
                }
                let count = error_counts.entry(conn_id).or_insert(0);
                *count += 1;
                if *count == max_conn_errors {
                    warn!(
                        conn_id,
                        threshold = max_conn_errors,
                        "connection exceeded protocol-error budget; throttling further messages"
                    );
                    counter!("ipc_throttled_connections_total").increment(1);
                }
            } else {
                // Well-behaved message — clear any accrued error budget.
                error_counts.remove(&conn_id);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_kernel_message(
        msg: IncomingMessage,
        registry: &PluginRegistry,
        event_bus: &EventBus,
        jwt_validator: &Option<Arc<JwtValidator>>,
        start_time: Instant,
        config_path: Option<&str>,
        event_store: Option<&EventStore>,
        mac_secret: &Option<Arc<Vec<u8>>>,
        action_timeout_ms: u32,
    ) -> bool {
        let envelope = match Envelope::decode(msg.frame.payload.as_ref()) {
            Ok(e) => e,
            Err(_) => {
                Self::send_error(
                    &msg.write_tx,
                    ErrorCode::ErrDeserialization,
                    "decode failed",
                )
                .await;
                return true;
            }
        };

        // Allow PluginRegister from unregistered senders; all others require registration
        let is_register = matches!(envelope.payload, Some(envelope::Payload::PluginRegister(_)));
        if !is_register && !registry.is_registered(msg.conn_id) {
            Self::send_error(&msg.write_tx, ErrorCode::ErrNotRegistered, "not registered").await;
            return true;
        }

        match envelope.payload {
            Some(envelope::Payload::PluginRegister(reg)) => {
                let plugin_id = reg.plugin_id.clone();
                let mut manifest = reg.manifest.unwrap_or_default();

                // JWT validation (only when kernel has jwt_secret configured)
                if let Some(validator) = jwt_validator {
                    match validator.validate(&reg.jwt_token) {
                        Ok(claims) => {
                            if claims.sub != plugin_id {
                                Self::send_register_reject(
                                    &msg.write_tx,
                                    "token plugin_id mismatch",
                                )
                                .await;
                                return true;
                            }
                            // Token fields take precedence over manifest declaration
                            manifest.permissions = claims.permissions;
                            manifest.ipc_targets = claims.ipc_targets;
                        }
                        Err(e) => {
                            Self::send_register_reject(&msg.write_tx, &format!("auth failed: {e}"))
                                .await;
                            return true;
                        }
                    }
                }

                let result = registry.register(
                    plugin_id.clone(),
                    msg.conn_id,
                    manifest,
                    msg.write_tx.clone(),
                );

                // When auth is on, mint a per-registration nonce; the plugin and
                // kernel both derive the frame-MAC key from it.
                let session_nonce: Vec<u8> = if mac_secret.is_some() && result.is_ok() {
                    use rand::RngCore;
                    let mut n = vec![0u8; crate::auth::frame_mac::SESSION_NONCE_LEN];
                    rand::thread_rng().fill_bytes(&mut n);
                    n
                } else {
                    Vec::new()
                };

                let ack = match &result {
                    Ok(()) => {
                        let granted = registry
                            .get(&plugin_id)
                            .map(|e| e.manifest.permissions.clone())
                            .unwrap_or_default();
                        info!(plugin_id = %plugin_id, "plugin registered");
                        counter!("plugins_registered_total").increment(1);
                        PluginRegisterAck {
                            accepted: true,
                            reject_reason: String::new(),
                            granted_permissions: granted,
                            session_nonce: session_nonce.clone(),
                        }
                    }
                    Err(e) => {
                        warn!(plugin_id = %plugin_id, reason = %e, "registration rejected");
                        PluginRegisterAck {
                            accepted: false,
                            reject_reason: e.to_string(),
                            granted_permissions: vec![],
                            session_nonce: Vec::new(),
                        }
                    }
                };

                let response = Envelope {
                    payload: Some(envelope::Payload::PluginRegisterAck(ack)),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, response).await;

                // Enable the frame MAC for this connection: derive the key, store
                // it for inbound verification, and tell the write loop (ordered
                // after the ack just sent) to start tagging outbound frames.
                if let (Some(secret), true) = (&mac_secret, result.is_ok()) {
                    let key = crate::auth::frame_mac::derive_session_key(
                        secret,
                        &session_nonce,
                        &plugin_id,
                    );
                    // EnableMac installs the inbound key AND activates outbound tagging
                    // inside the write_loop, after the ack has been written to the socket.
                    // This prevents inbound MAC verification from activating before the
                    // plugin has received the ack (VULN-020).
                    let _ = msg
                        .write_tx
                        .send(Outbound::EnableMac(key, msg.session_key.clone()))
                        .await;
                }

                if result.is_ok() {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis();
                    event_bus
                        .publish(
                            Event {
                                event_id: format!("sys-joined-{plugin_id}-{now_ms}"),
                                event_type: "system.plugin_joined".to_string(),
                                payload_json: format!(r#"{{"plugin_id":"{plugin_id}"}}"#)
                                    .into_bytes(),
                                retry_count: 0,
                            },
                            registry,
                        )
                        .await;
                }
                false
            }

            Some(envelope::Payload::Ping(ping)) => {
                let server_timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let pong = Envelope {
                    payload: Some(envelope::Payload::Pong(Pong {
                        original_timestamp: ping.timestamp,
                        server_timestamp,
                    })),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, pong).await;
                false
            }

            Some(envelope::Payload::Pong(_)) => {
                // watchdog ping response — record the pong
                if let Some(entry) = registry.get_by_conn_id(msg.conn_id) {
                    registry.record_pong(&entry.plugin_id);
                }
                false
            }

            Some(envelope::Payload::Subscribe(sub)) => {
                if let Some(entry) = registry.get_by_conn_id(msg.conn_id) {
                    event_bus.subscribe(&entry.plugin_id, sub.event_types);
                }
                false
            }

            Some(envelope::Payload::Unsubscribe(unsub)) => {
                if let Some(entry) = registry.get_by_conn_id(msg.conn_id) {
                    event_bus.unsubscribe(&entry.plugin_id, unsub.event_types);
                }
                false
            }

            Some(envelope::Payload::ActionRequest(req)) => {
                let action_start = Instant::now();
                let action_id = req.action_id.clone();
                let sender_id = registry
                    .get_by_conn_id(msg.conn_id)
                    .map(|e| e.plugin_id.clone())
                    .unwrap_or_default();

                // R5-07 (option b): route to a plugin that declared this action in
                // its manifest — "declared it" is the entire authorization model,
                // no extra permission check. Ambiguous declarations (>1 provider)
                // are refused rather than arbitrarily resolved.
                let not_found_status = match registry.find_action_provider(&req.action) {
                    ActionLookup::NotFound => Some(ActionStatus::ActionNotFound),
                    ActionLookup::Ambiguous(providers) => {
                        warn!(
                            action = %req.action,
                            providers = ?providers,
                            "ambiguous action declaration: multiple providers, refusing to route"
                        );
                        Some(ActionStatus::ActionNotFound)
                    }
                    ActionLookup::Found(provider) => {
                        let internal_id = format!(
                            "kact-{}",
                            ACTION_CORRELATION_SEQ.fetch_add(1, Ordering::Relaxed)
                        );
                        let effective_timeout_ms = if req.timeout_ms == 0 {
                            action_timeout_ms
                        } else {
                            req.timeout_ms
                        };
                        registry.register_pending_action(
                            internal_id.clone(),
                            PendingAction {
                                requester_write_tx: msg.write_tx.clone(),
                                original_action_id: action_id.clone(),
                                requester_id: sender_id.clone(),
                                deadline: Instant::now()
                                    + Duration::from_millis(effective_timeout_ms as u64),
                                provider_id: provider.plugin_id.clone(),
                            },
                        );

                        let forwarded = Envelope {
                            payload: Some(envelope::Payload::ActionRequest(ActionRequest {
                                action_id: internal_id,
                                action: req.action.clone(),
                                params_json: req.params_json.clone(),
                                timeout_ms: req.timeout_ms,
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&provider.write_tx, forwarded).await;
                        None
                    }
                };

                if let Some(status) = not_found_status {
                    let response = Envelope {
                        payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                            action_id,
                            status: status as i32,
                            data_json: vec![],
                            error: format!("{:?}", status),
                        })),
                        ..Default::default()
                    };
                    Self::send_envelope(&msg.write_tx, response).await;
                }
                histogram!("action_request_duration_ms")
                    .record(action_start.elapsed().as_millis() as f64);
                false
            }

            Some(envelope::Payload::ActionResponse(resp)) => {
                // A provider plugin answering a kernel-routed ActionRequest always
                // targets "kernel" (it doesn't know who really asked) — this is
                // where the kernel translates the internal correlation id back to
                // the original requester's action_id and proxies the response.
                // Resolve the sender's identity BEFORE touching the pending-action
                // map. We must not remove the entry unless the sender is actually
                // the provider it was routed to — otherwise any registered plugin
                // could spoof or steal another provider's response by guessing the
                // sequential internal action_id (AUDIT: response-spoofing gap).
                let sender_plugin_id = registry.get_by_conn_id(msg.conn_id).map(|e| e.plugin_id);

                let taken = match &sender_plugin_id {
                    Some(plugin_id) => {
                        registry.take_pending_action_if_provider(&resp.action_id, plugin_id)
                    }
                    None => None,
                };

                match taken {
                    Some(pending) => {
                        let response = Envelope {
                            payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                                action_id: pending.original_action_id,
                                status: resp.status,
                                data_json: resp.data_json,
                                error: resp.error,
                            })),
                            ..Default::default()
                        };
                        Self::send_envelope(&pending.requester_write_tx, response).await;
                    }
                    None => {
                        warn!(
                            action_id = %resp.action_id,
                            sender = ?sender_plugin_id,
                            "action response with no matching pending request for this sender \
                             (late, duplicate, already timed out, or sender is not the routed \
                             provider), dropping"
                        );
                    }
                }
                false
            }

            Some(envelope::Payload::KernelCommand(cmd)) => {
                let sender_id = registry
                    .get_by_conn_id(msg.conn_id)
                    .map(|e| e.plugin_id.clone())
                    .unwrap_or_default();

                let outcome = if cmd.command != "health_check"
                    && check_permission(registry, &sender_id, PermissionType::PermissionKernelAdmin)
                        .is_err()
                {
                    warn!(
                        sender = %sender_id,
                        command = %cmd.command,
                        "kernel command permission denied"
                    );
                    CommandOutcome::permission_denied(format!(
                        "{sender_id} lacks PERMISSION_KERNEL_ADMIN"
                    ))
                } else {
                    CommandHandler::dispatch(&cmd.command, registry, start_time, config_path)
                };

                let ack = Envelope {
                    payload: Some(envelope::Payload::KernelCommandAck(KernelCommandAck {
                        command_id: cmd.command_id,
                        status: outcome.status as i32,
                        data_json: outcome.data_json,
                        error: outcome.error,
                    })),
                    ..Default::default()
                };
                Self::send_envelope(&msg.write_tx, ack).await;
                false
            }

            Some(envelope::Payload::EventAck(ack)) => {
                if let Some(store) = event_store {
                    store.mark_delivered(&ack.event_id);
                }
                false
            }

            _ => {
                Self::send_error(&msg.write_tx, ErrorCode::ErrUnknown, "unhandled message").await;
                true
            }
        }
    }

    async fn forward(msg: IncomingMessage, plugin_id: &str, registry: &PluginRegistry) -> bool {
        let sender_id = match registry.get_by_conn_id(msg.conn_id) {
            Some(entry) => entry.plugin_id.clone(),
            None => {
                Self::send_error(&msg.write_tx, ErrorCode::ErrNotRegistered, "not registered")
                    .await;
                return true;
            }
        };

        // Default-deny peer-to-peer IPC: sender must hold PERMISSION_IPC_SEND.
        if check_ipc_send(registry, &sender_id).is_err() {
            warn!(sender = %sender_id, target = %plugin_id, "ipc send denied");
            counter!("ipc_send_denied_total").increment(1);
            Self::send_error(
                &msg.write_tx,
                ErrorCode::ErrPermissionDenied,
                "PERMISSION_IPC_SEND required",
            )
            .await;
            return true;
        }

        // Per-target allowlist (T-04): ipc_targets must explicitly list the target.
        // Empty ipc_targets = deny-all even with PERMISSION_IPC_SEND.
        if check_ipc_target(registry, &sender_id, plugin_id).is_err() {
            warn!(sender = %sender_id, target = %plugin_id, "ipc target not in allowlist");
            counter!("ipc_send_denied_total").increment(1);
            Self::send_error(
                &msg.write_tx,
                ErrorCode::ErrPermissionDenied,
                "target not in ipc_targets allowlist",
            )
            .await;
            return true;
        }

        // Audio stream gate (T-06): raw binary frames require PERMISSION_AUDIO_STREAM.
        if msg.frame.flags & FLAG_RAW_BINARY != 0
            && check_permission(registry, &sender_id, PermissionType::PermissionAudioStream)
                .is_err()
        {
            warn!(sender = %sender_id, target = %plugin_id, "audio stream permission denied");
            counter!("ipc_send_denied_total").increment(1);
            Self::send_error(
                &msg.write_tx,
                ErrorCode::ErrPermissionDenied,
                "PERMISSION_AUDIO_STREAM required for FLAG_RAW_BINARY frames",
            )
            .await;
            return true;
        }

        match registry.get(plugin_id) {
            Some(entry) => {
                // Strip FLAG_MAC_PRESENT: the recipient's write_loop re-tags with its own
                // session key. Forwarding the sender's flag without a fresh tag corrupts
                // the stream (mirrors broadcast()).
                let frame = Frame {
                    magic: msg.frame.magic,
                    flags: msg.frame.flags & !crate::ipc::framing::FLAG_MAC_PRESENT,
                    length: msg.frame.length,
                    target: msg.frame.target,
                    crc32: msg.frame.crc32,
                    payload: msg.frame.payload.clone(),
                    mac: None,
                };
                // Bounded send: a slow target must not block the router. Dropping
                // one frame for a non-draining plugin is not the sender's fault, so
                // this is not counted against the sender's error budget.
                if timeout(WRITE_SEND_TIMEOUT, entry.write_tx.send(out_frame(frame)))
                    .await
                    .is_err()
                {
                    warn!(target = %plugin_id, "forward timeout: slow target, frame dropped");
                    counter!("ipc_forward_timeouts_total").increment(1);
                }
                false
            }
            None => {
                warn!(target = %plugin_id, "forward: unknown target");
                Self::send_error(&msg.write_tx, ErrorCode::ErrUnknown, "plugin not found").await;
                true
            }
        }
    }

    async fn broadcast(msg: IncomingMessage, registry: &PluginRegistry) -> bool {
        let sender_id = match registry.get_by_conn_id(msg.conn_id) {
            Some(entry) => entry.plugin_id.clone(),
            None => {
                Self::send_error(&msg.write_tx, ErrorCode::ErrNotRegistered, "not registered")
                    .await;
                return true;
            }
        };

        // Default-deny: broadcasting is peer-to-peer fan-out — same gate as unicast.
        if check_ipc_send(registry, &sender_id).is_err() {
            warn!(sender = %sender_id, "broadcast denied");
            counter!("ipc_send_denied_total").increment(1);
            Self::send_error(
                &msg.write_tx,
                ErrorCode::ErrPermissionDenied,
                "PERMISSION_IPC_SEND required",
            )
            .await;
            return true;
        }

        // Audio stream gate (T-06): raw binary broadcast requires PERMISSION_AUDIO_STREAM.
        if msg.frame.flags & FLAG_RAW_BINARY != 0
            && check_permission(registry, &sender_id, PermissionType::PermissionAudioStream)
                .is_err()
        {
            warn!(sender = %sender_id, "audio stream broadcast denied");
            counter!("ipc_send_denied_total").increment(1);
            Self::send_error(
                &msg.write_tx,
                ErrorCode::ErrPermissionDenied,
                "PERMISSION_AUDIO_STREAM required for FLAG_RAW_BINARY frames",
            )
            .await;
            return true;
        }

        let entries = registry.list();
        for entry in entries {
            if entry.conn_id == msg.conn_id {
                continue; // skip sender
            }
            // Per-target allowlist check: mirrors forward(). Empty ipc_targets = deny-all.
            if check_ipc_target(registry, &sender_id, &entry.plugin_id).is_err() {
                counter!("ipc_send_denied_total").increment(1);
                continue;
            }
            // Strip FLAG_MAC_PRESENT: the recipient's write_loop re-tags with its own
            // session key. Forwarding the sender's flag without a fresh tag corrupts the stream.
            let frame = Frame {
                magic: msg.frame.magic,
                flags: msg.frame.flags & !crate::ipc::framing::FLAG_MAC_PRESENT,
                length: msg.frame.length,
                target: msg.frame.target,
                crc32: msg.frame.crc32,
                payload: msg.frame.payload.clone(),
                mac: None,
            };
            match timeout(WRITE_SEND_TIMEOUT, entry.write_tx.send(out_frame(frame))).await {
                Ok(_) => {}
                Err(_) => {
                    warn!(
                        plugin_id = %entry.plugin_id,
                        "broadcast timeout: slow plugin skipped"
                    );
                    counter!("broadcast_timeouts_total").increment(1);
                }
            }
        }
        false
    }

    async fn send_register_reject(tx: &mpsc::Sender<Outbound>, reason: &str) {
        let ack = PluginRegisterAck {
            accepted: false,
            reject_reason: reason.to_string(),
            granted_permissions: vec![],
            session_nonce: Vec::new(),
        };
        let env = Envelope {
            payload: Some(envelope::Payload::PluginRegisterAck(ack)),
            ..Default::default()
        };
        Self::send_envelope(tx, env).await;
    }

    async fn send_envelope(tx: &mpsc::Sender<Outbound>, mut env: Envelope) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
        env.message_id = format!("k-{ts}-{seq}");
        env.timestamp = ts;
        env.sender_id = "kernel".to_string();

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
            payload: payload.into(),
            mac: None,
        };
        let _ = tx.send(out_frame(frame)).await;
    }

    async fn send_error(tx: &mpsc::Sender<Outbound>, code: ErrorCode, message: &str) {
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
