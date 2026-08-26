//! `vyn device connect` — pair a remote device agent (the vynkor Android app)
//! by rendering a QR code and a `vynkor://pair` link that carry everything the
//! agent needs to join the host: host URL, device id, per-device JWT, the
//! device's OWN frame-MAC secret, and (when TLS is on) the served cert for
//! pinning.
//!
//! E-01: the host issues a unique per-device secret at pair time and stores it
//! encrypted in `<data_dir>/devices.json`; the master jwt_secret never leaves
//! the host. The QR is a physical, unidirectional trusted channel — good
//! enough to carry the device secret once and the self-signed cert.
//!
//! Lifecycle companions: `vyn device list`, `vyn device revoke`, `vyn device
//! remove`. Revocation takes effect on a running kernel immediately — the
//! router re-reads the store on every registration and the WS gateway on every
//! upgrade.

use std::io::Write;
use std::net::{IpAddr, UdpSocket};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use clap::Subcommand;
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;
use serde::Serialize;

use crate::auth::device_store::{DeviceStatus, DeviceStore};
use crate::utils::config::{effective_tls_cert_path, load_config, Config};

/// The JSON document encoded (deflate-compressed, base64url) as
/// `vynkor://pair?d=...&z=1`. The app decodes it into a host profile and
/// connects. `cert_pem` is present only when the kernel serves TLS (D-07
/// default), so the agent can pin it and use `wss://` against a self-signed
/// cert instead of falling back to `tls: false`.
#[derive(Serialize)]
struct PairPayload {
    v: u32,
    name: String,
    host_url: String,
    device_id: String,
    jwt_token: String,
    /// per-device secret issued by THIS host — never the master jwt_secret
    device_secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_pem: Option<String>,
}

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Issue a per-device credential and print a QR code + `vynkor://pair`
    /// link the vynkor Android app scans to configure itself and connect.
    /// Requires `jwt_secret`.
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
        /// Also write the QR to this path (SVG, opens in a browser).
        #[arg(long)]
        qr_out: Option<String>,
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
            qr_out,
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
                    qr_out,
                },
                config_path,
            )?;
            Ok(())
        }
        DeviceCmd::List { offline } => list(offline, config_path).await,
        DeviceCmd::Revoke { device_id, undo } => revoke(&device_id, undo, config_path),
        DeviceCmd::Remove { device_id } => remove(&device_id, config_path),
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
    qr_out: Option<String>,
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
        qr_out,
    } = opts;
    let cfg = load_config(config_path)?;
    let secret = cfg.jwt_secret.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret configured in '{config_path}' — set jwt_secret to pair devices"
        )
    })?;

    let device_id = device.unwrap_or_else(random_device_id);
    let name = name.unwrap_or_else(|| device_id.clone());
    let host_url = resolve_advertise_url(&cfg, host.as_deref())?;

    // E-01: mint the per-device credential FIRST — a failure here must not
    // leave a half-paired device behind (token exists, row missing).
    let store = DeviceStore::new(&cfg.data_dir, &secret);
    let device_secret = store.issue(&device_id, &name, ttl_seconds)?;

    let perms = permissions.map(parse_csv).unwrap_or_else(|| {
        vec![
            "PERMISSION_IPC_SEND".to_string(),
            "PERMISSION_EVENT_PUBLISH".to_string(),
        ]
    });
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
    let json = serde_json::to_string(&payload)?;
    // deflate + base64url: the cert dominates the payload and the in-app
    // scanner chokes past QR version ~33; `z=1` tells the agent to inflate.
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(json.as_bytes())?;
    let compressed = encoder.finish()?;
    let link = format!("vynkor://pair?z=1&d={}", URL_SAFE_NO_PAD.encode(compressed));

    println!("Scan with the vynkor Android app (or open the link):\n");
    print_qr(&link)?;
    println!("\n{link}\n");
    println!(
        "paired device '{device_id}' — link {} chars (QR v{})",
        link.len(),
        qr_version(&link)
    );
    println!("credential expires in {ttl_seconds}s; revoke anytime: vyn device revoke {device_id}");

    if let Some(path) = qr_out {
        write_svg(&link, &path)?;
        eprintln!("QR written to {path}");
    }
    Ok(link)
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

    const HEADERS: [&str; 6] = [
        "DEVICE_ID",
        "NAME",
        "CREATED",
        "EXPIRES",
        "LAST_SEEN",
        "STATE",
    ];
    let mut widths: [usize; 6] = HEADERS.map(str::len);
    let mut table = String::new();
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
        ];
        for (i, cell) in cells.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
        table.push_str(&format!("{}\n", cells.join("\x1f")));
    }

    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
        HEADERS[0],
        HEADERS[1],
        HEADERS[2],
        HEADERS[3],
        HEADERS[4],
        HEADERS[5],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
    );
    for line in table.lines() {
        let cells: Vec<&str> = line.split('\x1f').collect();
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            cells[4],
            cells[5],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
        );
    }
    Ok(())
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

/// Approximate QR version for the printed size hint (byte capacity of
/// versions 1..=40 at ECC level L, numeric/alphanumeric ignored — we're
/// byte-mode).
fn qr_version(link: &str) -> usize {
    // byte capacities, ECC L, versions 1..40 (ISO/IEC 18004 tables)
    const CAPS: [usize; 40] = [
        17, 32, 53, 78, 106, 134, 154, 192, 230, 271, 321, 367, 425, 458, 520, 586, 644, 718, 792,
        858, 929, 1003, 1091, 1171, 1273, 1367, 1465, 1528, 1628, 1732, 1840, 1952, 2068, 2188,
        2303, 2431, 2563, 2699, 2809, 2953,
    ];
    let n = link.len();
    CAPS.iter()
        .position(|&cap| cap >= n)
        .map(|i| i + 1)
        .unwrap_or(41)
}

/// Resolve the advertise URL the phone should dial. Never loopback — the QR is
/// scanned by a phone whose `localhost` is itself. A bare `--host` (no port)
/// gains the config port; a full URL keeps its host/port/path but is
/// canonicalized to `ws`/`wss`.
fn resolve_advertise_url(cfg: &Config, host_override: Option<&str>) -> anyhow::Result<String> {
    let scheme = if cfg.tls { "wss" } else { "ws" };
    let host = match host_override {
        Some(h) => h.trim().to_string(),
        None => {
            let ip = detect_lan_ip().ok_or_else(|| {
                anyhow::anyhow!(
                    "could not auto-detect a LAN address — pass --host (e.g. \
                     --host 100.64.0.2 or --host myhost.tailnet)"
                )
            })?;
            if ip.is_loopback() {
                anyhow::bail!(
                    "detected loopback {ip} — a phone scanning this QR would reach itself. \
                     Pass --host with a LAN IP or Tailscale name."
                );
            }
            format!("{ip}:{}", cfg.port)
        }
    };

    let gave_bare_host = !host.contains("://");
    let with_scheme = if gave_bare_host {
        format!("{scheme}://{host}")
    } else {
        host.clone()
    };
    let mut url =
        url::Url::parse(&with_scheme).map_err(|e| anyhow::anyhow!("bad --host '{host}': {e}"))?;

    if url.port().is_none() && gave_bare_host {
        url.set_port(Some(cfg.port)).ok();
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path(crate::utils::url::DEFAULT_WS_PATH);
    }
    url.set_scheme(scheme)
        .map_err(|()| anyhow::anyhow!("bad --host '{host}'"))?;

    let host_only = url.host_str().unwrap_or_default();
    if host_only == "localhost" || host_only == "127.0.0.1" || host_only == "::1" {
        eprintln!(
            "⚠️  host '{host_only}' is loopback — the phone cannot reach your host there. \
             Use a LAN IP or Tailscale name (same Wi-Fi/LAN only works while both are on it)."
        );
    }

    Ok(url.to_string())
}

/// Local egress IP via the UDP-connect trick (no packets actually sent).
fn detect_lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

fn random_device_id() -> String {
    use rand::Rng;
    let n: u32 = rand::thread_rng().gen();
    format!("dev-{:06x}", n & 0xFF_FFFF)
}

fn print_qr(link: &str) -> anyhow::Result<()> {
    let code = QrCode::new(link.as_bytes()).map_err(anyhow::Error::new)?;
    let image = code.render::<Dense1x2>().quiet_zone(true).build();
    println!("{image}");
    Ok(())
}

fn write_svg(link: &str, path: &str) -> anyhow::Result<()> {
    use qrcode::render::svg;
    let code = QrCode::new(link.as_bytes()).map_err(anyhow::Error::new)?;
    let svg = code
        .render::<svg::Color>()
        .quiet_zone(true)
        .min_dimensions(512, 512)
        .build();
    std::fs::write(path, svg)?;
    Ok(())
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
    fn loopback_host_warns_but_resolves() {
        let cfg = Config {
            port: 8080,
            tls: false,
            ..Config::default()
        };
        let url = resolve_advertise_url(&cfg, Some("localhost:8080")).unwrap();
        assert_eq!(url, "ws://localhost:8080/ws");
    }

    #[test]
    fn bare_host_gets_config_port() {
        let cfg = Config {
            port: 25565,
            tls: false,
            ..Config::default()
        };
        let url = resolve_advertise_url(&cfg, Some("myhost.tailnet")).unwrap();
        assert_eq!(url, "ws://myhost.tailnet:25565/ws");
    }

    #[test]
    fn host_with_explicit_port_keeps_it() {
        let cfg = Config {
            port: 25565,
            tls: true,
            ..Config::default()
        };
        let url = resolve_advertise_url(&cfg, Some("100.64.0.2:8443")).unwrap();
        assert_eq!(url, "wss://100.64.0.2:8443/ws");
    }

    #[test]
    fn full_url_keeps_path_and_drops_default_port() {
        let cfg = Config {
            port: 9999,
            tls: true,
            ..Config::default()
        };
        let url = resolve_advertise_url(&cfg, Some("https://myhost.tailnet:443/ws")).unwrap();
        assert_eq!(url, "wss://myhost.tailnet/ws");
    }

    #[test]
    fn random_device_id_has_prefix() {
        assert!(random_device_id().starts_with("dev-"));
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
                qr_out: None,
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
                qr_out: None,
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

    #[test]
    fn qr_version_estimates_byte_capacity() {
        assert_eq!(qr_version(&"a".repeat(10)), 1);
        assert_eq!(qr_version(&"a".repeat(100)), 5);
        assert_eq!(qr_version(&"a".repeat(3000)), 41);
    }

    #[test]
    fn format_ts_round_shape() {
        let s = format_ts(1_700_000_000);
        assert_eq!(s.len(), 19);
        assert!(s.starts_with("2023-"));
    }
}
