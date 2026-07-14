/// Python SDK integration tests.
///
/// Runs the Python SDK client script against a live kernel via `python3`.
/// Skipped when Python 3 is not available or the SDK package is not installed.
/// The client script registers as a plugin, pings the kernel, and exits 0 on
/// success. Any non-zero exit code is a test failure.
use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;
use veyron::proto::veyron::{envelope, ActionStatus, PluginManifest};
use veyron_sdk::VeyronClient;

use super::sdk_harness::SdkHarness;

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn sdk_python_dir() -> std::path::PathBuf {
    std::env::current_dir()
        .unwrap_or_default()
        .join("sdk/python")
}

/// Cross-SDK integration tests spawn the real `examples/echo_plugin.py`
/// subprocess (not an inline `-c` script) — it needs `google.protobuf` on
/// the system python3 (same dependency the existing `python_sdk_*` tests
/// skip on via `ModuleNotFoundError`/`ImportError` string-matching), checked
/// upfront here so each round-trip test's happy path doesn't need to guess
/// whether a failure came from a missing dependency mid-test.
fn python_deps_available() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("import google.protobuf, veyron")
        .current_dir(sdk_python_dir())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the Python SDK smoke test: register + ping + graceful exit.
#[tokio::test]
async fn python_sdk_register_and_ping() {
    if !python3_available() {
        eprintln!("[SKIP] python3 not found");
        return;
    }

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();

    // Wait a little for the kernel to be fully up.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The Python SDK test script: registers as "sdk-python-test", pings, exits 0.
    let script = format!(
        r#"
import sys
import asyncio
sys.path.insert(0, "{sdk_dir}")
from veyron.client import VeyronClient

async def main():
    c = VeyronClient("{socket}")
    await c.connect()
    try:
        await c.register("sdk-python-test")
        await c.ping()
        print("ok")
    finally:
        await c.close()

asyncio.run(main())
"#,
        sdk_dir = sdk_python_dir().display(),
        socket = socket,
    );

    let output = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .output()
        .await
        .expect("failed to run python3");

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // If the Python SDK isn't installed / async client not available, skip.
        if stderr.contains("ModuleNotFoundError") || stderr.contains("ImportError") {
            eprintln!("[SKIP] Python SDK not installed: {stderr}");
            return;
        }

        panic!(
            "Python SDK test failed (exit {}):\nstdout: {stdout}\nstderr: {stderr}",
            output.status
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ok"), "expected 'ok', got: {stdout}");
}

/// R5-02: a >= 64 KiB (COMPRESS_THRESHOLD) event published kernel-to-plugin
/// arrives compressed on the wire (`FLAG_COMPRESSED`); the Python SDK must
/// transparently decompress it. The Python client subscribes to an event
/// type, the harness publishes a 100 KiB event directly via `EventBus`
/// (bypassing the plugin-to-plugin IPC permission gate, which is unrelated
/// to compression), and the client asserts the decompressed bytes round-trip
/// exactly.
#[tokio::test]
async fn python_sdk_large_frame_round_trip() {
    if !python3_available() {
        eprintln!("[SKIP] python3 not found");
        return;
    }

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut child = tokio::process::Command::new("python3")
        .arg("-c")
        .arg(format!(
            r#"
import sys
import asyncio
sys.path.insert(0, "{sdk_dir}")
from veyron.client import VeyronClient

PAYLOAD = bytes((i % 256 for i in range(100_000)))

async def main():
    c = VeyronClient("{socket}")
    await c.connect()
    await c.register("sdk-python-large-recv")
    await c.subscribe(["large.event"])
    print("subscribed", flush=True)

    got = await c.recv()
    assert got.HasField("event"), f"expected event, got {{got}}"
    assert got.event.payload_json == PAYLOAD, "decompressed payload mismatch"
    print("ok")

    await c.close()

asyncio.run(main())
"#,
            sdk_dir = sdk_python_dir().display(),
            socket = socket,
        ))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn python3");

    // Wait for the subscriber to register + subscribe before publishing.
    let stdout = child.stdout.take().unwrap();
    let mut reader = tokio::io::BufReader::new(stdout).lines();
    let first_line = tokio::time::timeout(Duration::from_secs(5), reader.next_line())
        .await
        .expect("timed out waiting for subscription")
        .expect("failed reading subscriber stdout");
    if first_line.as_deref() != Some("subscribed") {
        let out = child.wait_with_output().await.expect("wait failed");
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("ModuleNotFoundError") || stderr.contains("ImportError") {
            eprintln!("[SKIP] Python SDK not installed: {stderr}");
            return;
        }
        panic!("subscriber did not report ready, got: {first_line:?}\nstderr: {stderr}");
    }

    // Give the kernel a moment to register the subscription before publishing.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 256) as u8).collect();
    harness
        .event_bus
        .publish(
            veyron::proto::veyron::Event {
                event_id: "large-1".to_string(),
                event_type: "large.event".to_string(),
                payload_json: payload,
                retry_count: 0,
            },
            &harness.registry,
        )
        .await;

    // Drain the remaining stdout lines (the "ok" line printed after recv()).
    let mut rest = String::new();
    while let Ok(Ok(Some(line))) =
        tokio::time::timeout(Duration::from_secs(5), reader.next_line()).await
    {
        rest.push_str(&line);
        rest.push('\n');
    }

    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("subscriber timed out")
        .expect("wait failed");

    if !status.success() || !rest.contains("ok") {
        let mut stderr_buf = Vec::new();
        if let Some(mut stderr) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_end(&mut stderr_buf).await;
        }
        let stderr = String::from_utf8_lossy(&stderr_buf);
        panic!(
            "Python SDK large-frame test failed (exit {status}):\nstdout: {rest}\nstderr: {stderr}"
        );
    }
}

/// Streaming ActionRequest round trip against the real Python echo_plugin's
/// "stream_echo" action: early accept, 2 chunks up, 2 chunks back, terminal
/// response with the concatenated bytes.
#[tokio::test]
async fn python_sdk_streaming_action_round_trip() {
    if !python3_available() {
        eprintln!("[SKIP] python3 not found");
        return;
    }
    if !python_deps_available() {
        eprintln!("[SKIP] Python SDK deps (google.protobuf/veyron) not importable");
        return;
    }

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut plugin = tokio::process::Command::new("python3")
        .arg("-m")
        .arg("examples.echo_plugin")
        .current_dir(sdk_python_dir())
        .env("VEYRON_SOCKET_PATH", &socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("failed to spawn python3 echo_plugin");

    tokio::time::sleep(Duration::from_millis(400)).await;

    let mut client = VeyronClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register("test-py-stream-sender", PluginManifest::default())
        .await
        .expect("register failed");

    tokio::time::sleep(Duration::from_millis(150)).await;

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

    let accept = tokio::time::timeout(Duration::from_secs(5), client.recv())
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
        let env = tokio::time::timeout(Duration::from_secs(5), client.recv())
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

    let terminal = tokio::time::timeout(Duration::from_secs(5), client.recv())
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

    let _ = plugin.kill().await;
    let _ = plugin.wait().await;
}
