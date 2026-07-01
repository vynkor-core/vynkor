use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use veyron::auth::frame_mac::{compute_tag, derive_session_key, verify_tag};
use veyron::ipc::framing::{
    read_frame, read_frame_with_timeout, serialize_header, target_as_str, write_frame,
    write_frame_raw, Frame, COMPRESS_THRESHOLD, FLAG_COMPRESSED, FLAG_MAC_PRESENT,
    MAX_PAYLOAD_SIZE,
};
use veyron::utils::errors::VeyronError;

async fn make_pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().expect("UnixStream::pair failed")
}

/// Build a valid 44-byte frame header declaring `len` payload bytes.
fn header_declaring(len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    h[0..2].copy_from_slice(&0x5652u16.to_be_bytes()); // magic
    h[4..8].copy_from_slice(&len.to_be_bytes()); // length
    h[8..14].copy_from_slice(b"kernel"); // target
    h // flags + crc left zero
}

#[tokio::test]
async fn stalled_payload_times_out() {
    let (mut writer, mut reader) = make_pair().await;
    // header promises 100 payload bytes, but we send none and keep the stream open
    writer.write_all(&header_declaring(100)).await.unwrap();

    let res = read_frame_with_timeout(&mut reader, Duration::from_millis(150)).await;
    assert!(
        matches!(res, Err(VeyronError::FrameReadTimeout)),
        "stalled payload must time out, got {res:?}"
    );
    drop(writer);
}

#[tokio::test]
async fn idle_connection_does_not_time_out() {
    let (writer, mut reader) = make_pair().await;
    // nothing written: no frame has started, so the read must keep waiting past
    // the frame timeout window (the timeout covers an in-progress frame only).
    let outer = tokio::time::timeout(
        Duration::from_millis(200),
        read_frame_with_timeout(&mut reader, Duration::from_millis(50)),
    )
    .await;
    assert!(
        outer.is_err(),
        "idle read must not return — no frame started"
    );
    drop(writer);
}

#[tokio::test]
async fn frame_round_trip_produces_identical_frame() {
    let (mut writer, mut reader) = make_pair().await;
    let payload = b"hello veyron";
    let target = "weather";
    let flags: u16 = 0;

    write_frame(&mut writer, target, flags, payload)
        .await
        .expect("write_frame must succeed");

    let frame = read_frame(&mut reader)
        .await
        .expect("read_frame must succeed");

    assert_eq!(frame.magic, 0x5652, "magic must be 0x5652 (VR)");
    assert_eq!(frame.flags, flags);
    assert_eq!(frame.length as usize, payload.len());
    assert_eq!(frame.payload, payload);
    assert_eq!(target_as_str(&frame), Some(target));
}

#[tokio::test]
async fn target_padded_to_32_bytes_in_frame() {
    let (mut writer, mut reader) = make_pair().await;
    write_frame(&mut writer, "plug", 0, b"x").await.unwrap();
    let frame = read_frame(&mut reader).await.unwrap();
    assert_eq!(frame.target.len(), 32);
    assert_eq!(&frame.target[..4], b"plug");
    assert!(
        frame.target[4..].iter().all(|&b| b == 0),
        "padding must be null bytes"
    );
}

#[tokio::test]
async fn mac_frame_round_trips_with_tag() {
    use veyron::ipc::framing::{write_frame_raw, FLAG_MAC_PRESENT};
    let (mut w, mut r) = make_pair().await;

    let payload = b"secured".to_vec();
    let crc = crc32fast::hash(&payload);
    let mut target = [0u8; 32];
    target[..6].copy_from_slice(b"kernel");
    let frame = Frame {
        magic: 0x5652,
        flags: FLAG_MAC_PRESENT,
        length: payload.len() as u32,
        target,
        crc32: crc,
        payload: payload.clone(),
        mac: Some([7u8; 32]),
    };
    write_frame_raw(&mut w, &frame).await.unwrap();
    drop(w);

    let got = read_frame(&mut r).await.unwrap();
    assert_eq!(got.flags & FLAG_MAC_PRESENT, FLAG_MAC_PRESENT);
    assert_eq!(
        got.length as usize,
        payload.len(),
        "length excludes the tag"
    );
    assert_eq!(got.payload, payload);
    assert_eq!(got.mac, Some([7u8; 32]));
}

#[tokio::test]
async fn non_mac_frame_has_no_tag() {
    let (mut w, mut r) = make_pair().await;
    write_frame(&mut w, "x", 0, b"plain").await.unwrap();
    drop(w);
    let got = read_frame(&mut r).await.unwrap();
    assert_eq!(got.mac, None);
}

#[tokio::test]
async fn target_as_str_trims_null_padding() {
    let mut frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: 0,
        target: [0u8; 32],
        crc32: 0,
        payload: vec![],
        mac: None,
    };
    frame.target[..6].copy_from_slice(b"kernel");
    assert_eq!(target_as_str(&frame), Some("kernel"));
}

#[tokio::test]
async fn target_as_str_returns_none_on_invalid_utf8() {
    let mut frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: 0,
        target: [0u8; 32],
        crc32: 0,
        payload: vec![],
        mac: None,
    };
    // 0xFF 0xFE is not valid UTF-8
    frame.target[0] = 0xFF;
    frame.target[1] = 0xFE;
    assert_eq!(target_as_str(&frame), None);
}

#[tokio::test]
async fn magic_mismatch_returns_error() {
    let (mut writer, mut reader) = make_pair().await;

    let mut bad_header = [0u8; 44];
    bad_header[0] = 0xDE;
    bad_header[1] = 0xAD;

    use tokio::io::AsyncWriteExt;
    writer
        .write_all(&bad_header)
        .await
        .expect("write must not fail");
    drop(writer);

    let result = read_frame(&mut reader).await;
    assert!(
        matches!(result, Err(VeyronError::FrameMagicMismatch)),
        "expected FrameMagicMismatch, got {:?}",
        result
    );
}

#[tokio::test]
async fn crc32_mismatch_returns_error() {
    let (mut writer, _reader) = make_pair().await;

    // write a valid-looking frame but corrupt the CRC field
    let payload = b"data";
    write_frame(&mut writer, "target", 0, payload)
        .await
        .unwrap();
    drop(writer);

    // read raw bytes, corrupt crc32 field (bytes 40-43), replay to reader
    // Instead: build a raw frame with wrong crc directly
    let (mut w2, mut r2) = make_pair().await;
    let mut header = [0u8; 44];
    header[0] = 0x56;
    header[1] = 0x52;
    // flags = 0, length = 4 (big-endian)
    header[4] = 0;
    header[5] = 0;
    header[6] = 0;
    header[7] = 4;
    // target = "target\0..."
    header[8..14].copy_from_slice(b"target");
    // crc32 = 0xDEADBEEF (wrong)
    header[40] = 0xDE;
    header[41] = 0xAD;
    header[42] = 0xBE;
    header[43] = 0xEF;

    use tokio::io::AsyncWriteExt;
    w2.write_all(&header).await.unwrap();
    w2.write_all(payload).await.unwrap();
    drop(w2);

    let result = read_frame(&mut r2).await;
    assert!(
        matches!(result, Err(VeyronError::FrameCrcMismatch)),
        "expected FrameCrcMismatch, got {:?}",
        result
    );
}

#[tokio::test]
async fn payload_too_large_returns_error() {
    let (mut w, mut r) = make_pair().await;

    // write a header claiming payload > MAX_PAYLOAD_SIZE
    let mut header = [0u8; 44];
    header[0] = 0x56;
    header[1] = 0x52;
    let too_large = (MAX_PAYLOAD_SIZE + 1) as u32;
    header[4..8].copy_from_slice(&too_large.to_be_bytes());

    use tokio::io::AsyncWriteExt;
    w.write_all(&header).await.unwrap();
    drop(w);

    let result = read_frame(&mut r).await;
    assert!(
        matches!(result, Err(VeyronError::PayloadTooLarge(_))),
        "expected PayloadTooLarge, got {:?}",
        result
    );
}

#[tokio::test]
async fn write_frame_rejects_payload_too_large() {
    let (mut w, _r) = make_pair().await;
    let huge = vec![0u8; MAX_PAYLOAD_SIZE + 1];
    let result = write_frame(&mut w, "x", 0, &huge).await;
    assert!(
        matches!(result, Err(VeyronError::PayloadTooLarge(_))),
        "expected PayloadTooLarge, got {:?}",
        result
    );
}

#[tokio::test]
async fn write_frame_raw_rejects_payload_too_large() {
    let (mut w, _r) = make_pair().await;
    let frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: (MAX_PAYLOAD_SIZE + 1) as u32,
        target: [0u8; 32],
        crc32: 0,
        payload: vec![0u8; MAX_PAYLOAD_SIZE + 1],
        mac: None,
    };
    let result = write_frame_raw(&mut w, &frame).await;
    assert!(
        matches!(result, Err(VeyronError::PayloadTooLarge(_))),
        "expected PayloadTooLarge, got {:?}",
        result
    );
}

#[tokio::test]
async fn large_payload_compressed_in_transit_and_decompressed_on_read() {
    // Payload above COMPRESS_THRESHOLD: must be compressed on wire, decompressed on read.
    let (mut w, mut r) = make_pair().await;
    let payload = vec![b'A'; COMPRESS_THRESHOLD + 1024]; // highly compressible
    let frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: [0u8; 32],
        crc32: crc32fast::hash(&payload),
        payload: payload.clone(),
        mac: None,
    };
    write_frame_raw(&mut w, &frame).await.expect("write failed");

    let received = read_frame(&mut r).await.expect("read failed");
    // Router sees plaintext: payload is always decompressed, and flags/length
    // are normalized to describe the plaintext, not the wire bytes.
    assert_eq!(received.payload, payload, "payload must round-trip intact");
    assert_eq!(
        received.flags & FLAG_COMPRESSED,
        0,
        "FLAG_COMPRESSED must be cleared on received frame"
    );
    assert_eq!(
        received.length as usize,
        payload.len(),
        "length must reflect decompressed size"
    );
}

#[tokio::test]
async fn small_payload_not_compressed() {
    // Payload below COMPRESS_THRESHOLD: must NOT be compressed.
    let (mut w, mut r) = make_pair().await;
    let payload = vec![b'B'; 1024]; // below threshold
    let frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: [0u8; 32],
        crc32: crc32fast::hash(&payload),
        payload: payload.clone(),
        mac: None,
    };
    write_frame_raw(&mut w, &frame).await.expect("write failed");

    let received = read_frame(&mut r).await.expect("read failed");
    assert_eq!(received.payload, payload, "payload must round-trip intact");
    assert_eq!(
        received.flags & FLAG_COMPRESSED,
        0,
        "FLAG_COMPRESSED must NOT be set for small payload"
    );
}

#[tokio::test]
async fn large_payload_with_mac_verifies_on_auth_connection() {
    // BUG-001 regression: MAC is tagged over the plaintext header+payload
    // before write_frame_raw compresses. The receiver must normalize the
    // decompressed frame back to plaintext flags/length so the tag matches.
    let (mut w, mut r) = make_pair().await;
    let key = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "plugin");
    let payload = vec![b'A'; COMPRESS_THRESHOLD + 1024];

    let mut frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: [0u8; 32],
        crc32: crc32fast::hash(&payload),
        payload: payload.clone(),
        mac: None,
    };
    frame.flags |= FLAG_MAC_PRESENT;
    let header = serialize_header(&frame);
    frame.mac = Some(compute_tag(&key, &header, &frame.payload));

    write_frame_raw(&mut w, &frame).await.expect("write failed");

    let received = read_frame(&mut r).await.expect("read failed");
    assert_eq!(received.payload, payload, "payload must round-trip intact");
    let recv_header = serialize_header(&received);
    let tag = received.mac.expect("MAC tag must be present");
    assert!(
        verify_tag(&key, &recv_header, &received.payload, &tag),
        "MAC must verify for a >=64KiB frame on an auth-enabled connection"
    );
}

#[tokio::test]
async fn decompressed_frame_recompresses_cleanly_on_forward() {
    // BUG-002 regression: a frame decompressed on read must not retain
    // FLAG_COMPRESSED, or re-forwarding it via write_frame_raw would skip
    // recompression and emit plaintext bytes falsely flagged compressed.
    let (mut w1, mut r1) = make_pair().await;
    let payload = vec![b'Z'; COMPRESS_THRESHOLD + 2048];
    let frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: payload.len() as u32,
        target: [0u8; 32],
        crc32: crc32fast::hash(&payload),
        payload: payload.clone(),
        mac: None,
    };
    write_frame_raw(&mut w1, &frame)
        .await
        .expect("write failed");
    let received = read_frame(&mut r1).await.expect("read failed");
    assert_eq!(received.flags & FLAG_COMPRESSED, 0);

    // Router re-forwards the decompressed frame to a second peer.
    let (mut w2, mut r2) = make_pair().await;
    write_frame_raw(&mut w2, &received)
        .await
        .expect("forward failed");
    let forwarded = read_frame(&mut r2).await.expect("forwarded read failed");
    assert_eq!(
        forwarded.payload, payload,
        "forwarded payload must decompress intact"
    );
}
