//! wake-on-first-call for lazy plugins (lifecycle only: spawn, queue, re-inject)
//!
//! the router hands calls for dormant actions here. the first call spawns the
//! plugin, later ones queue behind it; once the plugin registers, the queued
//! frames are re-sent to the router in arrival order and take the normal path.
//! frames are re-injected as received: connection.rs verified their MAC before
//! the router ever saw them, so nothing is bypassed.

use crate::ipc::messages::IncomingMessage;
use crate::ipc::protocol::helpers::send_envelope;
use crate::plugins::manager::PluginManager;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginConfig;
use crate::proto::vynkor::{envelope, ActionResponse, ActionStatus, Envelope};
use crate::utils::errors::VynkorError;
use futures_util::future::BoxFuture;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub trait PluginStarter: Send + Sync {
    fn start<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>>;
    fn stop<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>>;
}

pub struct ManagerStarter {
    manager: Arc<PluginManager>,
    configs: HashMap<String, PluginConfig>,
}

impl ManagerStarter {
    pub fn new(manager: Arc<PluginManager>, configs: HashMap<String, PluginConfig>) -> Self {
        Self { manager, configs }
    }
}

impl PluginStarter for ManagerStarter {
    fn start<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>> {
        Box::pin(async move {
            let config = self
                .configs
                .get(plugin_id)
                .ok_or_else(|| VynkorError::PluginNotFound(plugin_id.to_string()))?;
            self.manager.start(config.clone()).await.map(|_| ())
        })
    }

    fn stop<'a>(&'a self, plugin_id: &'a str) -> BoxFuture<'a, Result<(), VynkorError>> {
        Box::pin(async move { self.manager.stop(plugin_id).await })
    }
}

pub struct WakeRequest {
    pub plugin_id: String,
    pub msg: IncomingMessage,
}

/// how often a waking plugin is checked for registration
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// calls queued for a plugin that was asked to start and has not registered yet
struct Waking {
    queue: Vec<IncomingMessage>,
    since: Instant,
}

pub struct LazyStarter {
    registry: Arc<PluginRegistry>,
    starter: Arc<dyn PluginStarter>,
    router_tx: mpsc::Sender<IncomingMessage>,
    start_timeout: Duration,
}

impl LazyStarter {
    pub fn new(
        registry: Arc<PluginRegistry>,
        starter: Arc<dyn PluginStarter>,
        router_tx: mpsc::Sender<IncomingMessage>,
        start_timeout: Duration,
    ) -> Self {
        Self {
            registry,
            starter,
            router_tx,
            start_timeout,
        }
    }

    pub async fn run(self, mut rx: mpsc::Receiver<WakeRequest>) {
        let mut waking: HashMap<String, Waking> = HashMap::new();
        // `starter.start` can take a while; it runs off-loop so other wake
        // requests keep queueing, and reports back here
        let (started_tx, mut started_rx) =
            mpsc::unbounded_channel::<(String, Result<(), VynkorError>)>();
        let mut poll = tokio::time::interval(POLL_INTERVAL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                req = rx.recv() => {
                    let Some(req) = req else { break };
                    self.on_wake_request(req, &mut waking, &started_tx).await;
                }
                Some((id, result)) = started_rx.recv() => {
                    self.on_start_result(id, result, &mut waking);
                }
                _ = poll.tick() => {
                    self.poll_waking(&mut waking).await;
                }
            }
        }
    }

    async fn on_wake_request(
        &self,
        req: WakeRequest,
        waking: &mut HashMap<String, Waking>,
        started_tx: &mpsc::UnboundedSender<(String, Result<(), VynkorError>)>,
    ) {
        let WakeRequest { plugin_id, msg } = req;
        if let Some(w) = waking.get_mut(&plugin_id) {
            w.queue.push(msg);
            return;
        }
        // registered between the router's lookup and now: no spawn needed
        if self.registry.get(&plugin_id).is_some() {
            self.reinject(vec![msg]).await;
            return;
        }
        debug!(plugin_id, "waking lazy plugin");
        waking.insert(
            plugin_id.clone(),
            Waking {
                queue: vec![msg],
                since: Instant::now(),
            },
        );
        let starter = Arc::clone(&self.starter);
        let tx = started_tx.clone();
        tokio::spawn(async move {
            let result = starter.start(&plugin_id).await;
            let _ = tx.send((plugin_id, result));
        });
    }

    fn on_start_result(
        &self,
        id: String,
        result: Result<(), VynkorError>,
        waking: &mut HashMap<String, Waking>,
    ) {
        let Err(e) = result else { return };
        warn!(plugin_id = %id, error = %e, "lazy plugin failed to start");
        if let Some(w) = waking.remove(&id) {
            fail_queue(&id, w.queue);
        }
    }

    async fn poll_waking(&self, waking: &mut HashMap<String, Waking>) {
        let now = Instant::now();
        let mut ready = Vec::new();
        let mut expired = Vec::new();
        for (id, w) in waking.iter() {
            if self.registry.get(id).is_some() {
                ready.push(id.clone());
            } else if now.duration_since(w.since) >= self.start_timeout {
                expired.push(id.clone());
            }
        }
        for id in ready {
            if let Some(w) = waking.remove(&id) {
                self.reinject(w.queue).await;
            }
        }
        for id in expired {
            let Some(w) = waking.remove(&id) else {
                continue;
            };
            warn!(plugin_id = %id, timeout = ?self.start_timeout, "lazy plugin did not register in time");
            fail_queue(&id, w.queue);
            // stop off-loop: it can wait out the plugin's grace period
            let starter = Arc::clone(&self.starter);
            tokio::spawn(async move {
                if let Err(e) = starter.stop(&id).await {
                    warn!(plugin_id = %id, error = %e, "stopping half-started lazy plugin failed");
                }
            });
        }
    }

    /// send queued frames back through the router, preserving arrival order
    async fn reinject(&self, queue: Vec<IncomingMessage>) {
        for msg in queue {
            if self.router_tx.send(msg).await.is_err() {
                return;
            }
        }
    }
}

/// answer every queued call with `ACTION_ERROR`, shaped like the router's own
/// error replies (caller's `action_id` and `message_id`)
fn fail_queue(plugin_id: &str, queue: Vec<IncomingMessage>) {
    for msg in queue {
        let Ok(env) = Envelope::decode(msg.frame.payload.as_ref()) else {
            continue;
        };
        let Some(envelope::Payload::ActionRequest(req)) = env.payload else {
            continue;
        };
        let response = Envelope {
            message_id: env.message_id,
            payload: Some(envelope::Payload::ActionResponse(ActionResponse {
                action_id: req.action_id,
                status: ActionStatus::ActionError as i32,
                data_json: vec![],
                error: format!("lazy plugin {plugin_id} failed to start"),
            })),
            ..Default::default()
        };
        send_envelope(&msg.write_tx, response);
    }
}
