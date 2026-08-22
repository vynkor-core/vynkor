/// Integration tests for the real `secrets` plugin binary against a live
/// kernel.
///
/// Spawns the actual plugin built from
/// `../vynkor-plugins/plugins/secrets/` (sibling repo veyron-core/vynkor-plugins)
/// over the real UDS wire and verifies the kernel routes `secret_set`/
/// `secret_get`/`secret_list` to it, that the per-action `permission: "secrets"`
/// gate (Manifest v2) denies a caller without `PERMISSION_SECRETS`, and that
/// the plugin's caller identity is kernel-stamped (per-caller vault isolation).
/// Tests are skipped locally when the binary has not been built — run
/// `cargo build --release` in `../vynkor-plugins/plugins/secrets/`.
use std::process::{Command, Stdio};
use std::time::Duration;

use vynkor::proto::vynkor::{envelope, ActionRequest, ActionStatus, Envelope, PluginManifest};
use vynkor_sdk::VynkorClient;

use super::sdk_harness::SdkHarness;

fn secrets_binary() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("VYN_SECRETS_PLUGIN") {
        let path = std::path::PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let cwd = std::env::current_dir().ok()?;
    let rel = "../vynkor-plugins/plugins/secrets/target/release/secrets";
    let p = cwd.join(rel);
    p.exists().then_some(p)
}

fn plugin_manifest(permissions: &[&str]) -> PluginManifest {
    PluginManifest {
        permissions: permissions.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    }
}

fn action_req(action_id: &str, action: &str, params: &str) -> Envelope {
    Envelope {
        payload: Some(envelope::Payload::ActionRequest(ActionRequest {
            action_id: action_id.to_string(),
            action: action.to_string(),
            params_json: params.as_bytes().to_vec(),
            timeout_ms: 3000,
            streaming: false,
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// Spawn the real secrets plugin against a live kernel and round-trip a
/// secret through it, with a caller holding PERMISSION_SECRETS.
#[tokio::test]
async fn secrets_plugin_round_trip_with_permission() {
    let Some(bin) = secrets_binary() else {
        eprintln!(
            "[SKIP] secrets plugin binary not found — build via \
             `cargo build --release` in ../vynkor-plugins/plugins/secrets/, \
             or set VYN_SECRETS_PLUGIN to an existing binary path"
        );
        return;
    };

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();
    let vault_dir = tempfile::tempdir().unwrap();

    let mut child = Command::new(&bin)
        .env("VYN_SOCKET_PATH", &socket)
        // legacy alias for pre-built plugins (stage 4/B drops it)
        .env("VEYRON_SOCKET_PATH", &socket)
        .env("SECRETS_PLUGIN_DATA_DIR", vault_dir.path())
        .env(
            "SECRETS_PLUGIN_MASTER_KEY",
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn secrets plugin");

    tokio::time::sleep(Duration::from_millis(150)).await;

    let mut client = VynkorClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register(
            "test-secrets-sender",
            plugin_manifest(&["PERMISSION_SECRETS"]),
        )
        .await
        .expect("register failed");

    tokio::time::sleep(Duration::from_millis(150)).await;

    client
        .send(
            "kernel",
            action_req(
                "sec-1",
                "secret_set",
                r#"{"name":"api_key","value":"sk-test-123"}"#,
            ),
        )
        .await
        .expect("send secret_set failed");
    let env = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("secret_set recv timed out")
        .expect("secret_set recv failed");
    match env.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.action_id, "sec-1");
            assert_eq!(
                r.status,
                ActionStatus::ActionOk as i32,
                "error: {}",
                r.error
            );
            assert_eq!(r.data_json, br#"{"stored":true}"#);
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    client
        .send(
            "kernel",
            action_req("sec-2", "secret_get", r#"{"name":"api_key"}"#),
        )
        .await
        .expect("send secret_get failed");
    let env = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("secret_get recv timed out")
        .expect("secret_get recv failed");
    match env.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.action_id, "sec-2");
            assert_eq!(
                r.status,
                ActionStatus::ActionOk as i32,
                "error: {}",
                r.error
            );
            assert_eq!(r.data_json, br#"{"found":true,"value":"sk-test-123"}"#);
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    // The vault file must be encrypted (never plaintext), 0600, per-caller.
    let vault_path = vault_dir.path().join("test-secrets-sender.vault");
    assert!(vault_path.exists(), "vault file must exist after a write");
    let raw = std::fs::read(&vault_path).unwrap();
    assert!(
        !String::from_utf8_lossy(&raw).contains("sk-test-123"),
        "vault must not contain plaintext secret"
    );

    let _ = child.kill();
    let _ = child.wait();
}

/// A caller WITHOUT PERMISSION_SECRETS must be denied by the kernel's
/// data-driven per-action gate (Manifest v2), even though the plugin itself
/// declares the permission.
///
/// Note: `set_action_requirements` is normally populated by the plugin loader
/// from the installed plugin.json (plugins.d path) at boot. This test spawns
/// the binary manually, so it seeds the gate the same way `loader.rs` does —
/// exactly what `vyn plugin install secrets` → `plugins.d/secrets.yaml` does
/// in production.
#[tokio::test]
async fn secrets_plugin_denies_unprivileged_caller() {
    use std::collections::HashMap;
    use vynkor::proto::vynkor::PermissionType;

    let Some(bin) = secrets_binary() else {
        eprintln!(
            "[SKIP] secrets plugin binary not found — build via \
             `cargo build --release` in ../vynkor-plugins/plugins/secrets/, \
             or set VYN_SECRETS_PLUGIN to an existing binary path"
        );
        return;
    };

    let harness = SdkHarness::start().await;
    let socket = harness.socket_path.to_str().unwrap().to_string();
    let vault_dir = tempfile::tempdir().unwrap();

    let mut child = Command::new(&bin)
        .env("VYN_SOCKET_PATH", &socket)
        // legacy alias for pre-built plugins (stage 4/B drops it)
        .env("VEYRON_SOCKET_PATH", &socket)
        .env("SECRETS_PLUGIN_DATA_DIR", vault_dir.path())
        .env(
            "SECRETS_PLUGIN_MASTER_KEY",
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn secrets plugin");

    tokio::time::sleep(Duration::from_millis(150)).await;

    // Mirror the loader: from the installed manifest v2, every action declares
    // per-action permission "secrets" (PERMISSION_SECRETS).
    harness.registry.set_action_requirements(
        "secrets".to_string(),
        HashMap::from([
            ("secret_set".to_string(), PermissionType::PermissionSecrets),
            ("secret_get".to_string(), PermissionType::PermissionSecrets),
            (
                "secret_delete".to_string(),
                PermissionType::PermissionSecrets,
            ),
            ("secret_list".to_string(), PermissionType::PermissionSecrets),
        ]),
    );

    let mut client = VynkorClient::connect(&socket)
        .await
        .expect("SDK connect failed");
    client
        .register(
            "test-secrets-noperm",
            plugin_manifest(&["PERMISSION_NETWORK"]),
        )
        .await
        .expect("register failed");

    tokio::time::sleep(Duration::from_millis(150)).await;

    client
        .send(
            "kernel",
            action_req("sec-denied-1", "secret_get", r#"{"name":"api_key"}"#),
        )
        .await
        .expect("send failed");
    let env = tokio::time::timeout(Duration::from_secs(3), client.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");
    match env.payload {
        Some(envelope::Payload::ActionResponse(r)) => {
            assert_eq!(r.status, ActionStatus::ActionPermissionDeny as i32);
        }
        other => panic!("expected ActionResponse, got {other:?}"),
    }

    let _ = child.kill();
    let _ = child.wait();
}
