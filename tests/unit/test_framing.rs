use tokio::net::UnixStream;
use veyron::ipc::framing::{read_frame, target_as_str, write_frame, Frame, MAX_PAYLOAD_SIZE};
use veyron::utils::errors::VeyronError;

async fn make_pair() -> (UnixStream, UnixStream) {
    UnixStream::pair().expect("UnixStream::pair failed")
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
    assert_eq!(target_as_str(&frame), target);
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
async fn target_as_str_trims_null_padding() {
    let mut frame = Frame {
        magic: 0x5652,
        flags: 0,
        length: 0,
        target: [0u8; 32],
        crc32: 0,
        payload: vec![],
    };
    frame.target[..6].copy_from_slice(b"kernel");
    assert_eq!(target_as_str(&frame), "kernel");
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
