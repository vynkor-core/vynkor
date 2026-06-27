#![no_main]
use libfuzzer_sys::fuzz_target;
use tokio::io::BufReader;
use veyron::ipc::framing::read_frame;

/// Feed arbitrary bytes into the frame parser.
///
/// The parser must never panic — it may return Ok(frame) or Err(_), both fine.
/// OOM guard: MAX_PAYLOAD_SIZE (1 MiB) is already enforced in the parser.
fuzz_target!(|data: &[u8]| {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    rt.block_on(async {
        let mut reader = BufReader::new(std::io::Cursor::new(data));
        // Result is intentionally discarded — we only care about no panics.
        let _ = read_frame(&mut reader).await;
    });
});
