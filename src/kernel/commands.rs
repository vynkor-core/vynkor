use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::CommandStatus;
use crate::utils::config::load_config;
use crate::utils::logging::set_log_level;
use std::time::Instant;

/// Outcome of a kernel command: protobuf status code, JSON data payload, error text.
pub struct CommandOutcome {
    pub status: CommandStatus,
    pub data_json: Vec<u8>,
    pub error: String,
}

impl CommandOutcome {
    fn ok(data: String) -> Self {
        Self {
            status: CommandStatus::CommandOk,
            data_json: data.into_bytes(),
            error: String::new(),
        }
    }

    fn err(status: CommandStatus, error: String) -> Self {
        Self {
            status,
            data_json: vec![],
            error,
        }
    }

    pub fn permission_denied(error: String) -> Self {
        Self::err(CommandStatus::CommandPermissionDenied, error)
    }
}

/// Executes kernel control commands (health checks, config reload).
///
/// This owns the *semantics* of kernel commands. The IPC router only routes the
/// message here and serializes the outcome back onto the wire — command business
/// logic does not belong in the transport layer.
pub struct CommandHandler;

impl CommandHandler {
    pub fn dispatch(
        command: &str,
        registry: &PluginRegistry,
        start_time: Instant,
        config_path: Option<&str>,
    ) -> CommandOutcome {
        match command {
            "health_check" => {
                let uptime_secs = start_time.elapsed().as_secs();
                let plugin_count = registry.list().len();
                CommandOutcome::ok(format!(
                    r#"{{"uptime_secs":{uptime_secs},"plugin_count":{plugin_count}}}"#
                ))
            }
            // D-04: discovery surface — same data as GET /devices, served over
            // the IPC KernelCommand path for PERMISSION_KERNEL_ADMIN holders.
            "list_devices" => {
                let devices: Vec<serde_json::Value> = registry
                    .list_devices()
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "device_id": d.device_id,
                            "os": crate::plugins::registry::device_os_str(d.os),
                            "arch": d.arch,
                            "os_version": d.os_version,
                            "capabilities": d.capabilities,
                            "last_seen": d.last_seen,
                            "state": crate::plugins::registry::device_state_str(d.state),
                        })
                    })
                    .collect();
                CommandOutcome::ok(serde_json::Value::Array(devices).to_string())
            }
            "reload_config" => match config_path {
                Some(path) => match load_config(path) {
                    Ok(cfg) => {
                        let mut reloaded: Vec<&str> = vec![];

                        if set_log_level(&cfg.log_level) {
                            reloaded.push("log_level");
                        }

                        // Fields that require restart are logged and skipped.
                        // socket_path, jwt_secret, tls_cert_path, tls_key_path
                        // require a full kernel restart to apply.

                        let items = reloaded.join("\",\"");
                        CommandOutcome::ok(format!(
                            r#"{{"reloaded":["{items}"],"log_level":"{}","port":{}}}"#,
                            cfg.log_level, cfg.port
                        ))
                    }
                    Err(e) => CommandOutcome::err(
                        CommandStatus::CommandError,
                        format!("config reload failed: {e}"),
                    ),
                },
                None => CommandOutcome::err(
                    CommandStatus::CommandError,
                    "no config path configured".to_string(),
                ),
            },
            other => CommandOutcome::err(
                CommandStatus::CommandUnknown,
                format!("unknown command: {other}"),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::registry::{DeviceMeta, DeviceOs};
    use crate::proto::veyron::PluginManifest;
    use tokio::sync::mpsc;

    fn dummy_tx() -> mpsc::Sender<crate::ipc::connection::Outbound> {
        mpsc::channel(1).0
    }

    fn register_device(registry: &PluginRegistry, plugin_id: &str, conn_id: u64, device_id: &str) {
        registry
            .register_with_device(
                plugin_id.to_string(),
                conn_id,
                PluginManifest::default(),
                dummy_tx(),
                DeviceMeta {
                    device_id: device_id.to_string(),
                    user_id: "default".to_string(),
                    os: DeviceOs::Android,
                    arch: "aarch64".to_string(),
                    os_version: "14".to_string(),
                    capabilities: vec!["geo".to_string(), "battery".to_string()],
                },
            )
            .unwrap();
    }

    #[test]
    fn health_check_reports_plugin_count() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("health_check", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandOk);
        assert!(out.error.is_empty());
        let json = String::from_utf8(out.data_json).unwrap();
        assert!(json.contains("\"plugin_count\":0"));
        assert!(json.contains("uptime_secs"));
    }

    #[test]
    fn health_check_counts_registered_plugin() {
        let registry = PluginRegistry::new();
        registry
            .register(
                "alpha".to_string(),
                1,
                PluginManifest::default(),
                dummy_tx(),
                "",
                "",
            )
            .unwrap();
        let out = CommandHandler::dispatch("health_check", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandOk);
        let json = String::from_utf8(out.data_json).unwrap();
        assert!(json.contains("\"plugin_count\":1"), "json={json}");
    }

    #[test]
    fn reload_config_without_path_errors() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("reload_config", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandError);
        assert_eq!(out.error, "no config path configured");
    }

    #[test]
    fn reload_config_with_invalid_path_returns_error() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch(
            "reload_config",
            &registry,
            Instant::now(),
            Some("/nonexistent/veyron_cfg_test.yaml"),
        );
        assert_eq!(out.status, CommandStatus::CommandError);
        assert!(
            out.error.contains("config reload failed"),
            "error={}",
            out.error
        );
    }

    #[test]
    fn reload_config_with_valid_path_returns_ok() {
        let tmp = std::env::temp_dir().join(format!("veyron_cmd_test_{}.yaml", std::process::id()));
        std::fs::write(
            &tmp,
            b"port: 9001\nlog_level: info\npid_file: /tmp/v.pid\nlog_file: /tmp/v.log\ndata_dir: /tmp/v_data\n",
        )
        .unwrap();
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch(
            "reload_config",
            &registry,
            Instant::now(),
            Some(tmp.to_str().unwrap()),
        );
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(out.status, CommandStatus::CommandOk, "error={}", out.error);
        let json = String::from_utf8(out.data_json).unwrap();
        assert!(json.contains("\"port\":9001"), "json={json}");
        assert!(json.contains("\"log_level\":\"info\""), "json={json}");
    }

    #[test]
    fn unknown_command_reports_unknown() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("does_not_exist", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandUnknown);
        assert_eq!(out.error, "unknown command: does_not_exist");
    }

    #[test]
    fn list_devices_returns_empty_array_when_no_devices() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("list_devices", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandOk);
        assert_eq!(String::from_utf8(out.data_json).unwrap(), "[]");
    }

    #[test]
    fn list_devices_returns_registered_device_fields() {
        let registry = PluginRegistry::new();
        register_device(&registry, "geo", 1, "phone-1");
        let out = CommandHandler::dispatch("list_devices", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandOk);
        let json = String::from_utf8(out.data_json).unwrap();
        assert!(json.contains("\"device_id\":\"phone-1\""), "json={json}");
        assert!(json.contains("\"os\":\"android\""), "json={json}");
        assert!(json.contains("\"arch\":\"aarch64\""), "json={json}");
        assert!(json.contains("\"os_version\":\"14\""), "json={json}");
        assert!(
            json.contains("\"capabilities\":[\"geo\",\"battery\"]"),
            "json={json}"
        );
        assert!(json.contains("\"state\":\"online\""), "json={json}");
        assert!(json.contains("\"last_seen\":"), "json={json}");
    }
}
