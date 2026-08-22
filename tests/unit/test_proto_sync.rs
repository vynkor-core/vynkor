use std::fs;

// R8-05: the vendored proto copies must stay byte-identical to the wire source
// of truth. `../vynkor-wire/proto/vynkor_protocol.proto` (sibling repo
// vynkor-core/vynkor-wire) is the canonical message schema for plugin<->kernel
// IPC; ../vynkor-sdk-python/, ../vynkor-sdk-cpp/ each vendor a copy so their
// build.rs can generate bindings offline. Drift here means the SDKs speak a
// different protocol than the kernel — wire it into the test suite so a one-off
// edit to a single copy fails loudly.
#[test]
fn vendored_proto_copies_are_byte_identical() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let copies = [
        "../vynkor-wire/proto/vynkor_protocol.proto",
        "../vynkor-sdk-python/proto/vynkor_protocol.proto",
        "../vynkor-sdk-cpp/proto/vynkor_protocol.proto",
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

// R8-05 follow-up: the generated Python binding (../vynkor-sdk-python/vynkor/
// vynkor_protocol_pb2.py) must reflect the same wire schema. It is produced by
// ../vynkor-sdk-python/scripts/gen_proto_python.py from
// ../vynkor-wire/proto/vynkor_protocol.proto and is committed, but nothing
// guarded it against going stale when the proto grew (PERMISSION_STORAGE,
// ActionRequest.caller_plugin_id were missing). Marker check: when the proto
// adds a symbol, the regeneration step must run too.
#[test]
fn generated_python_binding_is_not_stale() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let pb2_path = repo_root.join("../vynkor-sdk-python/vynkor/vynkor_protocol_pb2.py");
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
        // v1.6 (D-01) additions — device identity, versioning, user_id, tool
        // schema. Field/message names whose leading bytes protoc escapes are
        // matched via stable tails; the rest are matched verbatim.
        "ActionSpec",
        "DeviceInfo",
        "device_id",
        "protocol_version",
        "user_id",
        "platforms",
        "os_version",
        "requires_confirmation",
        "params_schema",
    ] {
        assert!(
            source.contains(marker),
            "generated {pb2_path:?} is missing {marker}; run ../vynkor-sdk-python/scripts/gen_proto_python.py"
        );
    }

    // ActionRequest.caller_plugin_id (proto v1.3, field 6). protoc escapes the
    // first two bytes of the field name in the descriptor blob, so match the
    // stable tail.
    assert!(
        source.contains("ller_plugin_id"),
        "generated {pb2_path:?} is missing ActionRequest.caller_plugin_id; \
         run ../vynkor-sdk-python/scripts/gen_proto_python.py"
    );

    // v1.6 field names with escaped leading bytes — match stable tails (see
    // caller_plugin_id above).
    for marker in ["tion_specs", "pabilities"] {
        assert!(
            source.contains(marker),
            "generated {pb2_path:?} is missing v1.6 field {marker}; \
             run ../vynkor-sdk-python/scripts/gen_proto_python.py"
        );
    }
}

// D-01: the proto header comment (`// v 1.x`) and vynkor_wire::PROTOCOL_VERSION
// must agree — the wire README mandates bumping both in the same commit. Both
// live in sibling ../vynkor-wire; guard the pairing here so a one-sided bump
// fails loudly.
#[test]
fn proto_header_matches_wire_protocol_version() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let proto = fs::read_to_string(repo_root.join("../vynkor-wire/proto/vynkor_protocol.proto"))
        .unwrap_or_else(|e| panic!("failed to read wire proto: {e}"));
    let lib = fs::read_to_string(repo_root.join("../vynkor-wire/src/lib.rs"))
        .unwrap_or_else(|e| panic!("failed to read wire lib.rs: {e}"));

    let header = proto
        .lines()
        .find(|l| l.trim_start().starts_with("// v "))
        .and_then(|l| l.split_whitespace().nth(2))
        .unwrap_or_else(|| panic!("no `// v x.y` header in wire proto"));
    let const_ver = lib
        .lines()
        .find(|l| l.contains("PROTOCOL_VERSION"))
        .and_then(|l| l.split('"').nth(1))
        .unwrap_or_else(|| panic!("no PROTOCOL_VERSION const in wire lib.rs"));

    assert_eq!(
        header, const_ver,
        "wire proto header v{header} != vynkor_wire::PROTOCOL_VERSION {const_ver}; \
         bump both in the same commit (D-01)"
    );
}
