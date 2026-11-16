//! how the bridge reaches the host. `connect` is the extension point: a new
//! transport adds a `TransportConfig` variant and an arm here that yields a
//! ready `WsStream` (or, if it is not ws-shaped, a stream enum).

use super::{tls, BridgeError, TransportKind, WsStream, BRIDGE_CONNECT_TIMEOUT};
use crate::utils::config::TransportConfig;
use axum::http::HeaderValue;
use std::sync::{Arc, Once};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{connect_async, connect_async_tls_with_config, Connector};
use tracing::warn;

impl TransportConfig {
    pub(crate) fn kind(&self) -> TransportKind {
        match self {
            TransportConfig::Direct { .. } => TransportKind::Direct,
            TransportConfig::Relay { .. } => TransportKind::Relay,
        }
    }
}

pub(crate) async fn connect(t: &TransportConfig, token: &str) -> Result<WsStream, BridgeError> {
    match t {
        TransportConfig::Direct { url, ca_cert_path } => {
            connect_direct(url, ca_cert_path.as_deref(), token).await
        }
        TransportConfig::Relay { .. } => {
            // the retry loop would repeat this every cycle
            static WARN_ONCE: Once = Once::new();
            WARN_ONCE
                .call_once(|| warn!("bridge relay transport is not implemented yet, skipping"));
            Err(BridgeError::Unsupported(
                "relay transport is not implemented yet".into(),
            ))
        }
    }
}

async fn connect_direct(
    url: &str,
    ca_cert_path: Option<&std::path::Path>,
    token: &str,
) -> Result<WsStream, BridgeError> {
    let url = super::resolve_ws_url(url)?;
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| BridgeError::Connect(e.to_string()))?;
    // same handshake as the SDK: the JWT rides the subprotocol header,
    // never the URL (access-log hygiene)
    let protocol = if token.is_empty() {
        "vynkor".to_string()
    } else {
        format!("vynkor, {token}")
    };
    let value =
        HeaderValue::from_str(&protocol).map_err(|e| BridgeError::Connect(e.to_string()))?;
    req.headers_mut().insert("sec-websocket-protocol", value);
    let connect = async {
        if url.starts_with("wss://") {
            // no ca_cert_path -> None connector = webpki roots (host with a real CA cert)
            let connector = match ca_cert_path {
                Some(path) => Some(Connector::Rustls(Arc::new(tls::load_pinned_config(path)?))),
                None => None,
            };
            connect_async_tls_with_config(req, None, false, connector)
                .await
                .map_err(BridgeError::from_ws)
        } else {
            connect_async(req).await.map_err(BridgeError::from_ws)
        }
    };
    let (ws, _resp) = tokio::time::timeout(BRIDGE_CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| BridgeError::Timeout)??;
    Ok(ws)
}
