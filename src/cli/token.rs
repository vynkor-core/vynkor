use clap::Subcommand;

use crate::utils::config::load_config;

#[derive(Subcommand)]
pub enum TokenCmd {
    /// Mint a per-device JWT (D-07): sub=device_id, restricted claims,
    /// aud + jti nonce + short exp. Requires jwt_secret in the config file.
    Mint {
        /// Device id the token is bound to (the sub claim).
        #[arg(long)]
        device: String,
        /// Comma-separated restricted permissions. Default: IPC send + event
        /// publish (what a device agent needs to call host actions).
        #[arg(long)]
        permissions: Option<String>,
        /// Comma-separated ipc_targets allowlist for the token.
        #[arg(long)]
        ipc_targets: Option<String>,
        /// Token lifetime in seconds (short exp). Default: 86400 (24h).
        #[arg(long, default_value_t = 86400)]
        ttl_seconds: u64,
        /// Audience claim. Default: config jwt_audience, else "vynkor".
        #[arg(long)]
        aud: Option<String>,
    },
}

pub async fn handle(cmd: TokenCmd, config_path: &str) -> anyhow::Result<()> {
    let cfg = load_config(config_path).unwrap_or_default();
    let secret = cfg.jwt_secret.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret configured in '{config_path}' — set jwt_secret to mint tokens"
        )
    })?;
    match cmd {
        TokenCmd::Mint {
            device,
            permissions,
            ipc_targets,
            ttl_seconds,
            aud,
        } => {
            let perms = permissions.map(parse_csv).unwrap_or_else(|| {
                vec![
                    "PERMISSION_IPC_SEND".to_string(),
                    "PERMISSION_EVENT_PUBLISH".to_string(),
                ]
            });
            let targets = ipc_targets.map(parse_csv).unwrap_or_default();
            let audience = aud
                .or(cfg.jwt_audience)
                .unwrap_or_else(|| "vynkor".to_string());
            let token = crate::auth::jwt::mint_device_token(
                secret.as_bytes(),
                &device,
                perms,
                targets,
                ttl_seconds,
                &audience,
            )
            .map_err(anyhow::Error::msg)?;
            println!("{token}");
            Ok(())
        }
    }
}

fn parse_csv(s: String) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}
