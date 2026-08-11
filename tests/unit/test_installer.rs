use std::fs;
use std::io::Write;
use std::path::Path;

use semver::Version;
use tempfile::tempdir;

use veyron::marketplace::installer::{extract_zip, validate_manifest};
use veyron::marketplace::registry::{check_kernel_compatibility, RegistryEntry};
use veyron::proto::veyron::PermissionType;

// prost 0.13+ derives inherent as_str_name/try_from (no Enumeration::values);
// probe codes, stopping after a run of misses so a reserved gap (7) is fine.
fn all_permission_types() -> Vec<PermissionType> {
    let mut out = Vec::new();
    let mut misses = 0;
    for i in 0i32.. {
        match PermissionType::try_from(i) {
            Ok(pt) => {
                out.push(pt);
                misses = 0;
            }
            Err(_) => {
                misses += 1;
                if misses >= 4 {
                    break;
                }
            }
        }
    }
    out
}

fn make_entry(slug: &str, min: &str, max: &str) -> RegistryEntry {
    RegistryEntry {
        id: "001".into(),
        slug: slug.into(),
        name: slug.into(),
        description: String::new(),
        version: "1.0.0".into(),
        permissions: vec!["audio_stream".into()],
        archive_url: String::new(),
        source_url: String::new(),
        sha256: String::new(),
        min_kernel_version: min.into(),
        max_kernel_version: max.into(),
        signature: String::new(),
    }
}

fn write_manifest(dir: &Path, json: &str) {
    fs::write(dir.join("plugin.json"), json).unwrap();
}

fn make_zip(dest: &Path, entries: &[(&str, &[u8])]) {
    let file = fs::File::create(dest).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
}

// Step 2: kernel below min → error
#[test]
fn compat_below_min_rejected() {
    let entry = make_entry("stt-whisper", "0.3.0", "1.0.0");
    let kernel = Version::parse("0.2.0").unwrap();
    let err = check_kernel_compatibility(&entry, &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel >= 0.3.0") && msg.contains("you are running 0.2.0"),
        "unexpected: {msg}"
    );
}

// Step 2: kernel above max → error
#[test]
fn compat_above_max_rejected() {
    let entry = make_entry("stt-whisper", "0.3.0", "1.0.0");
    let kernel = Version::parse("2.0.0").unwrap();
    let err = check_kernel_compatibility(&entry, &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel <= 1.0.0") && msg.contains("you are running 2.0.0"),
        "unexpected: {msg}"
    );
}

// Step 5: zip-slip via ".." rejected
#[test]
fn zip_slip_dotdot_rejected() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("evil.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    make_zip(&archive, &[("../../evil.txt", b"pwned")]);

    let err = extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap_err();
    assert!(err.to_string().contains("path traversal"), "{err}");
    assert!(!tmp.path().join("evil.txt").exists());
}

// Step 5: zip-slip via absolute path rejected
#[test]
fn zip_slip_absolute_path_rejected() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("evil_abs.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    make_zip(&archive, &[("/etc/evil.txt", b"pwned")]);

    let err = extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap_err();
    assert!(err.to_string().contains("path traversal"), "{err}");
}

// Step 5: clean zip extracts correctly
#[test]
fn clean_zip_extracts() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("plugin.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    make_zip(
        &archive,
        &[
            ("plugin.json", b"{\"plugin_id\":\"foo\"}"),
            ("bin/foo", b"ELF"),
        ],
    );

    extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap();
    assert!(dest.join("plugin.json").exists());
    assert!(dest.join("bin/foo").exists());
}

// QA finding (phase 8): zip stores the binary as 0755 but extraction yielded
// 0644, so marketplace-installed plugins could never spawn. extraction must
// restore the stored unix mode.
#[cfg(unix)]
#[test]
fn extraction_restores_exec_bit() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("exec.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    let file = fs::File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    zip.start_file("database", options).unwrap();
    zip.write_all(b"ELF").unwrap();
    zip.finish().unwrap();

    extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap();

    let mode = fs::metadata(dest.join("database"))
        .unwrap()
        .permissions()
        .mode();
    assert!(mode & 0o111 != 0, "exec bit lost: {mode:o}");
}

// AUDIT M-06: archive with more entries than the cap must be rejected
// before any extraction happens.
#[test]
fn archive_with_excess_entries_rejected() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("many.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    let names: Vec<String> = (0..10_001).map(|i| format!("f{i}")).collect();
    let entries: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"x".as_ref())).collect();
    make_zip(&archive, &entries);

    let err = extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap_err();
    assert!(err.to_string().contains("exceeds max"), "{err}");
    assert!(!dest.join("f0").exists(), "no entry should be extracted");
}

// AUDIT M-06: a zip-bomb-style entry (highly compressible, decompresses far
// past the cap) must be aborted mid-copy — the cap is enforced on actual
// bytes written, not the archive's declared/compressed size.
#[test]
fn zip_bomb_decompressed_size_capped() {
    let tmp = tempdir().unwrap();
    let archive = tmp.path().join("bomb.zip");
    let dest = tmp.path().join("extracted");
    fs::create_dir_all(&dest).unwrap();

    let file = fs::File::create(&archive).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    // Stored (uncompressed): what matters here is that extraction enforces
    // the cap on bytes actually written, regardless of compression method —
    // Stored keeps the test's own I/O cost down (no deflate to run).
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("bomb.bin", options).unwrap();
    // Just over the 1 GiB cap — extraction must stop the instant the cap is
    // crossed, not after buffering the whole entry.
    let chunk = vec![0u8; 1024 * 1024];
    for _ in 0..1026 {
        zip.write_all(&chunk).unwrap();
    }
    zip.finish().unwrap();

    let err = extract_zip(&archive, &dest, 1024 * 1024 * 1024, 10_000).unwrap_err();
    assert!(
        err.to_string().contains("decompressed size exceeds max"),
        "{err}"
    );
}

// Step 7: missing kernel_compatibility_range → validation error
#[test]
fn manifest_missing_compat_range_errors() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{"plugin_id":"foo","version":"1.0.0","permissions":[],"binary":"foo"}"#,
    );
    let kernel = Version::parse("0.1.0").unwrap();
    let err = validate_manifest(&tmp.path().join("plugin.json"), &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Invalid plugin.json"),
        "expected parse error, got: {msg}"
    );
}

// Step 7: unknown permission → validation error
#[test]
fn manifest_unknown_permission_errors() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "foo",
            "version": "1.0.0",
            "permissions": ["teleport"],
            "binary": "foo",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"}
        }"#,
    );
    let kernel = Version::parse("0.1.0").unwrap();
    let err = validate_manifest(&tmp.path().join("plugin.json"), &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unknown permission") && msg.contains("teleport"),
        "unexpected: {msg}"
    );
}

// R8-02: every proto permission (both forms) must pass — drift guard for future enums
#[test]
fn manifest_accepts_every_proto_permission() {
    let kernel = Version::parse("0.1.0").unwrap();
    for pt in all_permission_types() {
        let proto_name = pt.as_str_name();
        if proto_name == "PERMISSION_UNKNOWN" {
            continue;
        }
        let lower = proto_name
            .strip_prefix("PERMISSION_")
            .unwrap_or(proto_name)
            .to_ascii_lowercase();
        for perm in [proto_name, lower.as_str()] {
            let tmp = tempdir().unwrap();
            write_manifest(
                tmp.path(),
                &format!(
                    r#"{{
                        "plugin_id": "perm-check",
                        "version": "1.0.0",
                        "permissions": ["{perm}"],
                        "binary": "perm-check",
                        "kernel_compatibility_range": {{"min": "0.1.0", "max": "*"}}
                    }}"#
                ),
            );
            let res = validate_manifest(&tmp.path().join("plugin.json"), &kernel);
            assert!(
                res.is_ok(),
                "permission {perm} (proto {proto_name}) should be accepted, got: {res:?}"
            );
        }
    }
}

// Step 7: kernel incompatible in plugin.json → validation error
#[test]
fn manifest_kernel_compat_enforced() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "foo",
            "version": "1.0.0",
            "permissions": [],
            "binary": "foo",
            "kernel_compatibility_range": {"min": "99.0.0", "max": "*"}
        }"#,
    );
    let kernel = Version::parse("0.1.0").unwrap();
    let err = validate_manifest(&tmp.path().join("plugin.json"), &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel >= 99.0.0"),
        "unexpected: {msg}"
    );
}

// Step 7: valid manifest → Ok
#[test]
fn manifest_valid_ok() {
    let tmp = tempdir().unwrap();
    write_manifest(
        tmp.path(),
        r#"{
            "plugin_id": "stt-whisper",
            "version": "1.2.0",
            "permissions": ["audio_stream", "network"],
            "binary": "stt-whisper",
            "kernel_compatibility_range": {"min": "0.1.0", "max": "*"},
            "events": ["system.ready"],
            "actions": ["transcribe_audio"]
        }"#,
    );
    let kernel = Version::parse("0.1.0").unwrap();
    let manifest = validate_manifest(&tmp.path().join("plugin.json"), &kernel).unwrap();
    assert_eq!(manifest.plugin_id, "stt-whisper");
    assert_eq!(manifest.version, "1.2.0");
}

// Step 7: plugin.json not found → error
#[test]
fn manifest_not_found_errors() {
    let tmp = tempdir().unwrap();
    let kernel = Version::parse("0.1.0").unwrap();
    let err = validate_manifest(&tmp.path().join("plugin.json"), &kernel).unwrap_err();
    assert!(err.to_string().contains("Invalid plugin.json"));
}

// append_config_example: appends a commented block with binary path + marker
#[test]
fn append_config_example_adds_commented_block() {
    use veyron::marketplace::installer::{append_config_example, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();

    let installed = InstalledPlugin {
        slug: "ping-pong".into(),
        plugin_id: "ping-pong".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/ping-pong/ping-pong").into(),
    };
    append_config_example(&cfg.display().to_string(), &installed).unwrap();

    let content = fs::read_to_string(&cfg).unwrap();
    assert!(content.contains("# veyron install: ping-pong v0.1.0"));
    assert!(content.contains("#   - id: ping-pong"));
    assert!(content.contains("binary: /home/u/.local/lib/veyron/plugins/ping-pong/ping-pong"));
    assert!(content.contains("#     sandbox: true"));
}

// append_config_example: network gets sandbox: false hint (egress needs a route)
#[test]
fn append_config_example_network_sandbox_false() {
    use veyron::marketplace::installer::{append_config_example, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();

    let installed = InstalledPlugin {
        slug: "network".into(),
        plugin_id: "network".into(),
        version: "0.1.5".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/network/network").into(),
    };
    append_config_example(&cfg.display().to_string(), &installed).unwrap();

    let content = fs::read_to_string(&cfg).unwrap();
    assert!(content.contains("#     sandbox: false"));
}

// append_config_example: second call for the same slug is a no-op (idempotent)
#[test]
fn append_config_example_is_idempotent() {
    use veyron::marketplace::installer::{append_config_example, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();

    let installed = InstalledPlugin {
        slug: "database".into(),
        plugin_id: "database".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/database/database").into(),
    };
    let path = cfg.display().to_string();
    append_config_example(&path, &installed).unwrap();
    let once = fs::read_to_string(&cfg).unwrap();
    append_config_example(&path, &installed).unwrap();
    let twice = fs::read_to_string(&cfg).unwrap();

    assert_eq!(once, twice);
    assert_eq!(once.matches("# veyron install: database").count(), 1);
}

// append_config_example: missing config file is a silent no-op, not an error
#[test]
fn append_config_example_missing_config_is_noop() {
    use veyron::marketplace::installer::{append_config_example, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.yaml");

    let installed = InstalledPlugin {
        slug: "network".into(),
        plugin_id: "network".into(),
        version: "0.1.5".into(),
        binary_path: Path::new("/x/network").into(),
    };
    assert!(append_config_example(&missing.display().to_string(), &installed).is_ok());
    assert!(!missing.exists());
}

// remove_config_example: fully-commented block is removed, config content intact
#[test]
fn remove_config_example_removes_commented_block() {
    use veyron::marketplace::installer::{
        append_config_example, remove_config_example, ConfigExampleStatus, InstalledPlugin,
    };

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();
    let path = cfg.display().to_string();

    let installed = InstalledPlugin {
        slug: "ping-pong".into(),
        plugin_id: "ping-pong".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/ping-pong/ping-pong-rs").into(),
    };
    append_config_example(&path, &installed).unwrap();

    let status = remove_config_example(&path, "ping-pong").unwrap();
    assert_eq!(status, ConfigExampleStatus::Removed);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), "port: 8888\n");
}

// remove_config_example: an uncommented (live) entry is never deleted
#[test]
fn remove_config_example_leaves_active_block() {
    use veyron::marketplace::installer::{remove_config_example, ConfigExampleStatus};

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    // operator uncommented the `- id:` line — the entry is live
    fs::write(
        &cfg,
        "port: 8888\n\n\
         # --- installed via `vyn plugin install network` ---\n\
         # veyron install: network v0.1.5\n\
         - id: network\n\
         \x20 binary: /x/network\n",
    )
    .unwrap();
    let path = cfg.display().to_string();
    let before = fs::read_to_string(&cfg).unwrap();

    let status = remove_config_example(&path, "network").unwrap();
    assert_eq!(status, ConfigExampleStatus::Active);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);
}

// remove_config_example: no marker for the slug → NotFound, file untouched
#[test]
fn remove_config_example_not_found_is_noop() {
    use veyron::marketplace::installer::{remove_config_example, ConfigExampleStatus};

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();
    let path = cfg.display().to_string();
    let before = fs::read_to_string(&cfg).unwrap();

    let status = remove_config_example(&path, "ghost").unwrap();
    assert_eq!(status, ConfigExampleStatus::NotFound);
    assert_eq!(fs::read_to_string(&cfg).unwrap(), before);
}

// remove_config_example: missing config file → NotFound, not an error
#[test]
fn remove_config_example_missing_config_is_not_found() {
    use veyron::marketplace::installer::{remove_config_example, ConfigExampleStatus};

    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("does-not-exist.yaml");

    let status = remove_config_example(&missing.display().to_string(), "network").unwrap();
    assert_eq!(status, ConfigExampleStatus::NotFound);
}

// remove_config_example: removing the middle block keeps sibling blocks
#[test]
fn remove_config_example_keeps_sibling_blocks() {
    use veyron::marketplace::installer::{
        append_config_example, remove_config_example, ConfigExampleStatus, InstalledPlugin,
    };

    let tmp = tempdir().unwrap();
    let cfg = tmp.path().join("config.yaml");
    fs::write(&cfg, "port: 8888\n").unwrap();
    let path = cfg.display().to_string();

    let mk = |slug: &str, binary: &str| InstalledPlugin {
        slug: slug.into(),
        plugin_id: slug.into(),
        version: "0.1.0".into(),
        binary_path: Path::new(binary).into(),
    };
    append_config_example(&path, &mk("ping-pong", "/x/ping-pong-rs")).unwrap();
    append_config_example(&path, &mk("network", "/x/network")).unwrap();
    append_config_example(&path, &mk("ai", "/x/ai")).unwrap();

    let status = remove_config_example(&path, "network").unwrap();
    assert_eq!(status, ConfigExampleStatus::Removed);

    let content = fs::read_to_string(&cfg).unwrap();
    assert!(!content.contains("veyron install: network"));
    assert!(content.contains("veyron install: ping-pong"));
    assert!(content.contains("veyron install: ai"));
}
