//! URL resolution shared by the bridge dialer and the device pairing QR
//! (MA-02): one home for the ws-scheme mapping and the default gateway
//! path, so the wire contract ("gateway lives at /ws") is written once.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_scheme_maps_only_http_families() {
        assert_eq!(ws_scheme_for("http"), Some("ws"));
        assert_eq!(ws_scheme_for("https"), Some("wss"));
        assert_eq!(ws_scheme_for("ftp"), None);
        assert_eq!(ws_scheme_for(""), None);
    }
}
