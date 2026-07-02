/// Python SDK integration tests.
///
/// Runs the Python SDK client script against a live kernel via `python3`.
/// Skipped when Python 3 is not available or the SDK package is not installed.
/// The client script registers as a plugin, pings the kernel, and exits 0 on
/// success. Any non-zero exit code is a test failure.
use std::process::Command;
use std::time::Duration;
use tokio::io::AsyncBufReadExt;

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
