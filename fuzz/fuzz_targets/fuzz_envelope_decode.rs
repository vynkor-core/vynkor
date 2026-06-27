#![no_main]
use libfuzzer_sys::fuzz_target;
use prost::Message;
use veyron::proto::veyron::Envelope;

/// Feed arbitrary bytes into protobuf `Envelope::decode`.
///
/// prost is memory-safe but the fuzzer may reveal unexpected allocation
/// patterns or nested-message depth explosions.
fuzz_target!(|data: &[u8]| {
    let _ = Envelope::decode(data);
});
