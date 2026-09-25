//! URL resolution shared by the bridge dialer and the device pairing QR
//! (MA-02): one home for the ws-scheme mapping and the default gateway
//! path, so the wire contract ("gateway lives at /ws") is written once.
//! CD-01: the pairing advertise url lives here too — both `vyn device
//! connect` and `POST /devices/pair` need it.

use std::net::{IpAddr, UdpSocket};

use crate::utils::errors::VynkorError;

/// Path the WS gateway serves on when a URL carries no explicit one.
pub const DEFAULT_WS_PATH: &str = "/ws";

/// ws/wss counterpart of an http/https scheme (`None` for anything else).
pub fn ws_scheme_for(scheme: &str) -> Option<&'static str> {
    match scheme {
        "http" => Some("ws"),
        "https" => Some("wss"),
        _ => None,
    }
}

/// Resolve the advertise URL a phone should dial. Never auto-detects
/// loopback — the QR is scanned by a phone whose `localhost` is itself. A
/// bare host (no port) gains `port`; a full URL keeps its host/port/path but
/// is canonicalized to `ws`/`wss`. An explicit loopback host is allowed
/// (tests, adb reverse) — callers warn via `is_loopback_url`.
pub fn resolve_advertise_url(
    port: u16,
    tls: bool,
    host_override: Option<&str>,
) -> Result<String, VynkorError> {
    let scheme = if tls { "wss" } else { "ws" };
    let host = match host_override.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => h.to_string(),
        None => {
            let ip = detect_lan_ip().ok_or_else(|| {
                VynkorError::NetworkError(
                    "could not auto-detect a LAN address — pass a host (e.g. \
                     100.64.0.2 or myhost.tailnet)"
                        .into(),
                )
            })?;
            if ip.is_loopback() {
                return Err(VynkorError::NetworkError(format!(
                    "detected loopback {ip} — a phone scanning this QR would reach itself; \
                     pass a LAN IP or Tailscale name"
                )));
            }
            format!("{ip}:{port}")
        }
    };

    let gave_bare_host = !host.contains("://");
    let with_scheme = if gave_bare_host {
        format!("{scheme}://{host}")
    } else {
        host.clone()
    };
    let mut url = url::Url::parse(&with_scheme)
        .map_err(|e| VynkorError::InvalidInput(format!("bad host '{host}': {e}")))?;

    if url.port().is_none() && gave_bare_host {
        url.set_port(Some(port)).ok();
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path(DEFAULT_WS_PATH);
    }
    // an explicit scheme wins over the kernel's own tls flag: behind a
    // tls-terminating proxy the kernel runs `tls: false` yet phones must
    // still dial wss:// (docs/TLS.md)
    let target = if gave_bare_host {
        scheme
    } else {
        match url.scheme() {
            "ws" | "wss" => return Ok(url.to_string()),
            other => ws_scheme_for(other).ok_or_else(|| {
                VynkorError::InvalidInput(format!(
                    "bad host '{host}': scheme must be ws, wss, http or https"
                ))
            })?,
        }
    };
    url.set_scheme(target)
        .map_err(|()| VynkorError::InvalidInput(format!("bad host '{host}'")))?;
    Ok(url.to_string())
}

/// True when the URL's host is loopback — unreachable from a phone.
pub fn is_loopback_url(advertise_url: &str) -> bool {
    url::Url::parse(advertise_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .is_some_and(|h| h == "localhost" || h == "127.0.0.1" || h == "[::1]" || h == "::1")
}

/// Local egress IP via the UDP-connect trick (no packets actually sent).
fn detect_lan_ip() -> Option<IpAddr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    sock.local_addr().ok().map(|a| a.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_resolves_and_is_flagged() {
        let url = resolve_advertise_url(8080, false, Some("localhost:8080")).unwrap();
        assert_eq!(url, "ws://localhost:8080/ws");
        assert!(is_loopback_url(&url));
        assert!(!is_loopback_url("ws://10.0.0.5:8080/ws"));
    }

    #[test]
    fn bare_host_gets_config_port() {
        let url = resolve_advertise_url(25565, false, Some("myhost.tailnet")).unwrap();
        assert_eq!(url, "ws://myhost.tailnet:25565/ws");
    }

    #[test]
    fn host_with_explicit_port_keeps_it() {
        let url = resolve_advertise_url(25565, true, Some("100.64.0.2:8443")).unwrap();
        assert_eq!(url, "wss://100.64.0.2:8443/ws");
    }

    #[test]
    fn full_url_keeps_path_and_drops_default_port() {
        let url = resolve_advertise_url(9999, true, Some("https://myhost.tailnet:443/ws")).unwrap();
        assert_eq!(url, "wss://myhost.tailnet/ws");
    }

    #[test]
    fn explicit_scheme_survives_plaintext_kernel_behind_proxy() {
        let url = resolve_advertise_url(8080, false, Some("wss://vyn.example.com/ws")).unwrap();
        assert_eq!(url, "wss://vyn.example.com/ws");
        let url = resolve_advertise_url(8080, false, Some("https://vyn.example.com")).unwrap();
        assert_eq!(url, "wss://vyn.example.com/ws");
        // and the reverse: an explicit ws:// is not silently upgraded
        let url = resolve_advertise_url(8080, true, Some("ws://10.0.0.5:8080/ws")).unwrap();
        assert_eq!(url, "ws://10.0.0.5:8080/ws");
        assert!(resolve_advertise_url(8080, true, Some("ftp://h")).is_err());
    }

    #[test]
    fn ws_scheme_maps_only_http_families() {
        assert_eq!(ws_scheme_for("http"), Some("ws"));
        assert_eq!(ws_scheme_for("https"), Some("wss"));
        assert_eq!(ws_scheme_for("ftp"), None);
        assert_eq!(ws_scheme_for(""), None);
    }
}
