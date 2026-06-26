use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use prost::Message;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::api::server::ApiServer;
use crate::auth::jwt::JwtValidator;
use crate::events::bus::{run_retry_worker, EventBus};
use crate::events::store::EventStore;
use crate::ipc::framing::Frame;
use crate::ipc::protocol::MessageRouter;
use crate::ipc::server::UdsServer;
use crate::plugins::loader::PluginLoader;
use crate::plugins::manager::PluginManager;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;
use crate::proto::veyron::{envelope, Envelope, Event, PluginShutdown};
use crate::utils::config::Config;

pub struct Kernel;

impl Kernel {
    pub async fn run(config: Config) -> anyhow::Result<()> {
        let shutdown = async {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = sigterm.recv() => { info!("received SIGTERM"); }
            }
        };
        Self::run_with_shutdown(config, shutdown).await
    }

    pub async fn run_with_shutdown<F>(config: Config, shutdown: F) -> anyhow::Result<()>
    where
        F: Future<Output = ()>,
    {
        let registry = Arc::new(PluginRegistry::new());
        let event_bus = Arc::new(EventBus::new());
        Self::run_with_components(config, registry, event_bus, shutdown).await
    }

    pub async fn run_with_components<F>(
        config: Config,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
        shutdown: F,
    ) -> anyhow::Result<()>
    where
        F: Future<Output = ()>,
    {
        crate::metrics::init_metrics();

        let event_store = match EventStore::new(&config.data_dir) {
            Ok(s) => {
                let s = Arc::new(s);
                info!(path = %config.data_dir.display(), "EventStore opened");
                Some(s)
            }
            Err(e) => {
                tracing::warn!("EventStore unavailable — at-least-once delivery disabled: {e}");
                None
            }
        };

        let event_bus = if let Some(store) = &event_store {
            Arc::new(EventBus::with_store(Arc::clone(store)))
        } else {
            event_bus
        };

        let (router_tx, router_rx) = mpsc::channel(1024);
        let ws_router_tx = router_tx.clone();
        let (_server_handle, disconnect_rx) =
            UdsServer::start(Path::new(&config.socket_path), router_tx).await?;
        let (ws_disconnect_tx, ws_disconnect_rx) = mpsc::channel::<u64>(64);
        info!("UDS server listening on {}", config.socket_path);

        let jwt_validator = config.jwt_secret.as_deref().map(|s| {
            info!("JWT auth enabled");
            Arc::new(JwtValidator::new(s.as_bytes()))
        });
        if jwt_validator.is_none() {
            if !config.allow_no_auth {
                anyhow::bail!(
                    "refusing to start without authentication: set `jwt_secret`, or set \
                     `allow_no_auth: true` in config to run without auth (insecure)"
                );
            }
            tracing::warn!(
                "JWT auth DISABLED (allow_no_auth) — any local process can register as any \
                 plugin; do not use in production"
            );
        }

        let kernel_start = std::time::Instant::now();
        let config_path = config.config_file.clone();
        tokio::spawn(MessageRouter::run_with_context(
            router_rx,
            Arc::clone(&registry),
            Arc::clone(&event_bus),
            jwt_validator.clone(),
            kernel_start,
            config_path,
            event_store.clone(),
        ));

        // disconnect handler: unregister plugin + publish system.plugin_left
        let disc_registry = Arc::clone(&registry);
        let disc_bus = Arc::clone(&event_bus);
        tokio::spawn(Self::disconnect_loop(
            disconnect_rx,
            disc_registry,
            disc_bus,
        ));

        // WS disconnect handler (same logic, separate channel)
        let ws_disc_registry = Arc::clone(&registry);
        let ws_disc_bus = Arc::clone(&event_bus);
        tokio::spawn(Self::disconnect_loop(
            ws_disconnect_rx,
            ws_disc_registry,
            ws_disc_bus,
        ));

        // at-least-once delivery retry worker
        if let Some(store) = event_store {
            let retry_bus = Arc::clone(&event_bus);
            let retry_reg = Arc::clone(&registry);
            tokio::spawn(run_retry_worker(store, retry_bus, retry_reg));
        }

        let supervisor = Arc::new(PluginSupervisor::with_events(
            &config.socket_path,
            config.log_buffer_lines,
            Some(Arc::clone(&event_bus)),
            Some(Arc::clone(&registry)),
        ));
        let sup_loop = Arc::clone(&supervisor);
        tokio::spawn(async move { sup_loop.monitor_loop().await });

        let watchdog_sup = Arc::clone(&supervisor);
        let watchdog_reg = Arc::clone(&registry);
        let watchdog_interval =
            std::time::Duration::from_secs(config.watchdog_interval_secs);
        let watchdog_timeout =
            std::time::Duration::from_secs(config.watchdog_timeout_secs);
        tokio::spawn(async move {
            watchdog_sup
                .watchdog_loop(watchdog_reg, watchdog_interval, watchdog_timeout)
                .await
        });

        let manager = Arc::new(PluginManager::new(supervisor, Arc::clone(&registry)));
        PluginLoader::load_all(&config.plugins, &manager).await;
        let api = ApiServer::new(
            config.port,
            manager,
            jwt_validator.clone(),
            Some(ws_router_tx),
            Some(ws_disconnect_tx),
            kernel_start,
        );
        tokio::spawn(async move {
            if let Err(e) = api.run().await {
                error!("HTTP API error: {e}");
            }
        });

        info!("kernel ready");
        shutdown.await;
        info!("shutdown signal received");

        Self::graceful_shutdown(&registry).await;
        Ok(())
    }

    async fn disconnect_loop(
        mut rx: mpsc::Receiver<u64>,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
    ) {
        while let Some(conn_id) = rx.recv().await {
            let plugin_id = match registry.get_by_conn_id(conn_id) {
                Some(e) => e.plugin_id.clone(),
                None => continue,
            };

            event_bus
                .publish(
                    Event {
                        event_id: format!("sys-left-{plugin_id}"),
                        event_type: "system.plugin_left".to_string(),
                        payload_json: format!(r#"{{"plugin_id":"{plugin_id}"}}"#).into_bytes(),
                        retry_count: 0,
                    },
                    &registry,
                )
                .await;

            event_bus.unsubscribe_all(&plugin_id);
            registry.unregister(&plugin_id);
        }
    }

    async fn graceful_shutdown(registry: &PluginRegistry) {
        let entries = registry.list();
        if entries.is_empty() {
            return;
        }

        let mut payload = Vec::new();
        let env = Envelope {
            payload: Some(envelope::Payload::PluginShutdown(PluginShutdown {
                reason: "kernel shutdown".to_string(),
                grace_seconds: 5,
            })),
            ..Default::default()
        };
        if env.encode(&mut payload).is_err() {
            return;
        }
        let crc = crc32fast::hash(&payload);
        let mut target = [0u8; 32];
        target[..4].copy_from_slice(b"self");

        let frame = Frame {
            magic: 0x5652,
            flags: 0,
            length: payload.len() as u32,
            target,
            crc32: crc,
            payload,
        };

        for entry in entries {
            let _ = entry.write_tx.send(frame.clone()).await;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
