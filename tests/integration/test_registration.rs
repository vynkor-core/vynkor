use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use vynkor::proto::vynkor::PluginManifest;
use vynkor_sdk::VynkorClient;

#[tokio::test]
async fn plugin_registration_accepted_and_appears_in_registry() {
    let (shutdown_tx, registry, _bus) = start_kernel("/tmp/vynkor_integ_reg.sock", 19200).await;

    let mut client = VynkorClient::connect("/tmp/vynkor_integ_reg.sock")
        .await
        .unwrap();
    let ack = timeout(
        Duration::from_secs(2),
        client.register("weather_plugin", PluginManifest::default()),
    )
    .await
    .expect("register timed out")
    .unwrap();

    assert!(ack.accepted, "ack must be accepted");
    assert!(
        registry.get("weather_plugin").is_some(),
        "plugin must appear in registry after registration"
    );

    let _ = shutdown_tx.send(());
}
