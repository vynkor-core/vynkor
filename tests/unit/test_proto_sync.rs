use std::fs;

// R8-05: the vendored proto copies must stay byte-identical to the wire source
// of truth. `../veyron-wire/proto/veyron_protocol.proto` (sibling repo
// veyron-core/veyron-wire) is the canonical message schema for plugin<->kernel
// IPC; ../veyron-sdk-python/, ../veyron-sdk-cpp/ each vendor a copy so their
// build.rs can generate bindings offline. Drift here means the SDKs speak a
// different protocol than the kernel — wire it into the test suite so a one-off
// edit to a single copy fails loudly.
#[test]
fn vendored_proto_copies_are_byte_identical() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let copies = [
        "../veyron-wire/proto/veyron_protocol.proto",
        "../veyron-sdk-python/proto/veyron_protocol.proto",
        "../veyron-sdk-cpp/proto/veyron_protocol.proto",
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

// R8-05 follow-up: the generated Python binding (../veyron-sdk-python/veyron/
// veyron_protocol_pb2.py) must reflect the same wire schema. It is produced by
// scripts/gen_proto_python.py from ../veyron-wire/proto/veyron_protocol.proto
// and is committed, but nothing guarded it against going stale when the proto
// grew (PERMISSION_STORAGE, ActionRequest.caller_plugin_id were missing). Marker
// check: when the proto adds a symbol, the regeneration step must run too.
#[test]
fn generated_python_binding_is_not_stale() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pb2_path = repo_root.join("../veyron-sdk-python/veyron/veyron_protocol_pb2.py");
    let source = fs::read_to_string(&pb2_path).unwrap_or_else(|e| {
        panic!(
            "failed to read generated binding {}: {e}",
            pb2_path.display()
        )
    });

    // PERMISSION_STORAGE=14 and PERMISSION_EVENT_PUBLISH=13 must be present in
    // the enum; the proto stores them verbatim in the serialized descriptor.
    for marker in [
        "PERMISSION_EVENT_PUBLISH",
        "PERMISSION_STORAGE",
        // v1.4 additions — the five new PermissionType values for the planned
        // secrets/clipboard/launcher/screenshot/home plugins.
        "PERMISSION_SECRETS",
        "PERMISSION_CLIPBOARD",
        "PERMISSION_LAUNCH",
        "PERMISSION_SCREEN",
        "PERMISSION_HOME",
    ] {
        assert!(
            source.contains(marker),
            "generated {pb2_path:?} is missing {marker}; run scripts/gen_proto_python.py"
        );
    }

    // ActionRequest.caller_plugin_id (proto v1.3, field 6). protoc escapes the
    // first two bytes of the field name in the descriptor blob, so match the
    // stable tail.
    assert!(
        source.contains("ller_plugin_id"),
        "generated {pb2_path:?} is missing ActionRequest.caller_plugin_id; \
         run scripts/gen_proto_python.py"
    );
}
