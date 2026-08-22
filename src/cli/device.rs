//! `vyn device connect` — pair a remote device agent (the vynkor Android app)
//! by rendering a QR code and a `vynkor://pair` link that carry everything the
//! agent needs to join the host: host URL, device id, per-device JWT, the
//! frame-MAC secret, and (when TLS is on) the served cert for pinning.
//!
//! The QR is a physical, unidirectional trusted channel — it can carry the
//! cert that a phone would otherwise refuse (self-signed), which is exactly
//! the D-07 "pin the exact served cert" rule for local clients, extended to
//! the phone.

use std::net::{IpAddr, UdpSocket};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use clap::Subcommand;
use qrcode::render::unicode::Dense1x2;
use qrcode::QrCode;
use serde::Serialize;

use crate::utils::config::{effective_tls_cert_path, load_config, Config};

/// The JSON document encoded (base64url) as `vynkor://pair?d=...`. The app
/// decodes it into a host profile and connects. `cert_pem` is present only
/// when the kernel serves TLS (D-07 default), so the agent can pin it and use
/// `wss://` against a self-signed cert instead of falling back to `tls: false`.
#[derive(Serialize)]
struct PairPayload {
    v: u32,
    name: String,
    host_url: String,
    device_id: String,
    jwt_token: String,
    jwt_secret: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cert_pem: Option<String>,
}

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// Pair a device agent: print a QR code + `vynkor://pair` link the vynkor
    /// Android app scans to configure itself and connect. Requires `jwt_secret`.
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
        /// Token lifetime in seconds. Default: 86400 (24h).
        #[arg(long, default_value_t = 86400)]
        ttl_seconds: u64,
        /// Audience claim. Default: config `jwt_audience`, else "vynkor".
        #[arg(long)]
        aud: Option<String>,
        /// Also write the QR to this path (SVG, opens in a browser).
        #[arg(long)]
        qr_out: Option<String>,
    },
}

pub fn handle(cmd: DeviceCmd, config_path: &str) -> anyhow::Result<()> {
    let DeviceCmd::Connect {
        device,
        name,
        host,
        permissions,
        ipc_targets,
        ttl_seconds,
        aud,
        qr_out,
    } = cmd;

    let cfg = load_config(config_path).unwrap_or_default();
    let secret = cfg.jwt_secret.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "no jwt_secret configured in '{config_path}' — set jwt_secret to pair devices"
        )
    })?;

    let device_id = device.unwrap_or_else(random_device_id);
    let name = name.unwrap_or_else(|| device_id.clone());
    let host_url = resolve_advertise_url(&cfg, host.as_deref())?;

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
        v: 1,
        name,
        host_url,
        device_id,
        jwt_token,
        jwt_secret: secret,
        cert_pem,
    };
    let json = serde_json::to_string(&payload)?;
    let link = format!(
        "vynkor://pair?d={}",
        URL_SAFE_NO_PAD.encode(json.as_bytes())
    );

    eprintln!(
        "\n⚠️  This QR carries the host's MASTER jwt_secret — anyone who scans it can mint \
tokens for every device on this host. Show it only to the device being paired.\n"
    );
    println!("Scan with the vynkor Android app (or open the link):\n");
    print_qr(&link)?;
    println!("\n{link}\n");

    if let Some(path) = qr_out {
        write_svg(&link, &path)?;
        eprintln!("QR written to {path}");
    }
    Ok(())
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
        url.set_path("/ws");
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
}
