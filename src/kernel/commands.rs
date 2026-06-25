use crate::plugins::registry::PluginRegistry;
use crate::proto::veyron::CommandStatus;
use crate::utils::config::load_config;
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
            "reload_config" => match config_path {
                Some(path) => match load_config(path) {
                    Ok(cfg) => CommandOutcome::ok(format!(
                        r#"{{"log_level":"{}","port":{}}}"#,
                        cfg.log_level, cfg.port
                    )),
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
    fn reload_config_without_path_errors() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("reload_config", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandError);
        assert_eq!(out.error, "no config path configured");
    }

    #[test]
    fn unknown_command_reports_unknown() {
        let registry = PluginRegistry::new();
        let out = CommandHandler::dispatch("does_not_exist", &registry, Instant::now(), None);
        assert_eq!(out.status, CommandStatus::CommandUnknown);
        assert_eq!(out.error, "unknown command: does_not_exist");
    }
}
