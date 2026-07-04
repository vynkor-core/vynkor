use std::time::Duration;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use veyron::ipc::framing::write_frame;
use veyron::ipc::messages::IncomingMessage;
use veyron::ipc::server::UdsServer;

fn tmp_socket() -> std::path::PathBuf {
    std::path::PathBuf::from(format!(
        "/tmp/veyron_test_{}.sock",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ))
}

#[tokio::test]
async fn server_accepts_connection_and_receives_frame() {
    let path = tmp_socket();
    let (tx, mut rx) = mpsc::channel::<IncomingMessage>(16);

    let (_handle, _disc) = UdsServer::start(&path, tx, 1024, 30, 64)
        .await
        .expect("server must start");

    let mut client = UnixStream::connect(&path).await.expect("must connect");
    write_frame(&mut client, "kernel", 0, b"hello")
        .await
        .expect("write must succeed");

    let msg = timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("recv must not timeout")
        .expect("channel must not close");

    assert_eq!(&*msg.frame.payload, b"hello");
    assert_eq!(
        veyron::ipc::framing::target_as_str(&msg.frame),
        Some("kernel")
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn server_assigns_unique_conn_ids_to_multiple_connections() {
    let path = tmp_socket();
    let (tx, mut rx) = mpsc::channel::<IncomingMessage>(16);

    let (_handle, _disc) = UdsServer::start(&path, tx, 1024, 30, 64)
        .await
        .expect("server must start");

    let mut c1 = UnixStream::connect(&path).await.unwrap();
    let mut c2 = UnixStream::connect(&path).await.unwrap();

    write_frame(&mut c1, "kernel", 0, b"from-c1").await.unwrap();
    write_frame(&mut c2, "kernel", 0, b"from-c2").await.unwrap();

    let m1 = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let m2 = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();

    assert_ne!(
        m1.conn_id, m2.conn_id,
        "each connection must get unique conn_id"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn server_cleans_up_stale_socket_on_start() {
    let path = tmp_socket();
    // create a genuine stale UDS socket at the path (e.g. left behind by a
    // crashed prior instance) — this is safe to remove and rebind.
    let (tx0, _rx0) = mpsc::channel::<IncomingMessage>(16);
    let (_stale_handle, _stale_disc) = UdsServer::start(&path, tx0, 1024, 30, 64)
        .await
        .expect("first bind must succeed");

    let (tx, _rx) = mpsc::channel::<IncomingMessage>(16);
    // must not fail even though a stale socket file exists at the path
    let result = UdsServer::start(&path, tx, 1024, 30, 64).await;
    assert!(
        result.is_ok(),
        "server must start even with stale socket file"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn server_refuses_to_bind_over_non_socket_path() {
    let path = tmp_socket();
    // a regular file at the socket path is not ours to delete (BUG-006) —
    // the kernel must refuse to bind rather than silently unlinking it.
    std::fs::write(&path, b"not a socket").unwrap();

    let (tx, _rx) = mpsc::channel::<IncomingMessage>(16);
    let result = UdsServer::start(&path, tx, 1024, 30, 64).await;
    assert!(
        result.is_err(),
        "server must refuse to bind over a pre-existing non-socket file"
    );
    assert!(
        path.exists(),
        "the pre-existing non-socket file must not be deleted"
    );

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn server_detects_client_disconnect() {
    let path = tmp_socket();
    let (tx, mut rx) = mpsc::channel::<IncomingMessage>(16);

    let (_handle, _disc) = UdsServer::start(&path, tx, 1024, 30, 64).await.unwrap();

    {
        let mut client = UnixStream::connect(&path).await.unwrap();
        write_frame(&mut client, "kernel", 0, b"ping")
            .await
            .unwrap();
        // drop client → disconnect
    }

    // must receive the frame
    let msg = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&*msg.frame.payload, b"ping");

    // channel stays open (server still running), next recv should not panic
    // We just verify the server didn't crash by checking it can still accept
    let mut c2 = UnixStream::connect(&path)
        .await
        .expect("server must still accept after disconnect");
    write_frame(&mut c2, "kernel", 0, b"after-disconnect")
        .await
        .unwrap();

    let msg2 = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&*msg2.frame.payload, b"after-disconnect");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn connection_handler_can_send_frame_back() {
    use veyron::ipc::framing::{read_frame, Frame};

    let path = tmp_socket();
    let (tx, mut rx) = mpsc::channel::<IncomingMessage>(16);

    let (_handle, _disc) = UdsServer::start(&path, tx, 1024, 30, 64).await.unwrap();

    let mut client = UnixStream::connect(&path).await.unwrap();
    write_frame(&mut client, "kernel", 0, b"request")
        .await
        .unwrap();

    let msg = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();

    // use the write_tx from IncomingMessage to send back
    let response_payload = b"response";
    msg.write_tx
        .send(veyron::ipc::connection::out_frame(Frame {
            magic: 0x5652,
            flags: 0,
            length: response_payload.len() as u32,
            target: {
                let mut t = [0u8; 32];
                t[..6].copy_from_slice(b"client");
                t
            },
            crc32: crc32fast::hash(response_payload),
            payload: response_payload.to_vec().into(),
            mac: None,
        }))
        .await
        .expect("send to write_tx must succeed");

    let frame = timeout(Duration::from_secs(2), read_frame(&mut client))
        .await
        .expect("read must not timeout")
        .expect("frame must decode");

    assert_eq!(&*frame.payload, b"response");

    let _ = std::fs::remove_file(&path);
}
