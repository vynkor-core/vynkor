use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use vynkor::proto::vynkor::{envelope, PluginManifest};
use vynkor_sdk::VynkorClient;

#[tokio::test]
async fn ping_pong_round_trip() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/vynkor_integ_ping.sock", 19206).await;

    let mut client = VynkorClient::connect("/tmp/vynkor_integ_ping.sock")
        .await
        .unwrap();
    client
        .register("pinger", PluginManifest::default())
        .await
        .unwrap();

    let rtt = timeout(Duration::from_secs(2), client.ping())
        .await
        .expect("ping timed out")
        .expect("ping failed");

    assert!(
        rtt < Duration::from_secs(1),
        "RTT must be < 1s, got {rtt:?}"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn pong_carries_original_timestamp() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/vynkor_integ_pong.sock", 19207).await;

    let mut client = VynkorClient::connect("/tmp/vynkor_integ_pong.sock")
        .await
        .unwrap();
    client
        .register("ts_checker", PluginManifest::default())
        .await
        .unwrap();

    use prost::Message;
    use vynkor::proto::vynkor::{Envelope, Ping};
    let ping_env = Envelope {
        payload: Some(envelope::Payload::Ping(Ping { timestamp: 12345 })),
        ..Default::default()
    };
    let mut buf = vec![];
    ping_env.encode(&mut buf).unwrap();
    client.send_raw("kernel", buf).await.unwrap();

    let response = timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match response.payload {
        Some(envelope::Payload::Pong(pong)) => {
            assert_eq!(
                pong.original_timestamp, 12345,
                "pong must echo original timestamp"
            );
        }
        other => panic!("expected Pong, got: {:?}", other),
    }

    let _ = shutdown_tx.send(());
}
