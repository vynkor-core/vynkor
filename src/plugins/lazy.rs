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
use crate::plugins::registry::ActionLookup;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginConfig;
use crate::proto::vynkor::{envelope, ActionResponse, ActionStatus, Envelope};
use crate::utils::errors::VynkorError;
use futures_util::future::BoxFuture;
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

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
            match self.manager.start(config.clone()).await {
                Ok(_) => Ok(()),
                // already supervised (e.g. mid-restart): it will register on its own
                Err(VynkorError::PluginAlreadyRunning(_)) => Ok(()),
                Err(e) => Err(e),
            }
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

/// calls held per waking plugin; more would grow without bound if the plugin
/// never registers
const MAX_QUEUED: usize = 64;

/// a re-injected frame the router never consumed stops being allowed after this
const ALLOW_TTL: Duration = Duration::from_secs(5);

type Started = (String, Result<(), VynkorError>);

/// one lazy plugin that is starting, or being stopped after a failed start.
/// start and stop for an id never overlap: a call that arrives while the
/// stop is running queues, and the next start waits for it
struct Waking {
    queue: Vec<IncomingMessage>,
    since: Instant,
    /// `starter.start` has not returned yet
    starting: bool,
    /// start timed out; stop once any in-flight start returns
    want_stop: bool,
    /// `starter.stop` is running
    stopping: bool,
}

/// how often idle lazy plugins are looked for
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

type SweepRequest = (Instant, oneshot::Sender<()>);

/// asks a running `LazyStarter` for an idle sweep
#[derive(Clone)]
pub struct SweepHandle {
    tx: mpsc::UnboundedSender<SweepRequest>,
}

impl SweepHandle {
    /// run one sweep as of `now` and return once it has decided what to stop
    /// (the stops themselves finish in the background)
    pub async fn sweep_idle(&self, now: Instant) {
        let (done_tx, done_rx) = oneshot::channel();
        if self.tx.send((now, done_tx)).is_ok() {
            let _ = done_rx.await;
        }
    }
}

pub struct LazyStarter {
    registry: Arc<PluginRegistry>,
    starter: Arc<dyn PluginStarter>,
    router_tx: mpsc::Sender<IncomingMessage>,
    start_timeout: Duration,
    poll_interval: Duration,
    sweep_tx: mpsc::UnboundedSender<SweepRequest>,
    sweep_rx: Option<mpsc::UnboundedReceiver<SweepRequest>>,
}

impl LazyStarter {
    pub fn new(
        registry: Arc<PluginRegistry>,
        starter: Arc<dyn PluginStarter>,
        router_tx: mpsc::Sender<IncomingMessage>,
        start_timeout: Duration,
    ) -> Self {
        let (sweep_tx, sweep_rx) = mpsc::unbounded_channel();
        Self {
            registry,
            starter,
            router_tx,
            start_timeout,
            poll_interval: POLL_INTERVAL,
            sweep_tx,
            sweep_rx: Some(sweep_rx),
        }
    }

    pub fn sweep_handle(&self) -> SweepHandle {
        SweepHandle {
            tx: self.sweep_tx.clone(),
        }
    }

    /// registration-check cadence; tests slow it down to hold the drain open
    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }

    pub async fn run(mut self, mut rx: mpsc::Receiver<WakeRequest>) {
        let mut waking: HashMap<String, Waking> = HashMap::new();
        // `starter.start`/`stop` can take a while; they run off-loop so other
        // wake requests keep queueing, and report back here
        let (started_tx, mut started_rx) = mpsc::unbounded_channel::<Started>();
        let (stopped_tx, mut stopped_rx) = mpsc::unbounded_channel::<String>();
        let mut poll = tokio::time::interval(self.poll_interval);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sweep =
            tokio::time::interval_at(tokio::time::Instant::now() + SWEEP_INTERVAL, SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut sweep_rx = self.sweep_rx.take().expect("run consumes the starter");

        loop {
            tokio::select! {
                req = rx.recv() => {
                    let Some(req) = req else { break };
                    self.on_wake_request(req, &mut waking, &started_tx).await;
                }
                Some((id, result)) = started_rx.recv() => {
                    self.on_start_result(id, result, &mut waking, &stopped_tx);
                }
                Some(id) = stopped_rx.recv() => {
                    self.on_stopped(id, &mut waking, &started_tx);
                }
                _ = poll.tick() => {
                    self.poll_waking(&mut waking, &stopped_tx).await;
                }
                _ = sweep.tick() => {
                    self.sweep_idle(Instant::now(), &mut waking, &stopped_tx);
                }
                Some((now, done)) = sweep_rx.recv() => {
                    self.sweep_idle(now, &mut waking, &stopped_tx);
                    let _ = done.send(());
                }
            }
        }
    }

    async fn on_wake_request(
        &self,
        req: WakeRequest,
        waking: &mut HashMap<String, Waking>,
        started_tx: &mpsc::UnboundedSender<Started>,
    ) {
        let WakeRequest { plugin_id, msg } = req;
        if let Some(w) = waking.get_mut(&plugin_id) {
            if w.queue.len() >= MAX_QUEUED {
                fail_msg(
                    msg,
                    ActionStatus::ActionError,
                    "lazy start queue full".to_string(),
                );
            } else {
                w.queue.push(msg);
            }
            return;
        }
        if self.registry.get(&plugin_id).is_some() {
            // registered and not waking: either the router looked up just
            // before registration, or a call hit the drain gate. deliver it
            // if the plugin really serves the action; never bounce it back
            // as dormant, that would loop
            let served = request_action(&msg).is_some_and(|action| {
                matches!(
                    self.registry.find_action_provider(&action),
                    ActionLookup::Found(p) if p.plugin_id == plugin_id
                )
            });
            if served {
                self.reinject(&plugin_id, vec![msg]).await;
            } else {
                let action = request_action(&msg).unwrap_or_default();
                fail_msg(
                    msg,
                    ActionStatus::ActionNotFound,
                    format!("lazy plugin {plugin_id} does not provide action {action}"),
                );
            }
            return;
        }
        debug!(plugin_id, "waking lazy plugin");
        // start the idle clock: the plugin may register before any call is routed
        self.registry.touch_used(&plugin_id);
        self.registry.lazy_gate_hold(&plugin_id);
        let mut w = Waking {
            queue: vec![msg],
            since: Instant::now(),
            starting: false,
            want_stop: false,
            stopping: false,
        };
        self.spawn_start(&plugin_id, &mut w, started_tx);
        waking.insert(plugin_id, w);
    }

    fn spawn_start(
        &self,
        plugin_id: &str,
        w: &mut Waking,
        started_tx: &mpsc::UnboundedSender<Started>,
    ) {
        w.starting = true;
        w.since = Instant::now();
        let starter = Arc::clone(&self.starter);
        let tx = started_tx.clone();
        let id = plugin_id.to_string();
        tokio::spawn(async move {
            let result = starter.start(&id).await;
            let _ = tx.send((id, result));
        });
    }

    fn spawn_stop(
        &self,
        plugin_id: &str,
        w: &mut Waking,
        stopped_tx: &mpsc::UnboundedSender<String>,
    ) {
        w.stopping = true;
        let starter = Arc::clone(&self.starter);
        let tx = stopped_tx.clone();
        let id = plugin_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = starter.stop(&id).await {
                warn!(plugin_id = %id, error = %e, "stopping lazy plugin failed");
            }
            let _ = tx.send(id);
        });
    }

    fn on_start_result(
        &self,
        id: String,
        result: Result<(), VynkorError>,
        waking: &mut HashMap<String, Waking>,
        stopped_tx: &mpsc::UnboundedSender<String>,
    ) {
        let Some(w) = waking.get_mut(&id) else {
            // slot already finished (registered and drained): nothing to do
            if let Err(e) = result {
                warn!(plugin_id = %id, error = %e, "late lazy start error");
            }
            return;
        };
        w.starting = false;
        if w.want_stop {
            // start timed out while still running; stop now that it returned
            self.spawn_stop(&id, w, stopped_tx);
            return;
        }
        if let Err(e) = result {
            warn!(plugin_id = %id, error = %e, "lazy plugin failed to start");
            if let Some(w) = waking.remove(&id) {
                self.registry.lazy_gate_release(&id);
                fail_queue(&id, w.queue);
            }
        }
    }

    fn on_stopped(
        &self,
        id: String,
        waking: &mut HashMap<String, Waking>,
        started_tx: &mpsc::UnboundedSender<Started>,
    ) {
        let Some(w) = waking.get_mut(&id) else { return };
        w.stopping = false;
        w.want_stop = false;
        if w.queue.is_empty() {
            waking.remove(&id);
            // idle stop holds the gate; nothing is waiting, so let go
            self.registry.lazy_gate_release(&id);
        } else {
            // calls arrived during the stop: start afresh now that it is done
            self.registry.lazy_gate_hold(&id);
            self.spawn_start(&id, w, started_tx);
        }
    }

    async fn poll_waking(
        &self,
        waking: &mut HashMap<String, Waking>,
        stopped_tx: &mpsc::UnboundedSender<String>,
    ) {
        let now = Instant::now();
        let mut ready = Vec::new();
        let mut expired = Vec::new();
        for (id, w) in waking.iter() {
            if w.stopping || w.want_stop {
                continue;
            }
            if self.registry.get(id).is_some() {
                ready.push(id.clone());
            } else if now.duration_since(w.since) >= self.start_timeout {
                expired.push(id.clone());
            }
        }
        for id in ready {
            if let Some(w) = waking.remove(&id) {
                self.reinject(&id, w.queue).await;
            }
        }
        for id in expired {
            let Some(w) = waking.get_mut(&id) else {
                continue;
            };
            warn!(plugin_id = %id, timeout = ?self.start_timeout, "lazy plugin did not register in time");
            fail_queue(&id, std::mem::take(&mut w.queue));
            self.registry.lazy_gate_release(&id);
            w.want_stop = true;
            if !w.starting {
                self.spawn_stop(&id, w, stopped_tx);
            }
        }
        // gates with no wake in progress go once the router has consumed the
        // frames we allowed (or they went stale)
        for id in self.registry.lazy_gate_ids() {
            if !waking.contains_key(&id) && self.registry.lazy_gate_trim(&id, ALLOW_TTL) {
                self.registry.lazy_gate_release(&id);
            }
        }
    }

    /// stop live lazy plugins idle past their `idle_stop`, through the same
    /// serialized stop as a timed-out wake: the gate sends calls arriving
    /// meanwhile to the queue, and `on_stopped` re-wakes them. a plugin with
    /// a pending action (a streaming session included) is never stopped
    fn sweep_idle(
        &self,
        now: Instant,
        waking: &mut HashMap<String, Waking>,
        stopped_tx: &mpsc::UnboundedSender<String>,
    ) {
        for d in self.registry.list_dormant() {
            let id = d.plugin_id;
            if waking.contains_key(&id) || self.registry.get(&id).is_none() {
                continue;
            }
            let Some(idle) = self.registry.idle_for_at(&id, now) else {
                // live but never used (registered on its own): start its clock
                self.registry.touch_used(&id);
                continue;
            };
            if idle < d.idle_stop || self.registry.has_pending_for_provider(&id) {
                continue;
            }
            info!(plugin_id = %id, idle = ?idle, "stopping idle lazy plugin");
            self.registry.lazy_gate_hold(&id);
            let mut w = Waking {
                queue: Vec::new(),
                since: now,
                starting: false,
                want_stop: false,
                stopping: false,
            };
            self.spawn_stop(&id, &mut w, stopped_tx);
            waking.insert(id, w);
        }
    }

    /// send queued frames back through the router in arrival order; the gate
    /// lets exactly these frames through to the now-live plugin
    async fn reinject(&self, plugin_id: &str, queue: Vec<IncomingMessage>) {
        for msg in queue {
            self.registry.lazy_gate_allow(plugin_id, frame_addr(&msg));
            if self.router_tx.send(msg).await.is_err() {
                return;
            }
        }
    }
}

fn frame_addr(msg: &IncomingMessage) -> usize {
    Arc::as_ptr(&msg.frame.payload) as *const u8 as usize
}

/// the action name of a queued `ActionRequest`, if it decodes as one
fn request_action(msg: &IncomingMessage) -> Option<String> {
    let env = Envelope::decode(msg.frame.payload.as_ref()).ok()?;
    match env.payload {
        Some(envelope::Payload::ActionRequest(r)) => Some(r.action),
        _ => None,
    }
}

fn fail_queue(plugin_id: &str, queue: Vec<IncomingMessage>) {
    for msg in queue {
        fail_msg(
            msg,
            ActionStatus::ActionError,
            format!("lazy plugin {plugin_id} failed to start"),
        );
    }
}

/// answer a queued call with an error, shaped like the router's own error
/// replies (caller's `action_id` and `message_id`)
fn fail_msg(msg: IncomingMessage, status: ActionStatus, error: String) {
    let Ok(env) = Envelope::decode(msg.frame.payload.as_ref()) else {
        return;
    };
    let Some(envelope::Payload::ActionRequest(req)) = env.payload else {
        return;
    };
    let response = Envelope {
        message_id: env.message_id,
        payload: Some(envelope::Payload::ActionResponse(ActionResponse {
            action_id: req.action_id,
            status: status as i32,
            data_json: vec![],
            error,
        })),
        ..Default::default()
    };
    send_envelope(&msg.write_tx, response);
}
