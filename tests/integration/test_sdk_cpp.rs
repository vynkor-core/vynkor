/// C++ SDK integration tests.
///
/// These tests spawn the real `echo_plugin` binary built from `sdk/cpp/examples/echo_plugin.cpp`
/// against `sdk/cpp/src/*` (framing, MAC, client) via CMake, and verify the kernel routes
/// messages to it over the actual C++ wire implementation. CI builds this binary before
/// running `cargo test` (see `.github/workflows/ci.yml`, job `cpp-sdk`). Tests are skipped
/// locally when the binary has not been built — see `sdk/cpp/README.md` for build steps.
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::Duration;
use veyron::proto::veyron::{envelope, ActionRequest, ActionStatus, Envelope, PluginManifest};
use veyron_sdk::VeyronClient;

use super::sdk_harness::SdkHarness;

fn echo_plugin_binary() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("VEYRON_CPP_ECHO_PLUGIN") {
        let path = std::path::PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let cwd = std::env::current_dir().ok()?;
    [
        "sdk/cpp/build/echo_plugin",
        "sdk/cpp/build/examples/echo_plugin",
    ]
    .into_iter()
    .map(|rel| cwd.join(rel))
    .find(|p| p.exists())
}

/// Spawn the real C++ echo plugin binary, send ActionRequest, verify ActionResponse.
#[tokio::test]
async fn cpp_sdk_echo_plugin_round_trip() {
    let Some(bin) = echo_plugin_binary() else {
        eprintln!(
            "[SKIP] C++ echo_plugin binary not found — build via `cmake -B sdk/cpp/build -S sdk/cpp \
             && cmake --build sdk/cpp/build --target echo_plugin` (see sdk/cpp/README.md), \
             or set VEYRON_CPP_ECHO_PLUGIN to an existing binary path"
        );
        return;
    };

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();

    let mut child = Command::new(&bin)
        .env("VEYRON_SOCKET_PATH", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn C++ echo plugin");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = VeyronClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register("test-cpp-sender", PluginManifest::default())
        .await
        .expect("register failed");

    // Give the C++ plugin time to register and declare its "echo" action
    // (find_action_provider needs it present before routing works).
    tokio::time::sleep(Duration::from_millis(100)).await;

    let req = ActionRequest {
        action_id: "cpp-act-1".to_string(),
        action: "echo".to_string(),
        params_json: br#"{"text":"hello from harness"}"#.to_vec(),
        timeout_ms: 3000,
        streaming: false,
    };
    client
        .send(
            // Kernel-brokered action routing (R5-07): the kernel looks up the
            // declared provider for "echo" (find_action_provider) and proxies
            // the response back here, translating the internal correlation id.
            "kernel",
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

/// Streaming ActionRequest round trip against the real C++ echo_plugin's
/// "stream_echo" action: early accept, 2 chunks up, 2 chunks back, terminal
/// response with the concatenated bytes.
#[tokio::test]
async fn cpp_sdk_streaming_action_round_trip() {
    let Some(bin) = echo_plugin_binary() else {
        eprintln!(
            "[SKIP] C++ echo_plugin binary not found — build via `cmake -B sdk/cpp/build -S sdk/cpp \
             && cmake --build sdk/cpp/build --target echo_plugin` (see sdk/cpp/README.md), \
             or set VEYRON_CPP_ECHO_PLUGIN to an existing binary path"
        );
        return;
    };

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();

    let mut child = Command::new(&bin)
        .env("VEYRON_SOCKET_PATH", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn C++ echo plugin");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = VeyronClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register("test-cpp-stream-sender", PluginManifest::default())
        .await
        .expect("register failed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let action_id = client
        .send_action_streaming("stream_echo", 3000)
        .await
        .expect("send_action_streaming failed");
    client
        .send_request_chunk(&action_id, 0, b"hello ".to_vec(), false)
        .await
        .expect("send chunk 0 failed");
    client
        .send_request_chunk(&action_id, 1, b"world".to_vec(), true)
        .await
        .expect("send chunk 1 failed");

    // First message back is the plugin's early session-accept ActionResponse
    // (sent before it has any chunk data — required for close_session to
    // later be honored mid-stream; see resolve_action_response in registry.rs).
    let accept = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");
    match accept.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.action_id, action_id);
            assert_eq!(r.status, ActionStatus::ActionOk as i32);
        }
        other => panic!("expected accepting ActionResponse, got {other:?}"),
    }

    let mut chunks_by_seq: HashMap<u32, Vec<u8>> = HashMap::new();
    for _ in 0..2 {
        let env = tokio::time::timeout(Duration::from_secs(3), client.recv())
            .await
            .expect("recv timed out")
            .expect("recv failed");
        match env.payload {
            Some(envelope::Payload::ActionResponseChunk(c)) => {
                assert_eq!(c.action_id, action_id);
                chunks_by_seq.insert(c.seq, c.chunk);
            }
            other => panic!("expected ActionResponseChunk, got {other:?}"),
        }
    }
    assert_eq!(chunks_by_seq.len(), 2);
    let mut reassembled = chunks_by_seq.remove(&0).expect("missing seq 0");
    reassembled.extend(chunks_by_seq.remove(&1).expect("missing seq 1"));
    assert_eq!(reassembled, b"hello world".to_vec());

    let terminal = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");
    match terminal.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.action_id, action_id);
            assert_eq!(r.status, ActionStatus::ActionOk as i32);
            assert_eq!(r.data_json, b"hello world".to_vec());
        }
        other => panic!("expected terminal ActionResponse, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
