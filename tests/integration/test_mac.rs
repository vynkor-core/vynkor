use super::helpers::start_kernel_secured;
use crate::jwt_helper::create_test_token;
use std::time::Duration;
use tokio::time::timeout;
use vynkor::auth::plugin_key::plugin_mac_secret;
use vynkor::proto::vynkor::{envelope, Envelope, ErrorCode, Ping, PluginManifest};
use vynkor_sdk::VynkorClient;

#[tokio::test]
async fn secured_kernel_completes_mac_handshake_and_pings() {
    let secret = "integration-mac-secret-32-bytes-min";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/vynkor_mac_handshake.sock", 19500, secret).await;

    let token = create_test_token("mac-plugin", vec![], secret.as_bytes(), 3600);

    let mut client = VynkorClient::connect_with_secret(
        "/tmp/vynkor_mac_handshake.sock",
        plugin_mac_secret(secret.as_bytes(), "mac-plugin").as_bytes(),
    )
    .await
    .expect("connect");
    let ack = client
        .register_with_token("mac-plugin", PluginManifest::default(), &token)
        .await
        .expect("register");
    assert!(ack.accepted, "registration must succeed");
    assert_eq!(
        ack.session_nonce.len(),
        16,
        "secured kernel must return a 16-byte session nonce"
    );

    // A ping round-trips with MAC'd frames both directions; the SDK verifies the
    // pong's tag and the kernel verified the ping's tag.
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 7 })),
        ..Default::default()
    };
    let mut buf = vec![];
    prost::Message::encode(&env, &mut buf).unwrap();
    client.send_raw("kernel", buf).await.expect("send ping");

    let reply = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("recv must not time out")
        .expect("recv ok");
    assert!(
        matches!(reply.payload, Some(envelope::Payload::Pong(_))),
        "expected Pong, got {:?}",
        reply.payload
    );
}

#[tokio::test]
async fn secured_kernel_rejects_unmaced_client() {
    let secret = "integration-mac-secret-2-32-bytes-min";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/vynkor_mac_reject.sock", 19501, secret).await;

    let token = create_test_token("plain-plugin", vec![], secret.as_bytes(), 3600);

    // Connect WITHOUT the secret: the client never derives the MAC key, so its
    // post-registration frames are un-tagged. The kernel must drop the connection.
    let mut client = VynkorClient::connect("/tmp/vynkor_mac_reject.sock")
        .await
        .expect("connect");
    let ack = client
        .register_with_token("plain-plugin", PluginManifest::default(), &token)
        .await
        .expect("register");
    assert!(ack.accepted);

    // Send an un-MAC'd ping; the kernel drops the connection, so recv eventually
    // fails (closed) rather than returning a Pong.
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 1 })),
        ..Default::default()
    };
    let mut buf = vec![];
    prost::Message::encode(&env, &mut buf).unwrap();
    let _ = client.send_raw("kernel", buf).await;

    // R5-12: the kernel now sends ERR_MAC_MISSING before dropping the
    // connection, rather than going silent (AUDIT M-05).
    let first = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("must receive an error frame before disconnect")
        .expect("recv should succeed for the error frame");
    match first.payload {
        Some(envelope::Payload::Error(e)) => {
            assert_eq!(e.code, ErrorCode::ErrMacMissing as i32);
        }
        other => panic!("expected ErrorMessage, got {other:?}"),
    }

    let got = timeout(Duration::from_secs(2), client.recv()).await;
    let dropped = match got {
        Err(_) => true,     // timed out (connection dead, no further data)
        Ok(Err(_)) => true, // read error — connection closed
        Ok(Ok(_)) => false, // got another reply — should not happen
    };
    assert!(
        dropped,
        "un-MAC'd client must be dropped by a secured kernel"
    );
}

/// Sends one MAC'd ping and reports whether a Pong came back within 2s.
async fn ping_gets_pong(client: &mut VynkorClient) -> bool {
    let env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 9 })),
        ..Default::default()
    };
    let mut buf = vec![];
    prost::Message::encode(&env, &mut buf).unwrap();
    if client.send_raw("kernel", buf).await.is_err() {
        return false;
    }
    for _ in 0..2 {
        match timeout(Duration::from_secs(2), client.recv()).await {
            Ok(Ok(e)) if matches!(e.payload, Some(envelope::Payload::Pong(_))) => return true,
            Ok(Ok(_)) => continue, // error frame before drop
            _ => return false,
        }
    }
    false
}

#[tokio::test]
async fn derived_plugin_secret_completes_handshake() {
    let secret = "integration-mac-secret-3-32-bytes-min";
    let sock = "/tmp/vynkor_mac_derived.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19502, secret).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let derived = plugin_mac_secret(secret.as_bytes(), "tg");
    let mut c = VynkorClient::connect_with_secret(sock, derived.as_bytes())
        .await
        .unwrap();
    let ack = c
        .register_with_token("tg", PluginManifest::default(), &token)
        .await
        .unwrap();
    assert!(ack.accepted);
    assert!(ping_gets_pong(&mut c).await, "derived key must MAC-verify");
}

#[tokio::test]
async fn master_secret_client_rejected_by_default() {
    let secret = "integration-mac-secret-4-32-bytes-min";
    let sock = "/tmp/vynkor_mac_master.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19503, secret).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let mut c = VynkorClient::connect_with_secret(sock, secret.as_bytes())
        .await
        .unwrap();
    let _ = c
        .register_with_token("tg", PluginManifest::default(), &token)
        .await;
    assert!(
        !ping_gets_pong(&mut c).await,
        "master secret must no longer MAC a local plugin"
    );
}

#[tokio::test]
async fn derived_key_of_other_plugin_is_rejected() {
    let secret = "integration-mac-secret-5-32-bytes-min";
    let sock = "/tmp/vynkor_mac_cross.sock";
    let (_s, _r, _b) = start_kernel_secured(sock, 19504, secret).await;
    let token = create_test_token("agent", vec![], secret.as_bytes(), 3600);
    let telegram_key = plugin_mac_secret(secret.as_bytes(), "telegram");
    let mut c = VynkorClient::connect_with_secret(sock, telegram_key.as_bytes())
        .await
        .unwrap();
    let _ = c
        .register_with_token("agent", PluginManifest::default(), &token)
        .await;
    assert!(
        !ping_gets_pong(&mut c).await,
        "telegram's key must not MAC as agent"
    );
}

#[tokio::test]
async fn legacy_flag_accepts_master_secret() {
    let secret = "integration-mac-secret-6-32-bytes-min";
    let sock = "/tmp/vynkor_mac_legacy.sock";
    let mut cfg = super::helpers::test_config(sock, 19505);
    cfg.allow_no_auth = false;
    cfg.jwt_secret = Some(secret.to_string());
    cfg.legacy_plugin_mac = true;
    let (_s, _r, _b) = super::helpers::start_kernel_with_config(cfg).await;
    let token = create_test_token("tg", vec![], secret.as_bytes(), 3600);
    let mut c = VynkorClient::connect_with_secret(sock, secret.as_bytes())
        .await
        .unwrap();
    let ack = c
        .register_with_token("tg", PluginManifest::default(), &token)
        .await
        .unwrap();
    assert!(ack.accepted);
    assert!(
        ping_gets_pong(&mut c).await,
        "legacy mode keeps master-secret MAC"
    );
}
