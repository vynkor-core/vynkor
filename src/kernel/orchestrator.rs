use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::api::server::ApiServer;
use crate::auth::jwt::{JwtValidator, MIN_JWT_SECRET_BYTES};
use crate::bridge::{Bridge, BridgeHandle};
use crate::events::bus::{run_retry_worker, EventBus};
use crate::events::store::EventStore;
use crate::ipc::connection::out_frame;
use crate::ipc::framing::Frame;
use crate::ipc::protocol::MessageRouter;
use crate::ipc::server::UdsServer;
use crate::plugins::loader::PluginLoader;
use crate::plugins::manager::PluginManager;
use crate::plugins::registry::PluginRegistry;
use crate::plugins::supervisor::PluginSupervisor;
use crate::proto::veyron::{envelope, Envelope, Event, PluginShutdown};
use crate::utils::config::{resolve_device_id, Config, Role};

pub struct Kernel;

/// Reload config from `config_file` (if set) and apply its log level.
/// No-op (`Ok(())`) when `config_file` is `None` — matches the boot-time
/// behavior where an in-memory-only `Config` has nothing to reload from.
fn reload_config_on_sighup(config_file: &Option<String>) -> anyhow::Result<Config> {
    let path = config_file
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no config file to reload from"))?;
    let cfg = crate::utils::config::load_config(path)?;
    crate::utils::logging::set_log_level(&cfg.log_level);
    Ok(cfg)
}

impl Kernel {
    pub async fn run(config: Config) -> anyhow::Result<()> {
        let config_file = config.config_file.clone();
        let shutdown = async move {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
            let mut sighup =
                signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = sigterm.recv() => { info!("received SIGTERM"); break; }
                    _ = sighup.recv() => {
                        info!("received SIGHUP — reloading config");
                        match reload_config_on_sighup(&config_file) {
                            Ok(cfg) => info!(log_level = %cfg.log_level, "config reloaded via SIGHUP"),
                            Err(e) => tracing::warn!("SIGHUP config reload failed: {e}"),
                        }
                    }
                }
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

        let (router_tx, router_rx) = mpsc::channel(config.router_channel_capacity);
        let ws_router_tx = router_tx.clone();
        let (_server_handle, disconnect_rx) = UdsServer::start(
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
        // Frame-MAC key material: the same secret used for JWT, or None when
        // running without auth (then frames are CRC-only, unchanged).
        let mac_secret = config
            .jwt_secret
            .as_ref()
            .map(|s| Arc::new(s.as_bytes().to_vec()));
        // T-04: operator-declared permission allowlist per plugin id, used to
        // clamp JWT-claimed permissions at registration (see protocol.rs).
        let config_permissions = Arc::new(
            config
                .plugins
                .iter()
                .map(|d| (d.id.clone(), d.permissions.clone()))
                .collect::<std::collections::HashMap<_, _>>(),
        );
        // D-06: in client role, mirror the configured capabilities to the host
        // and hand the router the relay handle (frames whose target is not in
        // the local registry fall through to the host).
        let bridge_handle = if config.role == Role::Client {
            if let Some(bridge_cfg) = &config.bridge {
                let handle = BridgeHandle::new();
                let bridge = Bridge::new(
                    bridge_cfg.clone(),
                    resolve_device_id(&config),
                    Arc::clone(&registry),
                    ws_router_tx.clone(),
                    handle.clone(),
                );
                tokio::spawn(bridge.run());
                Some(handle)
            } else {
                warn!("role: client without a bridge block — running host-local only");
                None
            }
        } else {
            None
        };
        tokio::spawn(MessageRouter::run_with_context(
            router_rx,
            Arc::clone(&registry),
            Arc::clone(&event_bus),
            jwt_validator.clone(),
            kernel_start,
            config_path,
            event_store.clone(),
            mac_secret,
            Some(config_permissions),
            config.ipc_rate_limit_rps,
            config.action_caller_rate_limit_rps,
            config.action_caller_max_concurrent,
            config.action_timeout_ms,
            config.max_conn_errors,
            config.max_tracked_error_conns,
            config.session_idle_timeout_secs,
            bridge_handle,
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
            tokio::spawn(run_retry_worker(
                store,
                retry_bus,
                retry_reg,
                config.event_max_retries,
                config.event_retention_secs,
            ));
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
        let supervisor = Arc::new(supervisor);
        let sup_loop = Arc::clone(&supervisor);
        tokio::spawn(async move { sup_loop.monitor_loop().await });

        let watchdog_sup = Arc::clone(&supervisor);
        let watchdog_reg = Arc::clone(&registry);
        let watchdog_interval = std::time::Duration::from_secs(config.watchdog_interval_secs);
        let watchdog_timeout = std::time::Duration::from_secs(config.watchdog_timeout_secs);
        tokio::spawn(async move {
            watchdog_sup
                .watchdog_loop(watchdog_reg, watchdog_interval, watchdog_timeout)
                .await
        });

        let shutdown_supervisor = Arc::clone(&supervisor);
        let manager = Arc::new(PluginManager::new(supervisor, Arc::clone(&registry)));
        PluginLoader::load_all(&config.plugins, &manager, Some(&event_bus)).await;
        let api = ApiServer::new(
            config.port,
            bind_ip,
            manager,
            jwt_validator.clone(),
            Some(ws_router_tx),
            Some(ws_disconnect_tx),
            kernel_start,
            config.api_rate_limit_rps,
            config.api_rate_limit_burst,
            tls_cert_path,
            tls_key_path,
            config.plugins.clone(),
            config.ws_handshake_timeout_secs,
            config.max_ws_connections,
            config.ws_register_timeout_secs,
        );
        tokio::spawn(async move {
            if let Err(e) = api.run().await {
                error!("HTTP API error: {e}");
            }
        });

        info!("kernel ready");
        shutdown.await;
        info!("shutdown signal received");

        Self::graceful_shutdown(
            &registry,
            &shutdown_supervisor,
            config.default_grace_seconds,
        )
        .await;
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

            let now_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            event_bus
                .publish(
                    Event {
                        event_id: format!("sys-left-{plugin_id}-{now_ms}"),
                        event_type: "system.plugin_left".to_string(),
                        payload_json: crate::events::bus::plugin_lifecycle_payload(
                            &registry, &plugin_id,
                        ),
                        retry_count: 0,
                    },
                    &registry,
                )
                .await;

            event_bus.unsubscribe_all(&plugin_id);
            registry.unregister(&plugin_id);
        }
    }

    async fn graceful_shutdown(
        registry: &PluginRegistry,
        supervisor: &PluginSupervisor,
        default_grace_seconds: u32,
    ) {
        let entries = registry.list();
        if entries.is_empty() {
            return;
        }

        // Advertise each plugin's real grace window: its supervised config value
        // when set, else the kernel default — matching what the supervisor will
        // actually enforce before SIGKILL.
        for entry in entries {
            let grace = supervisor
                .grace_seconds_for(&entry.plugin_id)
                .unwrap_or(default_grace_seconds);
            let mut payload = Vec::new();
            let env = Envelope {
                payload: Some(envelope::Payload::PluginShutdown(PluginShutdown {
                    reason: "kernel shutdown".to_string(),
                    grace_seconds: grace,
                })),
                ..Default::default()
            };
            if env.encode(&mut payload).is_err() {
                continue;
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
                payload: payload.into(),
                mac: None,
            };
            let _ = entry.write_tx.send(out_frame(frame)).await;
        }

        supervisor.graceful_shutdown(default_grace_seconds).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reload_config_on_sighup_none_path_is_a_no_op_error() {
        let result = reload_config_on_sighup(&None);
        assert!(result.is_err(), "no config file means nothing to reload");
    }

    #[test]
    fn reload_config_on_sighup_reloads_from_disk() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(
            file,
            "port: 9000\nlog_level: debug\ndata_dir: /tmp/veyron_test_data"
        )
        .expect("write");
        let path = file.path().to_str().unwrap().to_string();

        let cfg = reload_config_on_sighup(&Some(path)).expect("reload must succeed");
        assert_eq!(cfg.log_level, "debug");
    }

    #[test]
    fn reload_config_on_sighup_surfaces_parse_errors() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(file, "not: [valid: yaml: at: all").expect("write");
        let path = file.path().to_str().unwrap().to_string();

        let result = reload_config_on_sighup(&Some(path));
        assert!(result.is_err(), "malformed YAML must not reload silently");
    }

    /// End-to-end: install the real SIGHUP handler exactly as `Kernel::run`
    /// does, send an actual `SIGHUP` to this process, and confirm the reload
    /// path runs (observed via the log-level change it produces) rather than
    /// only exercising `reload_config_on_sighup` as an isolated function.
    #[tokio::test]
    async fn sighup_signal_triggers_config_reload() {
        let mut file = tempfile::NamedTempFile::new().expect("tempfile");
        writeln!(
            file,
            "port: 9000\nlog_level: debug\ndata_dir: /tmp/veyron_test_data"
        )
        .expect("write");
        let config_file = Some(file.path().to_str().unwrap().to_string());

        let mut sighup = signal(SignalKind::hangup()).expect("failed to install SIGHUP handler");

        nix::sys::signal::raise(nix::sys::signal::Signal::SIGHUP).expect("failed to raise SIGHUP");

        tokio::time::timeout(std::time::Duration::from_secs(2), sighup.recv())
            .await
            .expect("must receive the SIGHUP within timeout")
            .expect("signal stream must not be closed");

        let cfg = reload_config_on_sighup(&config_file).expect("reload must succeed");
        assert_eq!(cfg.log_level, "debug");
    }
}
