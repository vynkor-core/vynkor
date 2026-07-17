use prost::Message;
use std::time::Duration;
use veyron::proto::veyron::{
    envelope, ActionRequest, ActionResponse, ActionStatus, CommandStatus, Envelope, PluginManifest,
};
use veyron_sdk::VeyronClient;

use super::sdk_harness::SdkHarness;

/// Register → ping round-trip via Rust SDK against a live kernel.
#[tokio::test]
async fn rust_sdk_register_and_ping() {
    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap();

    let mut client = VeyronClient::connect(socket)
        .await
        .expect("SDK connect failed");

    client
        .register("sdk-rust-ping", PluginManifest::default())
        .await
        .expect("register failed");

    tokio::time::timeout(Duration::from_secs(2), client.ping())
        .await
        .expect("ping timed out")
        .expect("ping failed");
}

/// Send an ActionRequest from plugin A to plugin B; B receives and responds.
/// Sender must declare PERMISSION_IPC_SEND and list the target in ipc_targets.
#[tokio::test]
async fn rust_sdk_send_action_request_and_receive_response() {
    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap();

    let mut client_a = VeyronClient::connect(socket)
        .await
        .expect("connect A failed");
    client_a
        .register(
            "sdk-action-a",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["sdk-action-b".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("register A failed");

    let mut client_b = VeyronClient::connect(socket)
        .await
        .expect("connect B failed");
    client_b
        .register(
            "sdk-action-b",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["sdk-action-a".to_string()],
                ..Default::default()
            },
        )
        .await
        .expect("register B failed");

    // A sends ActionRequest to B (encode manually like the routing test).
    let req_env = Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: "act-001".to_string(),
            action: "echo".to_string(),
            params_json: br#"{"msg":"hello"}"#.to_vec(),
            timeout_ms: 3000,
            streaming: false,
            ..Default::default()
        })),
        ..Default::default()
    };
    let mut payload = Vec::new();
    req_env.encode(&mut payload).unwrap();
    client_a
        .send_raw("sdk-action-b", payload)
        .await
        .expect("send ActionRequest failed");

    // B receives the ActionRequest.
    let received = tokio::time::timeout(Duration::from_secs(2), client_b.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    let action_id = match &received.payload {
        Some(envelope::Payload::ActionRequest(ar)) => {
            assert_eq!(ar.action, "echo");
            assert_eq!(ar.action_id, "act-001");
            ar.action_id.clone()
        }
        other => panic!("expected ActionRequest, got {other:?}"),
    };

    // B responds with ActionResponse.
    let resp_env = Envelope {
        payload: Some(envelope::Payload::ActionResponse(ActionResponse {
            action_id,
            status: ActionStatus::ActionOk as i32,
            data_json: br#"{"echo":"hello"}"#.to_vec(),
            error: String::new(),
        })),
        ..Default::default()
    };
    let mut resp_payload = Vec::new();
    resp_env.encode(&mut resp_payload).unwrap();
    client_b
        .send_raw("sdk-action-a", resp_payload)
        .await
        .expect("send ActionResponse failed");

    // A receives the ActionResponse.
    let response_env = tokio::time::timeout(Duration::from_secs(2), client_a.recv())
        .await
        .expect("recv response timed out")
        .expect("recv response failed");

    match response_env.payload {
        Some(envelope::Payload::ActionResponse(ar)) => {
            assert_eq!(ar.action_id, "act-001");
            assert!(ar.error.is_empty(), "unexpected error: {}", ar.error);
            assert_eq!(ar.status, ActionStatus::ActionOk as i32);
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }
}

/// Send a kernel command via the SDK and verify the ACK.
#[tokio::test]
async fn rust_sdk_kernel_command_health_check() {
    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap();

    let mut client = VeyronClient::connect(socket)
        .await
        .expect("SDK connect failed");

    client
        .register("sdk-cmd-test", PluginManifest::default())
        .await
        .expect("register failed");

    let ack = tokio::time::timeout(
        Duration::from_secs(2),
        client.send_command("cmd-1", "health_check", b"{}"),
    )
    .await
    .expect("command timed out")
    .expect("command failed");

    assert_eq!(
        ack.status,
        CommandStatus::CommandOk as i32,
        "health_check should return COMMAND_OK"
    );
    let json = String::from_utf8(ack.data_json).unwrap();
    assert!(json.contains("uptime_secs"), "json={json}");
    assert!(json.contains("plugin_count"), "json={json}");
}
