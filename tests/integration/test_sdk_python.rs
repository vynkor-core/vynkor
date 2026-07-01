/// Python SDK integration tests.
///
/// Runs the Python SDK client script against a live kernel via `python3`.
/// Skipped when Python 3 is not available or the SDK package is not installed.
/// The client script registers as a plugin, pings the kernel, and exits 0 on
/// success. Any non-zero exit code is a test failure.
use std::process::Command;
use std::time::Duration;

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
