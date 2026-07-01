/// C++ SDK integration tests.
///
/// These tests attempt to spawn the `echo_plugin_rs` binary (a Rust reference
/// plugin) and verify the kernel routes messages to it. A real C++ plugin binary
/// would be substituted here once CMake CI is wired in; the IPC contract is
/// identical. Tests are skipped when the binary has not been built.
use std::process::{Command, Stdio};
use std::time::Duration;
use veyron::proto::veyron::{
    envelope, ActionRequest, ActionResponse, ActionStatus, Envelope, PluginManifest,
};
use veyron_sdk::VeyronClient;

use super::sdk_harness::SdkHarness;

fn echo_plugin_binary() -> Option<std::path::PathBuf> {
    let bin = std::env::current_dir()
        .ok()?
        .join("target/debug/echo_plugin_rs");
    if bin.exists() {
        Some(bin)
    } else {
        None
    }
}

/// Spawn the echo plugin binary, send ActionRequest, verify ActionResponse.
#[tokio::test]
async fn cpp_sdk_echo_plugin_round_trip() {
    let Some(bin) = echo_plugin_binary() else {
        eprintln!("[SKIP] echo_plugin_rs binary not found — run `cargo build -p echo_plugin_rs`");
        return;
    };

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();

    let mut child = Command::new(&bin)
        .arg("--socket")
        .arg(&socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn echo plugin");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = VeyronClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register("test-cpp-sender", PluginManifest::default())
        .await
        .expect("register failed");

    let req = ActionRequest {
        action_id: "cpp-act-1".to_string(),
        action: "echo".to_string(),
        params_json: br#"{"text":"hello from harness"}"#.to_vec(),
        timeout_ms: 3000,
    };
    client
        .send(
            "echo",
            Envelope {
                payload: Some(envelope::Payload::ActionRequest(req)),
                ..Default::default()
            },
        )
        .await
        .expect("send failed");

    let env = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match env.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.action_id, "cpp-act-1");
            assert!(
                r.error.is_empty(),
                "echo plugin returned error: {}",
                r.error
            );
            assert_eq!(r.status, ActionStatus::ActionOk as i32);
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
