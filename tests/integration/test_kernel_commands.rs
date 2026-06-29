use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{CommandStatus, PluginManifest};
use veyron_sdk::VeyronClient;

#[tokio::test]
async fn health_check_via_ipc_returns_ok_with_json_fields() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_cmd_hc.sock", 19210).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_hc.sock")
        .await
        .unwrap();
    client
        .register("cmd-hc-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("cmd-1", "health_check", b"{}"),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandOk);
    assert!(ack.error.is_empty(), "unexpected error: {}", ack.error);
    let json = String::from_utf8(ack.data_json).unwrap();
    assert!(
        json.contains("uptime_secs"),
        "missing uptime_secs in: {json}"
    );
    assert!(
        json.contains("plugin_count"),
        "missing plugin_count in: {json}"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_command_ack_echoes_command_id() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_cmd_id.sock", 19211).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_id.sock")
        .await
        .unwrap();
    client
        .register("cmd-id-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("unique-id-42", "health_check", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.command_id, "unique-id-42");

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn unknown_command_via_ipc_returns_command_unknown() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_unk.sock", 19212).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_unk.sock")
        .await
        .unwrap();
    client
        .register("cmd-unk-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("x1", "totally_unknown_cmd", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandUnknown);
    assert!(
        ack.error.contains("totally_unknown_cmd"),
        "error should name the command, got: {}",
        ack.error
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn reload_config_without_path_returns_error_via_ipc() {
    // Kernel started without a config_file path → reload must return COMMAND_ERROR
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_reload.sock", 19213).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_reload.sock")
        .await
        .unwrap();
    client
        .register("cmd-reload-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("r1", "reload_config", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandError);
    assert!(
        ack.error.contains("no config path"),
        "expected 'no config path', got: {}",
        ack.error
    );

    let _ = shutdown_tx.send(());
}
