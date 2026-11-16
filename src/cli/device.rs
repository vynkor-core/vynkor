//! `vyn device connect` — pair a remote device agent (the vynkor Android app)
//! by printing a `vynkor://pair` link that carries everything the agent needs
//! to join the host: host URL, device id, per-device JWT, the device's OWN
//! frame-MAC secret, and (when TLS is on) the served cert for pinning.
//!
//! E-01: the host issues a unique per-device secret at pair time and stores it
//! encrypted in `<data_dir>/devices.json`; the master jwt_secret never leaves
//! the host.
//!
//! K-05: QR-code rendering (terminal + SVG) used to live here too, but it's
//! pure onboarding UX, not pairing-protocol logic — moved to the standalone
//! `vyn-pair` binary (`src/bin/vyn-pair.rs`), which reads the printed link
//! from here and renders it. This binary just prints the link/token; it never
//! links the `qrcode` crate.
//!
//! Lifecycle companions: `vyn device list`, `vyn device revoke`, `vyn device
//! remove`. Revocation takes effect on a running kernel immediately — the
//! router re-reads the store on every registration and the WS gateway on every
//! upgrade.

use clap::Subcommand;

use crate::auth::device_store::{record_grants, DeviceStatus, DeviceStore};
use crate::auth::grants::{Grant, Grants, Preset};
use crate::auth::pairing::{
    default_device_permissions, encode_pair_link, PairPayload, TicketLinkPayload, TicketView,
};
use crate::utils::config::{effective_tls_cert_path, load_config, Config};
use crate::utils::url::{is_loopback_url, resolve_advertise_url};

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Issue a per-device credential and print the `vynkor://pair` link the
    /// vynkor Android app scans (via QR) to configure itself and connect.
    /// Requires `jwt_secret`. Pipe the printed link to `vyn-pair` to render
    /// it as a QR code (terminal or SVG) — this command only prints text.
    Connect {
        /// Device id (the JWT `sub`). Default: auto-generated.
        #[arg(long)]
        device: Option<String>,
        /// Display name saved on the device. Default: the device id.
        #[arg(long)]
        name: Option<String>,
        /// Host address the phone connects to — LAN IP, Tailscale name/100.x,
        /// or a full `ws(s)://` URL. Default: auto-detect LAN IP + config port.
        #[arg(long)]
        host: Option<String>,
        /// Comma-separated restricted permissions (same as `vyn token mint`).
        #[arg(long)]
        permissions: Option<String>,
        /// Comma-separated ipc_targets allowlist.
        #[arg(long)]
        ipc_targets: Option<String>,
        /// Credential lifetime in seconds — bounds BOTH the token exp and the
        /// stored row. Default: 86400 (24h).
        #[arg(long, default_value_t = 86400)]
        ttl_seconds: u64,
        /// Audience claim. Default: config `jwt_audience`, else "vynkor".
        #[arg(long)]
        aud: Option<String>,
        /// Grant preset: guest (agent only), user (view/logs/control plugins)
        /// or owner (everything). Default: `user` for a new device; a re-pair
        /// of an existing device id keeps its current grants.
        #[arg(long)]
        preset: Option<Preset>,
    },
    /// Mint a single-use pairing ticket on the running kernel (CD-01) and
    /// print a `vynkor://pair` link carrying just `{v, ws, ticket, cert_pem?}`.
    /// The app trades the ticket for its own credential via
    /// `POST /devices/consume` — no secrets in the QR. Pipe to `vyn-pair`
    /// for a QR code.
    Pair {
        /// Ticket lifetime: seconds, or with a unit (`90s`, `5m`, `1h`).
        /// Clamped to 1h by the kernel.
        #[arg(long, default_value = "5m", value_parser = parse_duration_secs)]
        ttl: u64,
        /// Display name saved on the device once it pairs.
        #[arg(long)]
        name: Option<String>,
        /// Host address the phone connects to — LAN IP, Tailscale name/100.x,
        /// or a full `ws(s)://` URL. Default: kernel auto-detects its LAN IP.
        #[arg(long)]
        host: Option<String>,
        /// Kernel-admin JWT. Falls back to VYN_JWT_TOKEN, else a 60s admin
        /// token is minted locally from config jwt_secret.
        #[arg(long)]
        token: Option<String>,
        /// Grant preset for the device minted from this ticket: guest, user or
        /// owner. Default: user.
        #[arg(long)]
        preset: Option<Preset>,
    },
    /// List paired device credentials (from the local store), merged with live
    /// state from the running kernel when reachable.
    List {
        /// Skip the live merge (works offline).
        #[arg(long)]
        offline: bool,
    },
    /// Revoke a device credential — its next connect attempt is rejected.
    Revoke {
        /// Device id to revoke.
        device_id: String,
        /// Undo a revocation (restore access).
        #[arg(long)]
        undo: bool,
    },
    /// Delete a device credential row entirely (the device must re-pair).
    Remove {
        /// Device id to remove.
        device_id: String,
    },
    /// Show or change what a paired device may do on this host. Takes effect
    /// on the next request, no re-pair needed. Grants: plugins.view,
    /// plugins.logs, plugins.control, plugins.install, devices.manage.
    Grants {
        /// Device id.
        device_id: String,
        /// Replace the grants with a preset: guest, user or owner.
        #[arg(long, conflicts_with_all = ["add", "remove"])]
        preset: Option<Preset>,
        /// Comma-separated grants to add.
        #[arg(long, value_delimiter = ',')]
        add: Vec<String>,
        /// Comma-separated grants to remove.
        #[arg(long, value_delimiter = ',')]
        remove: Vec<String>,
    },
}

pub async fn handle(cmd: DeviceCmd, config_path: &str) -> anyhow::Result<()> {
    match cmd {
        DeviceCmd::Connect {
            device,
            name,
            host,
            permissions,
            ipc_targets,
            ttl_seconds,
            aud,
            preset,
        } => {
            connect(
                ConnectOpts {
                    device,
                    name,
                    host,
                    permissions,
                    ipc_targets,
                    ttl_seconds,
                    aud,
                    preset,
                },
                config_path,
            )?;
            Ok(())
        }
        DeviceCmd::Pair {
            ttl,
            name,
            host,
            token,
            preset,
        } => {
            let token = token.or_else(|| std::env::var("VYN_JWT_TOKEN").ok());
            pair(ttl, name, host, token, preset, config_path).await?;
            Ok(())
        }
        DeviceCmd::List { offline } => list(offline, config_path).await,
        DeviceCmd::Revoke { device_id, undo } => revoke(&device_id, undo, config_path),
        DeviceCmd::Remove { device_id } => remove(&device_id, config_path),
        DeviceCmd::Grants {
            device_id,
            preset,
            add,
            remove,
        } => {
            println!(
                "{}",
                grants(&device_id, preset, &add, &remove, config_path)?
            );
            Ok(())
        }
    }
}

fn open_store(cfg: &Config) -> anyhow::Result<DeviceStore> {
    let secret = cfg.jwt_secret.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret configured in config — set jwt_secret to manage device credentials"
        )
    })?;
    Ok(DeviceStore::new(&cfg.data_dir, secret))
}

struct ConnectOpts {
    device: Option<String>,
    name: Option<String>,
    host: Option<String>,
    permissions: Option<String>,
    ipc_targets: Option<String>,
    ttl_seconds: u64,
    aud: Option<String>,
    preset: Option<Preset>,
}

fn connect(opts: ConnectOpts, config_path: &str) -> anyhow::Result<String> {
    let ConnectOpts {
        device,
        name,
        host,
        permissions,
        ipc_targets,
        ttl_seconds,
        aud,
        preset,
    } = opts;
    let cfg = load_config(config_path)?;
    let secret = cfg.jwt_secret.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret configured in '{config_path}' — set jwt_secret to pair devices"
        )
    })?;

    let device_id = device.unwrap_or_else(random_device_id);
    let name = name.unwrap_or_else(|| device_id.clone());
    let host_url = resolve_advertise_url(cfg.port, cfg.tls, host.as_deref())?;
    warn_if_loopback(&host_url);

    // E-01: mint the per-device credential FIRST — a failure here must not
    // leave a half-paired device behind (token exists, row missing).
    let store = DeviceStore::new(&cfg.data_dir, &secret);
    let device_secret = store.issue(
        &device_id,
        &name,
        ttl_seconds,
        preset.map(|p| p.grants()).as_ref(),
    )?;

    let perms = permissions
        .map(parse_csv)
        .unwrap_or_else(default_device_permissions);
    let targets = ipc_targets.map(parse_csv).unwrap_or_default();
    let audience = aud
        .or(cfg.jwt_audience.clone())
        .unwrap_or_else(|| "vynkor".to_string());
    let jwt_token = crate::auth::jwt::mint_device_token(
        secret.as_bytes(),
        &device_id,
        perms,
        targets,
        ttl_seconds,
        &audience,
    )
    .map_err(anyhow::Error::msg)?;

    // Pin the served cert when TLS is on, so the phone can use wss:// against a
    // self-signed cert (rcgen ECDSA ~800B PEM — fits the QR). Absent cert →
    // no pin; the app falls back to webpki-roots (publicly-trusted certs).
    let cert_pem = if cfg.tls {
        match effective_tls_cert_path(&cfg) {
            Some(p) if p.exists() => Some(std::fs::read_to_string(&p)?),
            _ => None,
        }
    } else {
        None
    };

    let payload = PairPayload {
        v: 2,
        name,
        host_url,
        device_id: device_id.clone(),
        jwt_token,
        device_secret,
        cert_pem,
    };
    let link = encode_pair_link(&payload)?;

    println!("Pairing link (open on the phone, or render as a QR with `vyn-pair`):\n");
    println!("{link}\n");
    println!("paired device '{device_id}' — link {} chars", link.len());
    if let Some(fp) = fingerprint_of(payload.cert_pem.as_deref()) {
        println!("cert sha256: {fp}");
    }
    println!("render a scannable QR code: vyn device connect ... | vyn-pair");
    println!("credential expires in {ttl_seconds}s; revoke anytime: vyn device revoke {device_id}");

    Ok(link)
}

/// CD-01: thin client over `POST /devices/pair` — the running kernel owns the
/// ticket store, so this never touches tickets.json itself.
async fn pair(
    ttl_secs: u64,
    name: Option<String>,
    host: Option<String>,
    token: Option<String>,
    preset: Option<Preset>,
    config_path: &str,
) -> anyhow::Result<String> {
    let cfg = load_config(config_path)?;
    let token = match token {
        Some(t) => t,
        None => local_admin_token(&cfg, config_path)?,
    };
    let scheme = if cfg.tls { "https" } else { "http" };
    let base = format!("{scheme}://127.0.0.1:{}", cfg.port);
    let cert = effective_tls_cert_path(&cfg);
    let client = super::plugin::build_client(cfg.tls, cert.as_deref())?;
    let body = serde_json::json!({
        "ttl_secs": ttl_secs,
        "name": name,
        "host": host,
        "preset": preset.map(|p| p.as_str()),
    });
    let resp =
        super::plugin::api_post_json(&client, &base, "/devices/pair", Some(&token), &body).await?;
    let view: TicketView = serde_json::from_str(&resp)?;
    warn_if_loopback(&view.ws);
    let cert_sha256 = view.cert_sha256.clone();

    let link = encode_pair_link(&TicketLinkPayload {
        v: view.v,
        ws: view.ws.clone(),
        ticket: view.ticket,
        cert_pem: view.cert_pem,
    })?;
    println!("Pairing link (single use, open on the phone or render with `vyn-pair`):\n");
    println!("{link}\n");
    println!(
        "ticket for {} expires in {}s (at {} UTC) — link {} chars",
        view.ws,
        view.ttl_secs,
        format_ts(view.expires_at),
        link.len()
    );
    if let Some(fp) = cert_sha256 {
        println!("cert sha256: {fp}");
    }
    println!("render a scannable QR code: vyn device pair ... | vyn-pair");
    println!("once scanned, the device shows up in: vyn device list");
    Ok(link)
}

/// Short-lived kernel-admin token signed with the local jwt_secret — the
/// operator running `vyn` on the host can already read that secret.
fn local_admin_token(cfg: &Config, config_path: &str) -> anyhow::Result<String> {
    let secret = cfg.jwt_secret.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret in '{config_path}' and no --token/VYN_JWT_TOKEN — pairing needs auth"
        )
    })?;
    let audience = cfg
        .jwt_audience
        .clone()
        .unwrap_or_else(|| "vynkor".to_string());
    Ok(crate::auth::jwt::mint_device_token(
        secret.as_bytes(),
        "vyn-cli",
        vec![crate::proto::vynkor::PermissionType::PermissionKernelAdmin
            .as_str_name()
            .to_string()],
        vec![],
        60,
        &audience,
    )?)
}

/// `300`, `300s`, `5m`, `1h` → seconds.
fn parse_duration_secs(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last() {
        Some((i, 's')) => (&s[..i], 1),
        Some((i, 'm')) => (&s[..i], 60),
        Some((i, 'h')) => (&s[..i], 3600),
        _ => (s, 1),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("bad duration '{s}' (try 300, 90s, 5m, 1h)"))?;
    match n.checked_mul(mult) {
        Some(0) | None => Err(format!("duration '{s}' out of range")),
        Some(v) => Ok(v),
    }
}

/// cd-08: shown next to the link so the operator can match it against what
/// the phone pins (`vyn tls status` prints the same value).
fn fingerprint_of(cert_pem: Option<&str>) -> Option<String> {
    cert_pem.and_then(|p| crate::utils::tls::cert_sha256_fingerprint(p).ok())
}

fn warn_if_loopback(url: &str) {
    if is_loopback_url(url) {
        eprintln!(
            "⚠️  '{url}' is loopback — the phone cannot reach your host there. \
             Use a LAN IP or Tailscale name (same Wi-Fi/LAN only works while both are on it)."
        );
    }
}

async fn list(offline: bool, config_path: &str) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(&cfg)?;
    let rows = store.list()?;
    if rows.is_empty() {
        println!("No paired devices. Pair one with `vyn device connect`.");
        return Ok(());
    }

    // live merge: last_seen + online/offline from GET /devices when reachable
    let live = if offline {
        None
    } else {
        fetch_live_devices(&cfg).await
    };

    const HEADERS: [&str; 7] = [
        "DEVICE_ID",
        "NAME",
        "CREATED",
        "EXPIRES",
        "LAST_SEEN",
        "STATE",
        "GRANTS",
    ];
    let mut widths: [usize; 7] = HEADERS.map(str::len);
    let mut lines: Vec<[String; 7]> = Vec::new();
    for row in rows {
        let now = now_secs();
        let state = match row.status(now) {
            DeviceStatus::Active => "active",
            DeviceStatus::Revoked => "REVOKED",
            DeviceStatus::Expired => "expired",
        };
        let last_seen = live
            .as_ref()
            .and_then(|m| m.get(&row.device_id))
            .map(|(last_seen_ms, _)| format_ts(last_seen_ms / 1000))
            .unwrap_or_else(|| "-".to_string());
        let cells = [
            row.device_id.clone(),
            row.name.clone(),
            format_ts(row.created_at),
            format_ts(row.expires_at),
            last_seen,
            state.to_string(),
            grants_cell(&record_grants(&row)),
        ];
        for (i, cell) in cells.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
        lines.push(cells);
    }

    let fmt_line = |cells: &[&str]| -> String {
        // last column is left unpadded
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                if i + 1 == cells.len() {
                    c.to_string()
                } else {
                    format!("{c:<w$}", w = widths[i])
                }
            })
            .collect::<Vec<_>>()
            .join("  ")
    };
    println!("{}", fmt_line(&HEADERS));
    for cells in &lines {
        let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
        println!("{}", fmt_line(&refs));
    }
    Ok(())
}

/// `guest` for an empty set, else the comma-separated grant names.
fn grants_cell(g: &Grants) -> String {
    if g.is_empty() {
        "guest".to_string()
    } else {
        g.iter().map(Grant::as_str).collect::<Vec<_>>().join(",")
    }
}

/// `vyn device grants` — returns the line to print. Validates every grant
/// name BEFORE touching the store.
fn grants(
    device_id: &str,
    preset: Option<Preset>,
    add: &[String],
    remove: &[String],
    config_path: &str,
) -> anyhow::Result<String> {
    let parse_all = |names: &[String]| -> anyhow::Result<Vec<Grant>> {
        names
            .iter()
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .map(|n| n.parse::<Grant>().map_err(anyhow::Error::msg))
            .collect()
    };
    let to_add = parse_all(add)?;
    let to_remove = parse_all(remove)?;

    let cfg = load_config(config_path)?;
    let store = open_store(&cfg)?;
    let Some(mut current) = store.grants(device_id)? else {
        anyhow::bail!("unknown device {device_id}");
    };
    if preset.is_some() || !to_add.is_empty() || !to_remove.is_empty() {
        if let Some(p) = preset {
            current = p.grants();
        }
        current.extend(to_add);
        for g in to_remove {
            current.remove(&g);
        }
        if !store.set_grants(device_id, &current)? {
            anyhow::bail!("unknown device {device_id}");
        }
    }
    Ok(if current.is_empty() {
        format!("{device_id}: (guest — agent only)")
    } else {
        format!(
            "{device_id}: {}",
            current
                .iter()
                .map(Grant::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// GET /devices from the running kernel (localhost); None = unreachable.
async fn fetch_live_devices(
    cfg: &Config,
) -> Option<std::collections::HashMap<String, (u64, bool)>> {
    let scheme = if cfg.tls { "https" } else { "http" };
    let base = format!("{scheme}://127.0.0.1:{}", cfg.port);
    let cert = effective_tls_cert_path(cfg);
    let client = super::plugin::build_client(cfg.tls, cert.as_deref()).ok()?;
    let body = super::plugin::api_get(&client, &base, "/devices", None)
        .await
        .ok()?;
    let value: Vec<serde_json::Value> = serde_json::from_str(&body).ok()?;
    Some(
        value
            .into_iter()
            .filter_map(|d| {
                let id = d.get("device_id")?.as_str()?.to_string();
                let last_seen = d.get("last_seen")?.as_u64().unwrap_or(0);
                let online = d.get("state")?.as_str()? == "online";
                Some((id, (last_seen, online)))
            })
            .collect(),
    )
}

fn revoke(device_id: &str, undo: bool, config_path: &str) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(&cfg)?;
    if undo {
        if store.set_revoked(device_id, false)? {
            println!("device '{device_id}' un-revoked");
        } else {
            println!("no such device '{device_id}'");
        }
        return Ok(());
    }
    if store.set_revoked(device_id, true)? {
        println!("device '{device_id}' revoked — future connections will be rejected");
    } else {
        anyhow::bail!("no paired device '{device_id}' (see: vyn device list)");
    }
    Ok(())
}

fn remove(device_id: &str, config_path: &str) -> anyhow::Result<()> {
    let cfg = load_config(config_path)?;
    let store = open_store(&cfg)?;
    if store.remove(device_id)? {
        println!("device '{device_id}' removed — it must pair again to connect");
    } else {
        println!("no paired device '{device_id}'");
    }
    Ok(())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// C3-style dependency-free UTC civil-from-days formatting (Howard Hinnant).
fn format_ts(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100 + yoe / 146_096);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 3600 % 60
    )
}

fn random_device_id() -> String {
    use rand::RngExt;
    let n: u32 = rand::rng().random();
    format!("dev-{:06x}", n & 0xFF_FFFF)
}

fn parse_csv(s: String) -> Vec<String> {
    s.split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;

    fn write_cfg(dir: &std::path::Path, extras: &str) -> String {
        let path = dir.join("config.yaml");
        std::fs::write(
            &path,
            format!(
                "port: 8080\ntls: false\ndata_dir: {}\n{extras}",
                dir.display()
            ),
        )
        .unwrap();
        path.to_string_lossy().to_string()
    }

    #[test]
    fn random_device_id_has_prefix() {
        assert!(random_device_id().starts_with("dev-"));
    }

    #[test]
    fn duration_parses_units() {
        assert_eq!(parse_duration_secs("300"), Ok(300));
        assert_eq!(parse_duration_secs("90s"), Ok(90));
        assert_eq!(parse_duration_secs("5m"), Ok(300));
        assert_eq!(parse_duration_secs("1h"), Ok(3600));
        assert!(parse_duration_secs("0").is_err());
        assert!(parse_duration_secs("5d").is_err());
        assert!(parse_duration_secs("").is_err());
    }

    fn decode_link(link: &str) -> serde_json::Value {
        let encoded = link.strip_prefix("vynkor://pair?z=1&d=").unwrap();
        let compressed = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        use std::io::Read;
        let mut json = String::new();
        decoder.read_to_string(&mut json).unwrap();
        serde_json::from_str(&json).unwrap()
    }

    #[tokio::test]
    async fn pair_posts_to_kernel_and_prints_ticket_link() {
        let mut server = mockito::Server::new_async().await;
        let ticket = "t".repeat(43);
        let mock = server
            .mock("POST", "/devices/pair")
            .match_header(
                "authorization",
                mockito::Matcher::Regex("^Bearer .+".into()),
            )
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"ttl_secs": 120, "name": "friend"}),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!({
                    "v": 2, "ticket": ticket, "ws": "ws://10.0.0.5:8080/ws",
                    "ttl_secs": 120, "expires_at": 1_700_000_000u64
                })
                .to_string(),
            )
            .create_async()
            .await;

        let dir = tempfile::tempdir().unwrap();
        let port = server.socket_address().port();
        let cfg_path = dir.path().join("config.yaml");
        std::fs::write(
            &cfg_path,
            format!(
                "port: {port}\ntls: false\ndata_dir: {}\njwt_secret: {}\n",
                dir.path().display(),
                "s".repeat(40)
            ),
        )
        .unwrap();

        let link = pair(
            120,
            Some("friend".into()),
            None,
            None,
            None,
            cfg_path.to_str().unwrap(),
        )
        .await
        .unwrap();
        mock.assert_async().await;

        let v = decode_link(&link);
        assert_eq!(v["v"], 2);
        assert_eq!(v["ws"], "ws://10.0.0.5:8080/ws");
        assert_eq!(v["ticket"], ticket);
        assert!(v.get("jwt_token").is_none() && v.get("device_secret").is_none());
    }

    #[test]
    fn pair_payload_v2_has_no_master_secret_field() {
        let payload = PairPayload {
            v: 2,
            name: "n".into(),
            host_url: "ws://h:1/ws".into(),
            device_id: "dev-1".into(),
            jwt_token: "tok".into(),
            device_secret: "s".repeat(64),
            cert_pem: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("device_secret"));
        assert!(!json.contains("jwt_secret"), "master secret must not ship");
        assert_eq!(json.matches("\"device_secret\"").count(), 1);
    }

    #[test]
    fn connect_end_to_end_issues_row_and_v2_link() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write_cfg(dir.path(), &format!("jwt_secret: {}", "s".repeat(40)));

        let link = connect(
            ConnectOpts {
                device: Some("dev-e2e".into()),
                name: Some("lab".into()),
                host: Some("10.0.0.5".into()),
                permissions: None,
                ipc_targets: None,
                ttl_seconds: 3600,
                aud: None,
                preset: None,
            },
            &cfg_path,
        )
        .unwrap();

        assert!(link.starts_with("vynkor://pair?z=1&d="), "{link}");

        // decode + inflate + parse the payload like the agent would
        let encoded = link.strip_prefix("vynkor://pair?z=1&d=").unwrap();
        let compressed = URL_SAFE_NO_PAD.decode(encoded).unwrap();
        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        use std::io::Read;
        let mut json = String::new();
        decoder.read_to_string(&mut json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["v"], 2);
        assert_eq!(value["device_id"], "dev-e2e");
        assert!(value["device_secret"].as_str().unwrap().len() == 64);
        assert!(value.get("jwt_secret").is_none());

        // row exists and decrypts
        let cfg = load_config(&cfg_path).unwrap();
        let store = DeviceStore::new(&cfg.data_dir, cfg.jwt_secret.as_ref().unwrap());
        let (row, secret) = store.get("dev-e2e").unwrap().unwrap();
        assert_eq!(secret, value["device_secret"]);
        assert_eq!(row.status(row.created_at), DeviceStatus::Active);
    }

    #[test]
    fn revoke_and_remove_flow_over_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write_cfg(dir.path(), &format!("jwt_secret: {}", "s".repeat(40)));
        let cfg = load_config(&cfg_path).unwrap();

        connect(
            ConnectOpts {
                device: Some("dev-r".into()),
                name: None,
                host: Some("10.0.0.5".into()),
                permissions: None,
                ipc_targets: None,
                ttl_seconds: 3600,
                aud: None,
                preset: None,
            },
            &cfg_path,
        )
        .unwrap();

        revoke("dev-r", false, &cfg_path).unwrap();
        let store = DeviceStore::new(&cfg.data_dir, cfg.jwt_secret.as_ref().unwrap());
        assert!(store.active_secret("dev-r").is_err());

        revoke("dev-r", true, &cfg_path).unwrap();
        assert!(store.active_secret("dev-r").unwrap().is_some());

        remove("dev-r", &cfg_path).unwrap();
        assert!(store.get("dev-r").unwrap().is_none());
    }

    fn connect_dev(cfg_path: &str, id: &str, preset: Option<Preset>) {
        connect(
            ConnectOpts {
                device: Some(id.into()),
                name: None,
                host: Some("10.0.0.5".into()),
                permissions: None,
                ipc_targets: None,
                ttl_seconds: 3600,
                aud: None,
                preset,
            },
            cfg_path,
        )
        .unwrap();
    }

    fn setup() -> (tempfile::TempDir, String, DeviceStore) {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write_cfg(dir.path(), &format!("jwt_secret: {}", "s".repeat(40)));
        let cfg = load_config(&cfg_path).unwrap();
        let store = DeviceStore::new(&cfg.data_dir, cfg.jwt_secret.as_ref().unwrap());
        (dir, cfg_path, store)
    }

    #[test]
    fn connect_default_preset_is_user() {
        let (_d, cfg, store) = setup();
        connect_dev(&cfg, "dev-u", None);
        assert_eq!(
            store.grants("dev-u").unwrap().unwrap(),
            Preset::User.grants()
        );
    }

    #[test]
    fn connect_guest_has_no_grants() {
        let (_d, cfg, store) = setup();
        connect_dev(&cfg, "dev-g", Some(Preset::Guest));
        assert!(store.grants("dev-g").unwrap().unwrap().is_empty());
    }

    #[test]
    fn repair_without_preset_keeps_grants() {
        let (_d, cfg, store) = setup();
        connect_dev(&cfg, "dev-o", Some(Preset::Owner));
        connect_dev(&cfg, "dev-o", None);
        assert_eq!(
            store.grants("dev-o").unwrap().unwrap(),
            Preset::Owner.grants()
        );
        connect_dev(&cfg, "dev-o", Some(Preset::Guest));
        assert!(store.grants("dev-o").unwrap().unwrap().is_empty());
    }

    #[test]
    fn preset_flag_parses_and_rejects_unknown() {
        use clap::Parser;
        #[derive(Parser)]
        struct W {
            #[command(subcommand)]
            c: DeviceCmd,
        }
        let ok = W::try_parse_from(["w", "connect", "--preset", "owner"]).unwrap();
        assert!(matches!(
            ok.c,
            DeviceCmd::Connect {
                preset: Some(Preset::Owner),
                ..
            }
        ));
        assert!(W::try_parse_from(["w", "connect", "--preset", "admin"]).is_err());
        assert!(W::try_parse_from(["w", "pair", "--preset", "admin"]).is_err());
        assert!(W::try_parse_from(["w", "grants", "d", "--preset", "admin"]).is_err());
        // --preset excludes --add/--remove
        assert!(W::try_parse_from(["w", "grants", "d", "--preset", "user", "--add", "x"]).is_err());
    }

    #[test]
    fn grants_show_set_preset_add_remove() {
        let (_d, cfg, store) = setup();
        connect_dev(&cfg, "dev-x", None);
        assert_eq!(
            grants("dev-x", None, &[], &[], &cfg).unwrap(),
            "dev-x: plugins.view, plugins.logs, plugins.control"
        );
        assert_eq!(
            grants("dev-x", Some(Preset::Guest), &[], &[], &cfg).unwrap(),
            "dev-x: (guest — agent only)"
        );
        grants("dev-x", Some(Preset::Owner), &[], &[], &cfg).unwrap();
        assert_eq!(store.grants("dev-x").unwrap().unwrap().len(), 5);
        let out = grants(
            "dev-x",
            None,
            &["plugins.install".into()],
            &["plugins.control".into(), "devices.manage".into()],
            &cfg,
        )
        .unwrap();
        assert_eq!(out, "dev-x: plugins.view, plugins.logs, plugins.install");
        // persisted
        assert_eq!(store.grants("dev-x").unwrap().unwrap().len(), 3);
    }

    #[test]
    fn grants_unknown_name_fails_before_write() {
        let (_d, cfg, store) = setup();
        connect_dev(&cfg, "dev-x", None);
        let err = grants(
            "dev-x",
            None,
            &["plugins.install".into(), "plugins.foo".into()],
            &[],
            &cfg,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("plugins.foo") && err.contains("devices.manage"),
            "{err}"
        );
        // nothing was written, not even the valid half
        assert_eq!(
            store.grants("dev-x").unwrap().unwrap(),
            Preset::User.grants()
        );
    }

    #[test]
    fn grants_unknown_device_errors() {
        let (_d, cfg, _store) = setup();
        let err = grants("ghost", Some(Preset::Owner), &[], &[], &cfg)
            .unwrap_err()
            .to_string();
        assert_eq!(err, "unknown device ghost");
    }

    #[test]
    fn list_shows_grants_column() {
        assert_eq!(grants_cell(&Grants::new()), "guest");
        assert_eq!(
            grants_cell(&Preset::User.grants()),
            "plugins.view,plugins.logs,plugins.control"
        );
    }

    #[test]
    fn format_ts_round_shape() {
        let s = format_ts(1_700_000_000);
        assert_eq!(s.len(), 19);
        assert!(s.starts_with("2023-"));
    }
}
