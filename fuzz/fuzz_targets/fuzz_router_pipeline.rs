#![no_main]
use libfuzzer_sys::fuzz_target;
use prost::Message;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use veyron::events::bus::EventBus;
use veyron::ipc::connection::Outbound;
use veyron::ipc::framing::Frame;
use veyron::ipc::messages::IncomingMessage;
use veyron::ipc::protocol::MessageRouter;
use veyron::plugins::registry::PluginRegistry;
use veyron::proto::veyron::{envelope, Envelope, PluginManifest, PluginRegister};

/// Feed arbitrary bytes as an IPC frame payload through the full router pipeline.
///
/// Registers two plugins first (deterministic), then feeds fuzz data as a
/// message from plugin_a targeting plugin_b or "kernel". The router must
/// never panic regardless of payload content.
fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();

    rt.block_on(async {
        let registry = Arc::new(PluginRegistry::new());
        let bus = Arc::new(EventBus::new());

        let (router_tx, router_rx) = mpsc::channel::<IncomingMessage>(64);
        tokio::spawn(MessageRouter::run(router_rx, Arc::clone(&registry), Arc::clone(&bus), None));

        let (a_tx, mut a_rx) = mpsc::channel::<Outbound>(32);
        let (b_tx, _b_rx) = mpsc::channel::<Outbound>(32);

        // Register plugin_a via the router (exercises registration path)
        let reg_env = Envelope {
            payload: Some(envelope::Payload::PluginRegister(PluginRegister {
                plugin_id: "fuzz_a".to_string(),
                manifest: Some(PluginManifest {
                    permissions: vec!["PERMISSION_IPC_SEND".to_string()],
                    ipc_targets: vec!["fuzz_b".to_string()],
                    ..Default::default()
                }),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut reg_bytes = vec![];
        reg_env.encode(&mut reg_bytes).unwrap();
        let _ = router_tx
            .send(IncomingMessage {
                conn_id: 1,
                frame: make_kernel_frame(reg_bytes),
                write_tx: a_tx.clone(),
                session_key: Arc::new(Mutex::new(None)),
            })
            .await;
        // consume ack
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            a_rx.recv(),
        )
        .await;

        // Register plugin_b directly in registry (bypass router to keep target focused)
        let _ = registry.register("fuzz_b".to_string(), 2, PluginManifest::default(), b_tx);

        // Now send the fuzz data as a frame from conn_id=1 targeting "kernel"
        let _ = router_tx
            .send(IncomingMessage {
                conn_id: 1,
                frame: make_frame("kernel", data.to_vec()),
                write_tx: a_tx.clone(),
                session_key: Arc::new(Mutex::new(None)),
            })
            .await;

        // And again targeting plugin_b (exercises forward path)
        let _ = router_tx
            .send(IncomingMessage {
                conn_id: 1,
                frame: make_frame("fuzz_b", data.to_vec()),
                write_tx: a_tx.clone(),
                session_key: Arc::new(Mutex::new(None)),
            })
            .await;

        // Drain responses briefly so the router task can finish
        let _ = tokio::time::timeout(std::time::Duration::from_millis(50), async {
            while a_rx.recv().await.is_some() {}
        })
        .await;
    });
});

fn make_frame(target: &str, payload: Vec<u8>) -> Frame {
    let crc = crc32fast::hash(&payload);
    let mut t = [0u8; 32];
    let b = target.as_bytes();
    t[..b.len().min(32)].copy_from_slice(&b[..b.len().min(32)]);
    Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: t,
        crc32: crc,
        payload: payload.into(),
        mac: None,
    }
}

fn make_kernel_frame(payload: Vec<u8>) -> Frame {
    make_frame("kernel", payload)
}
