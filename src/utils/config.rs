use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub port: u16,
    pub log_level: String,
    pub pid_file: PathBuf,
    pub log_file: PathBuf,
    #[allow(dead_code)]
    pub data_dir: PathBuf,
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
}

fn default_socket_path() -> String {
    "/tmp/veyron.sock".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8000,
            log_level: "info".to_string(),
            pid_file: PathBuf::from("/tmp/veyron.pid"),
            log_file: PathBuf::from("/tmp/veyron.log"),
            data_dir: PathBuf::from("/var/lib/veyron"),
            socket_path: default_socket_path(),
        }
    }
}

pub fn load_config(path: &str) -> anyhow::Result<Config> {
    let content = std::fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&content)?;
    Ok(config)
}
