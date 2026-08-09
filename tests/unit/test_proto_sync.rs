use std::fs;

// R8-05: the vendored proto copies must stay byte-identical to the wire source
// of truth. `proto/veyron_protocol.proto` is the canonical message schema for
// plugin<->kernel IPC; wire/, sdk/python/, sdk/cpp/ each vendor a copy so their
// build.rs can generate bindings offline. Drift here means the SDKs speak a
// different protocol than the kernel — wire it into the test suite so a one-off
// edit to a single copy fails loudly.
#[test]
fn vendored_proto_copies_are_byte_identical() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let copies = [
        "wire/proto/veyron_protocol.proto",
        "sdk/python/proto/veyron_protocol.proto",
        "sdk/cpp/proto/veyron_protocol.proto",
    ];

    let contents: Vec<(String, Vec<u8>)> = copies
        .iter()
        .map(|p| {
            let bytes = fs::read(repo_root.join(p))
                .unwrap_or_else(|e| panic!("failed to read vendored proto {p}: {e}"));
            (p.to_string(), bytes)
        })
        .collect();

    let (first_path, first) = &contents[0];
    for (path, bytes) in &contents[1..] {
        assert_eq!(
            first, bytes,
            "vendored proto {path} drifted from {first_path}; run the proto-sync step to re-vendor"
        );
    }
}
