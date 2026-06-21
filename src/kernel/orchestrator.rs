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
use crate::events::bus::EventBus;
use crate::ipc::framing::Frame;
use crate::ipc::protocol::MessageRouter;
use crate::ipc::server::UdsServer;
use crate::plugins::loader::PluginLoader;
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
        let (router_tx, router_rx) = mpsc::channel(1024);
        let (_server_handle, disconnect_rx) =
            UdsServer::start(Path::new(&config.socket_path), router_tx).await?;
        info!("UDS server listening on {}", config.socket_path);

        let jwt_validator = config.jwt_secret.as_deref().map(|s| {
            info!("JWT auth enabled");
            Arc::new(JwtValidator::new(s.as_bytes()))
        });
        if jwt_validator.is_none() {
            tracing::warn!("JWT auth disabled — set jwt_secret in config for production use");
        }

        tokio::spawn(MessageRouter::run(
            router_rx,
            Arc::clone(&registry),
            Arc::clone(&event_bus),
            jwt_validator,
        ));

        // disconnect handler: unregister plugin + publish system.plugin_left
        let disc_registry = Arc::clone(&registry);
        let disc_bus = Arc::clone(&event_bus);
        tokio::spawn(Self::disconnect_loop(
            disconnect_rx,
            disc_registry,
            disc_bus,
        ));

        let supervisor = Arc::new(PluginSupervisor::with_log_lines(
            &config.socket_path,
            config.log_buffer_lines,
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

        PluginLoader::load_all(&config.plugins, &supervisor).await;

        let api = ApiServer::new(config.port, Arc::clone(&registry), supervisor);
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
