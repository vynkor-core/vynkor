use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use veyron::ipc::framing::FLAG_RAW_BINARY;
use veyron::proto::veyron::{envelope, ErrorCode, PluginManifest};
use veyron_sdk::VeyronClient;

/// Plugin without PERMISSION_AUDIO_STREAM sends a FLAG_RAW_BINARY frame →
/// receives ERR_PERMISSION_DENIED; the target plugin receives nothing.
#[tokio::test]
async fn audio_stream_denied_without_permission() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_audio_perm.sock", 19250).await;

    let mut sender = VeyronClient::connect("/tmp/veyron_integ_audio_perm.sock")
        .await
        .unwrap();
    let mut target = VeyronClient::connect("/tmp/veyron_integ_audio_perm.sock")
        .await
        .unwrap();

    // sender has IPC_SEND + target listed, but NOT PERMISSION_AUDIO_STREAM
    sender
        .register(
            "audio_sender",
            PluginManifest {
                permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                ipc_targets: vec!["audio_target".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    target
        .register("audio_target", PluginManifest::default())
        .await
        .unwrap();

    // Send raw binary frame (FLAG_RAW_BINARY) without audio stream permission
    sender
        .send_raw_with_flags("audio_target", FLAG_RAW_BINARY, b"raw-pcm-data".to_vec())
        .await
        .unwrap();

    // Sender must receive ERR_PERMISSION_DENIED
    let err_env = timeout(Duration::from_secs(2), sender.recv())
        .await
        .expect("recv timed out")
        .expect("recv failed");

    match err_env.payload {
        Some(envelope::Payload::Error(e)) => {
            assert_eq!(
                e.code,
                ErrorCode::ErrPermissionDenied as i32,
                "expected ERR_PERMISSION_DENIED, got code {}",
                e.code
            );
        }
        other => panic!("expected ErrorMessage, got: {:?}", other),
    }

    // Target must receive nothing
    let target_recv = timeout(Duration::from_millis(200), target.recv()).await;
    assert!(
        target_recv.is_err(),
        "target must not receive a FLAG_RAW_BINARY frame from unpermissioned sender"
    );

    let _ = shutdown_tx.send(());
}

/// Plugin WITH PERMISSION_AUDIO_STREAM can send FLAG_RAW_BINARY frames.
#[tokio::test]
async fn audio_stream_allowed_with_permission() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_audio_perm2.sock", 19251).await;

    let mut sender = VeyronClient::connect("/tmp/veyron_integ_audio_perm2.sock")
        .await
        .unwrap();
    let mut target = VeyronClient::connect("/tmp/veyron_integ_audio_perm2.sock")
        .await
        .unwrap();

    sender
        .register(
            "audio_sender2",
            PluginManifest {
                permissions: vec![
                    "PERMISSION_IPC_SEND".to_string(),
                    "PERMISSION_AUDIO_STREAM".to_string(),
                ],
                ipc_targets: vec!["audio_target2".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();
    target
        .register("audio_target2", PluginManifest::default())
        .await
        .unwrap();

    sender
        .send_raw_with_flags("audio_target2", FLAG_RAW_BINARY, b"raw-pcm-data".to_vec())
        .await
        .unwrap();

    // Sender must NOT receive ERR_PERMISSION_DENIED (no reply at all = routed successfully)
    let sender_reply = timeout(Duration::from_millis(300), sender.recv()).await;
    if let Ok(Ok(env)) = sender_reply {
        if let Some(envelope::Payload::Error(e)) = env.payload {
            assert_ne!(
                e.code,
                ErrorCode::ErrPermissionDenied as i32,
                "permissioned plugin must not receive ERR_PERMISSION_DENIED"
            );
        }
    }
    // Raw binary frames are opaque to the SDK recv() (not a valid Envelope),
    // so we don't assert on target.recv() — just confirm routing was not blocked.

    let _ = shutdown_tx.send(());
}
