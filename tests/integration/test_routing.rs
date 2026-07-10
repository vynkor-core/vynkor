use super::helpers::start_kernel;
use prost::Message;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{envelope, ActionRequest, Envelope, PluginManifest};
use veyron_sdk::VeyronClient;

#[tokio::test]
async fn plugin_a_sends_to_plugin_b_and_b_receives() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_routing.sock", 19201).await;

    let mut plugin_a = VeyronClient::connect("/tmp/veyron_integ_routing.sock")
        .await
        .unwrap();
    let mut plugin_b = VeyronClient::connect("/tmp/veyron_integ_routing.sock")
        .await
        .unwrap();

    // plugin_a needs PERMISSION_IPC_SEND + ipc_targets listing plugin_b (T-04)
    plugin_a
        .register(
            "plugin_a",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["plugin_b".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    plugin_b
        .register("plugin_b", PluginManifest::default())
        .await
        .unwrap();

    // plugin_a sends ActionRequest targeting plugin_b
    let req = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: "req-001".to_string(),
            action: "do_something".to_string(),
            params_json: b"{}".to_vec(),
            timeout_ms: 0,
            streaming: false,
        })),
        ..Default::default()
    };
    let mut payload = vec![];
    req.encode(&mut payload).unwrap();
    plugin_a.send_raw("plugin_b", payload).await.unwrap();

    // plugin_b must receive it
    let received = timeout(Duration::from_secs(2), plugin_b.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    assert!(
        matches!(
            received.payload,
            Some(envelope::Payload::ActionRequest(ref r)) if r.action_id == "req-001"
        ),
        "plugin_b must receive ActionRequest from plugin_a, got: {:?}",
        received.payload
    );

    let _ = shutdown_tx.send(());
}
