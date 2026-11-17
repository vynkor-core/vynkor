use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::api::server::ApiServer;
use crate::auth::jwt::{JwtValidator, MIN_JWT_SECRET_BYTES};
use crate::bridge::{Bridge, BridgeHandle, BridgeStatus};
use crate::events::bus::{run_retry_worker, EventBus};
use crate::events::store::EventStore;
use crate::ipc::protocol::MessageRouter;
use crate::ipc::server::UdsServer;
use crate::plugins::lazy::{LazyStarter, ManagerStarter};
use crate::plugins::loader::PluginLoader;
use crate::plugins::manager::PluginManager;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;
use crate::utils::config::{resolve_device_id, Config, Role};

mod shutdown;

/// K-04: everything `graceful_shutdown` needs to close cleanly instead of
/// relying on process-exit to tear these down. `background` covers loops
/// with no cooperative stop signal (disconnect handlers, retry worker,
/// watchdog, monitor, router, bridge) — they hold no state that needs
/// flushing, so `abort()` is sufficient once upstream sources (plugins, UDS)
/// are already shut off.
pub(super) struct ShutdownHandles {
    pub(super) uds_accept: tokio::task::JoinHandle<()>,
    /// the lazy waker, if any lazy plugin is configured; aborted with the
    /// accept loop so nothing is woken once teardown begins
    pub(super) lazy: Option<tokio::task::JoinHandle<()>>,
    pub(super) api_shutdown_handle: axum_server::Handle<std::net::SocketAddr>,
    pub(super) api_task: tokio::task::JoinHandle<()>,
    pub(super) background: Vec<tokio::task::JoinHandle<()>>,
}

/// embedder hooks; `Default` = none (the cli adds `plugins_reload` on SIGHUP)
#[derive(Default)]
pub struct RunOptions {
    /// receives the bridge connection status (`Disabled` when there is no bridge)
    pub bridge_status: Option<tokio::sync::watch::Sender<BridgeStatus>>,
    /// notify_waiters() on it = reconnect the bridge now instead of after the backoff
    pub bridge_retry: Option<std::sync::Arc<tokio::sync::Notify>>,
    /// notify_one() on it = re-read the config file and start, stop or
    /// restart the plugins whose definitions changed (plugins.d hot-load)
    pub plugins_reload: Option<std::sync::Arc<tokio::sync::Notify>>,
}

pub struct Kernel;

impl Kernel {
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
        Self::run_with_options(config, registry, event_bus, shutdown, RunOptions::default()).await
    }

    pub async fn run_with_options<F>(
        config: Config,
        registry: Arc<PluginRegistry>,
        event_bus: Arc<EventBus>,
        shutdown: F,
        opts: RunOptions,
    ) -> anyhow::Result<()>
    where
        F: Future<Output = ()>,
    {
        // library entry point, not just `main`: tests and embedders reach the
        // tls server/bridge through here without ever running the binary
        crate::utils::tls::install_crypto_provider();
        crate::metrics::init_metrics();
        crate::plugins::runner::set_cgroup_instance(&config.pid_file);

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

        // attach, don't replace: callers hold this Arc (tests publish through it)
        if let Some(store) = &event_store {
            event_bus.set_store(Arc::clone(store));
        }

        let (router_tx, router_rx) = mpsc::channel(config.router_channel_capacity);
        let ws_router_tx = router_tx.clone();
        let (uds_accept_handle, disconnect_rx) = UdsServer::start(
            Path::new(&config.socket_path),
            router_tx,
            config.max_connections,
            config.fragment_timeout_secs,
            config.max_reassembly_streams,
        )
        .await?;
        let (ws_disconnect_tx, ws_disconnect_rx) = mpsc::channel::<u64>(64);
        info!("UDS server listening on {}", config.socket_path);

        if let Some(secret) = config.jwt_secret.as_deref() {
            if secret.len() < MIN_JWT_SECRET_BYTES {
                anyhow::bail!(
                    "jwt_secret is {} bytes, must be at least {MIN_JWT_SECRET_BYTES} bytes \
                     (HS256 secrets shorter than this are brute-forceable)",
                    secret.len()
                );
            }
        }
        let jwt_validator = config.jwt_secret.as_deref().map(|s| {
            info!("JWT auth enabled");
            Arc::new(JwtValidator::with_audience(
                s.as_bytes(),
                config.jwt_audience.clone(),
            ))
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

        // D-07: TLS is on by default; resolve (or auto-generate) the cert pair
        // before any listener starts so a half-configured tls_* fails the boot
        // instead of silently downgrading to plaintext.
        let (tls_cert_path, tls_key_path) = crate::utils::tls::resolve_tls_paths(&config)?;

        // D-07: a host that actually serves remote devices has auth on — bind
        // beyond loopback only then. With allow_no_auth the API stays local
        // regardless of role (exposing an unauthenticated control plane would
        // be a footgun), overridable via explicit `bind:`.
        let bind_ip: std::net::IpAddr = if let Some(bind) = &config.bind {
            bind.parse()
                .map_err(|_| anyhow::anyhow!("invalid bind address '{bind}'"))?
        } else if config.role == Role::Host && config.jwt_secret.is_some() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
        } else {
            if config.role == Role::Host {
                warn!(
                    "role: host without jwt_secret — API stays bound to 127.0.0.1; \
                     set jwt_secret to expose the network path to remote devices"
                );
            }
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        };

        let kernel_start = std::time::Instant::now();
        let config_path = config.config_file.clone();
        // Frame-MAC key material: the JWT secret, or None when running without
        // auth (then frames are CRC-only, unchanged). The router never MACs a
        // local plugin with it directly: each gets plugin_mac_secret(this,
        // plugin_id) unless legacy_plugin_mac is set.
        let mac_secret = config
            .jwt_secret
            .as_ref()
            .map(|s| Arc::new(s.as_bytes().to_vec()));
        // E-01: per-device credential store (encrypted at rest under a key
        // derived from jwt_secret). Always present when auth is on; the router
        // and the WS gateway only consult it for device-scoped connections.
        let device_store = config.jwt_secret.as_ref().map(|s| {
            Arc::new(crate::auth::device_store::DeviceStore::new(
                &config.data_dir,
                s,
            ))
        });
        // the configured plugin set, swapped by a reload. also T-04's
        // per-plugin permission allowlist: registration clamps JWT-claimed
        // permissions to it (see protocol.rs)
        let plugin_defs = crate::plugins::reload::SharedDefs::from(config.plugins.clone());
        // D-06: in client role, mirror the configured capabilities to the host
        // and hand the router the relay handle (frames whose target is not in
        // the local registry fall through to the host).
        let mut background_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        let mut bridge_status = opts.bridge_status;
        let bridge_handle = if config.role == Role::Client {
            if let Some(bridge_cfg) = &config.bridge {
                let mut handle = match bridge_status.take() {
                    Some(tx) => BridgeHandle::with_status(tx),
                    None => BridgeHandle::new(),
                };
                if let Some(retry) = opts.bridge_retry.clone() {
                    handle = handle.with_retry(retry);
                }
                let bridge = Bridge::new(
                    bridge_cfg.clone(),
                    resolve_device_id(&config),
                    Arc::clone(&registry),
                    ws_router_tx.clone(),
                    handle.clone(),
                );
                background_handles.push(tokio::spawn(bridge.run()));
                Some(handle)
            } else {
                warn!("role: client without a bridge block — running host-local only");
                None
            }
        } else {
            None
        };
        // no bridge started: it never will, tell the embedder so it can stop waiting
        if let Some(tx) = bridge_status {
            tx.send_replace(BridgeStatus::Disabled);
        }
        // lazy plugins: the router queues first calls here; the LazyStarter
        // (spawned once the plugin manager exists) wakes the plugin
        let (lazy_tx, lazy_rx) = mpsc::channel(64);
        background_handles.push(tokio::spawn(MessageRouter::run_with_context(
            router_rx,
            Arc::clone(&registry),
            Arc::clone(&event_bus),
            jwt_validator.clone(),
            kernel_start,
            config_path,
            event_store.clone(),
            mac_secret,
            config.legacy_plugin_mac,
            Some(plugin_defs.clone()),
            config.ipc_rate_limit_rps,
            config.action_caller_rate_limit_rps,
            config.action_caller_max_concurrent,
            config.action_timeout_ms,
            config.max_conn_errors,
            config.max_tracked_error_conns,
            config.session_idle_timeout_secs,
            config.prune_interval_secs,
            device_store.clone(),
            bridge_handle,
            config.device_callers.clone().map(Arc::new),
            Some(lazy_tx),
        )));

        // disconnect handler: unregister plugin + publish system.plugin_left
        let disc_registry = Arc::clone(&registry);
        let disc_bus = Arc::clone(&event_bus);
        background_handles.push(tokio::spawn(Self::disconnect_loop(
            disconnect_rx,
            disc_registry,
            disc_bus,
        )));

        // WS disconnect handler (same logic, separate channel)
        let ws_disc_registry = Arc::clone(&registry);
        let ws_disc_bus = Arc::clone(&event_bus);
        background_handles.push(tokio::spawn(Self::disconnect_loop(
            ws_disconnect_rx,
            ws_disc_registry,
            ws_disc_bus,
        )));

        // at-least-once delivery retry worker
        if let Some(store) = event_store {
            let retry_bus = Arc::clone(&event_bus);
            let retry_reg = Arc::clone(&registry);
            background_handles.push(tokio::spawn(run_retry_worker(
                store,
                retry_bus,
                retry_reg,
                config.event_max_retries,
                config.event_retention_secs,
            )));
        }

        let mut supervisor = PluginSupervisor::with_events(
            &config.socket_path,
            config.log_buffer_lines,
            Some(Arc::clone(&event_bus)),
            Some(Arc::clone(&registry)),
            config.restart_backoff_base_ms,
            config.restart_backoff_max_ms,
        );
        supervisor.set_data_dir(config.data_dir.clone());
        // local plugins get plugin_mac_secret(jwt_secret, plugin_id) as
        // VYN_JWT_SECRET, never the master itself
        supervisor.set_plugin_mac(
            config
                .jwt_secret
                .as_ref()
                .map(|s| Arc::new(s.as_bytes().to_vec())),
            config.legacy_plugin_mac,
        );
        let supervisor = Arc::new(supervisor);
        let sup_loop = Arc::clone(&supervisor);
        background_handles.push(tokio::spawn(async move { sup_loop.monitor_loop().await }));

        let watchdog_sup = Arc::clone(&supervisor);
        let watchdog_reg = Arc::clone(&registry);
        let watchdog_interval = std::time::Duration::from_secs(config.watchdog_interval_secs);
        let watchdog_timeout = std::time::Duration::from_secs(config.watchdog_timeout_secs);
        background_handles.push(tokio::spawn(async move {
            watchdog_sup
                .watchdog_loop(watchdog_reg, watchdog_interval, watchdog_timeout)
                .await
        }));

        let shutdown_supervisor = Arc::clone(&supervisor);
        let manager = Arc::new(PluginManager::new(supervisor, Arc::clone(&registry)));
        let lazy_configs = PluginLoader::load_all(
            &config.plugins,
            &manager,
            Some(&event_bus),
            config.lazy_idle_stop_secs,
        )
        .await;
        // the waker runs even with no lazy plugin: a reload may add one
        let lazy_starter = Arc::new(ManagerStarter::new(Arc::clone(&manager), lazy_configs));
        let lazy_handle = Some(tokio::spawn(
            LazyStarter::new(
                Arc::clone(&registry),
                Arc::clone(&lazy_starter) as Arc<dyn crate::plugins::lazy::PluginStarter>,
                ws_router_tx.clone(),
                std::time::Duration::from_millis(config.lazy_start_timeout_ms),
            )
            .run(lazy_rx),
        ));
        // plugins.d hot-load: SIGHUP or the embedder's notify re-reads the
        // config and starts/stops/restarts what changed
        if let Some(trigger) = opts.plugins_reload.clone() {
            let reloader = Arc::new(crate::plugins::reload::Reloader::new(
                Arc::clone(&manager),
                Arc::clone(&event_bus),
                plugin_defs.clone(),
                lazy_starter,
                config.lazy_idle_stop_secs,
                config.config_file.clone(),
            ));
            background_handles.push(tokio::spawn(reloader.run(trigger)));
        }
        // CD-01: ticket pairing shares the device store; needs jwt_secret to
        // sign device tokens, so it exists exactly when the store does
        let pairing = match (&device_store, &config.jwt_secret) {
            (Some(store), Some(secret)) => {
                Some(Arc::new(crate::auth::pairing::PairingService::new(
                    &config.data_dir,
                    Arc::clone(store),
                    crate::auth::pairing::PairingConfig {
                        jwt_secret: secret.clone(),
                        audience: config
                            .jwt_audience
                            .clone()
                            .unwrap_or_else(|| "vynkor".to_string()),
                        port: config.port,
                        tls: config.tls,
                        cert_path: tls_cert_path.clone(),
                        device_ttl_secs: 86_400,
                    },
                )))
            }
            _ => None,
        };
        let api = ApiServer::new(
            config.port,
            bind_ip,
            manager,
            jwt_validator.clone(),
            device_store,
            Some(ws_router_tx),
            Some(ws_disconnect_tx),
            kernel_start,
            config.api_rate_limit_rps,
            config.api_rate_limit_burst,
            tls_cert_path,
            tls_key_path,
            plugin_defs.clone(),
            config.ws_handshake_timeout_secs,
            config.max_ws_connections,
            config.ws_register_timeout_secs,
        )
        .with_pairing(pairing)
        .with_device_recheck_secs(config.ws_device_recheck_secs);
        // K-04: kept outside the spawned task so graceful_shutdown can signal
        // it (axum-server's Handle is the drain/stop switch for the listener).
        let api_shutdown_handle = axum_server::Handle::new();
        let api_task_handle = api_shutdown_handle.clone();
        // A gateway that cannot serve (port taken, bad TLS files) used to be
        // only logged while the kernel ran on without it: devices and the
        // CLI then looked "connected but silent". Its failure now stops the
        // kernel with that error.
        let (api_failed_tx, api_failed_rx) = tokio::sync::oneshot::channel::<String>();
        let api_task = tokio::spawn(async move {
            if let Err(e) = api.run(api_task_handle).await {
                error!("HTTP API error: {e}");
                let _ = api_failed_tx.send(e.to_string());
            }
        });

        info!("kernel ready");
        let api_failure = tokio::select! {
            _ = shutdown => {
                info!("shutdown signal received");
                None
            }
            Ok(e) = api_failed_rx => {
                error!("HTTP API stopped ({e}); shutting the kernel down");
                Some(e)
            }
        };

        Self::graceful_shutdown(
            &registry,
            &shutdown_supervisor,
            config.default_grace_seconds,
            ShutdownHandles {
                uds_accept: uds_accept_handle,
                lazy: lazy_handle,
                api_shutdown_handle,
                api_task,
                background: background_handles,
            },
        )
        .await;
        match api_failure {
            Some(e) => Err(anyhow::anyhow!("HTTP API failed: {e}")),
            None => Ok(()),
        }
    }
}
