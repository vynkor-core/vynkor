use super::helpers::{start_kernel, start_kernel_secured};
use crate::jwt_helper::create_test_token;
use prost::Message;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use veyron::proto::veyron::{envelope, ActionRequest, Envelope, PluginManifest};
use veyron_sdk::VeyronClient;

/// Raw HTTP GET helper — avoids adding reqwest to dev-deps.
async fn http_get_body(port: u16, path: &str) -> (u16, String) {
    http_get_body_with_auth(port, path, None).await
}

/// Raw HTTP GET helper with optional Authorization header.
async fn http_get_body_with_auth(port: u16, path: &str, token: Option<&str>) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("connect to kernel HTTP port");

    let auth_header = if let Some(t) = token {
        format!("Authorization: Bearer {}\r\n", t)
    } else {
        String::new()
    };

    let req =
        format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n{auth_header}\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let raw = String::from_utf8_lossy(&buf).to_string();
    let status: u16 = raw
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = raw
        .find("\r\n\r\n")
        .map(|pos| raw[pos + 4..].to_string())
        .unwrap_or_default();
    (status, body)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn metrics_endpoint_reachable_and_returns_200() {
    let (shutdown, _reg, _bus) = start_kernel("/tmp/veyron_metrics_reachable.sock", 19600).await;

    let (status, _body) = http_get_body(19600, "/metrics").await;
    assert_eq!(status, 200, "/metrics must return HTTP 200");

    let _ = shutdown.send(());
}

#[tokio::test]
async fn messages_routed_counter_appears_after_routing() {
    let (shutdown, _reg, _bus) = start_kernel("/tmp/veyron_metrics_routed.sock", 19601).await;

    // Two plugins; plugin_a routes a message to plugin_b.
    let mut plugin_a = VeyronClient::connect("/tmp/veyron_metrics_routed.sock")
        .await
        .unwrap();
    let mut plugin_b = VeyronClient::connect("/tmp/veyron_metrics_routed.sock")
        .await
        .unwrap();

    plugin_a
        .register(
            "metrics-a",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["metrics-b".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    plugin_b
        .register("metrics-b", PluginManifest::default())
        .await
        .unwrap();

    let env = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: "m-001".to_string(),
            action: "ping".to_string(),
            params_json: b"{}".to_vec(),
            timeout_ms: 0,
            streaming: false,
        })),
        ..Default::default()
    };
    let mut buf = vec![];
    env.encode(&mut buf).unwrap();
    plugin_a.send_raw("metrics-b", buf).await.unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;

    let (_status, body) = http_get_body(19601, "/metrics").await;
    assert!(
        body.contains("messages_routed_total"),
        "messages_routed_total must appear in /metrics after routing; body:\n{body}"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn ipc_send_denied_counter_appears_after_permission_denial() {
    let (shutdown, _reg, _bus) = start_kernel("/tmp/veyron_metrics_denied.sock", 19602).await;

    // Register plugin WITHOUT PERMISSION_IPC_SEND; attempt to send to another plugin.
    let mut plugin = VeyronClient::connect("/tmp/veyron_metrics_denied.sock")
        .await
        .unwrap();
    plugin
        .register("denied-sender", PluginManifest::default())
        .await
        .unwrap();

    let env = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: "deny-001".to_string(),
            action: "anything".to_string(),
            params_json: b"{}".to_vec(),
            timeout_ms: 0,
            streaming: false,
        })),
        ..Default::default()
    };
    let mut buf = vec![];
    env.encode(&mut buf).unwrap();
    // Send attempt will be denied; ignore the error.
    let _ = plugin.send_raw("nonexistent-target", buf).await;

    tokio::time::sleep(Duration::from_millis(50)).await;

    let (_status, body) = http_get_body(19602, "/metrics").await;
    assert!(
        body.contains("ipc_send_denied_total"),
        "ipc_send_denied_total must appear in /metrics after a denied send; body:\n{body}"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn plugins_registered_counter_appears_after_registration() {
    let (shutdown, _reg, _bus) = start_kernel("/tmp/veyron_metrics_registered.sock", 19603).await;

    let mut plugin = VeyronClient::connect("/tmp/veyron_metrics_registered.sock")
        .await
        .unwrap();
    plugin
        .register("reg-counter-plugin", PluginManifest::default())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(30)).await;

    let (_status, body) = http_get_body(19603, "/metrics").await;
    assert!(
        body.contains("plugins_registered_total"),
        "plugins_registered_total must appear in /metrics after a plugin registers; body:\n{body}"
    );

    let _ = shutdown.send(());
}

#[tokio::test]
async fn mac_error_counter_appears_after_untagged_frame_on_secured_kernel() {
    let secret = "metrics-mac-secret-32-bytes-minimum";
    let (shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/veyron_metrics_mac.sock", 19604, secret).await;

    // Connect without MAC support: the plain VeyronClient sends CRC-only frames.
    // On a secured kernel, the first frame after registration ack has FLAG_MAC_PRESENT
    // set on the outbound side, and the kernel expects it on the inbound side too.
    // An untagged inbound frame triggers the mac error counter.
    let token = create_test_token("mac-err-plugin", vec![], secret.as_bytes(), 3600);
    let mut plain = VeyronClient::connect("/tmp/veyron_metrics_mac.sock")
        .await
        .unwrap();
    // Register with a valid token but without a MAC key → kernel will drop connection
    // after the first untagged frame following the ack.
    let _ = plain
        .register_with_token("mac-err-plugin", PluginManifest::default(), &token)
        .await;
    // Send a ping (untagged) — kernel should reject it and increment the mac error counter.
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(veyron::proto::veyron::Ping {
            timestamp: 1,
        })),
        ..Default::default()
    };
    let mut buf = vec![];
    env.encode(&mut buf).unwrap();
    let _ = plain.send_raw("kernel", buf).await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    // /metrics requires auth on secured kernels, pass the JWT token
    let (_status, body) = http_get_body_with_auth(19604, "/metrics", Some(&token)).await;
    assert!(
        body.contains("ipc_frame_errors_total"),
        "ipc_frame_errors_total must appear in /metrics after a MAC failure; body:\n{body}"
    );
    assert!(
        body.contains("mac"),
        "error label must mention 'mac'; body:\n{body}"
    );

    let _ = shutdown.send(());
}
