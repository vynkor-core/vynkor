use super::helpers::start_kernel_secured;
use crate::jwt_helper::create_test_token;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{envelope, Envelope, ErrorCode, Ping, PluginManifest};
use veyron_sdk::VeyronClient;

#[tokio::test]
async fn secured_kernel_completes_mac_handshake_and_pings() {
    let secret = "integration-mac-secret";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/veyron_mac_handshake.sock", 19500, secret).await;

    let token = create_test_token("mac-plugin", vec![], secret.as_bytes(), 3600);

    let mut client =
        VeyronClient::connect_with_secret("/tmp/veyron_mac_handshake.sock", secret.as_bytes())
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
    let secret = "integration-mac-secret-2";
    let (_shutdown, _reg, _bus) =
        start_kernel_secured("/tmp/veyron_mac_reject.sock", 19501, secret).await;

    let token = create_test_token("plain-plugin", vec![], secret.as_bytes(), 3600);

    // Connect WITHOUT the secret: the client never derives the MAC key, so its
    // post-registration frames are un-tagged. The kernel must drop the connection.
    let mut client = VeyronClient::connect("/tmp/veyron_mac_reject.sock")
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
