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

// write_plugin_config: writes a per-plugin drop-in with binary path + id
#[test]
fn write_plugin_config_creates_dropin_file() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let installed = InstalledPlugin {
        slug: "ping-pong".into(),
        plugin_id: "ping-pong".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/ping-pong/ping-pong").into(),
    };
    assert!(write_plugin_config(&plugins_dir, &installed).unwrap());

    let path = plugins_dir.join("ping-pong.yaml");
    assert!(path.exists());
    let content = fs::read_to_string(&path).unwrap();
    assert!(content.contains("id: ping-pong"));
    assert!(content.contains("binary: /home/u/.local/lib/veyron/plugins/ping-pong/ping-pong"));
    assert!(content.contains("restart: on-failure"));
    assert!(content.contains("max_restarts: 5"));
    assert!(content.contains("sandbox: true"));
}

// write_plugin_config: network gets sandbox: false hint (egress needs a route)
#[test]
fn write_plugin_config_network_sandbox_false() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let installed = InstalledPlugin {
        slug: "network".into(),
        plugin_id: "network".into(),
        version: "0.1.5".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/network/network").into(),
    };
    write_plugin_config(&plugins_dir, &installed).unwrap();

    let content = fs::read_to_string(plugins_dir.join("network.yaml")).unwrap();
    assert!(content.contains("sandbox: false"));
}

// write_plugin_config: existing drop-in is left untouched (operator-tuned),
// and the write reports "not written"
#[test]
fn write_plugin_config_keeps_existing_file() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");
    fs::create_dir_all(&plugins_dir).unwrap();
    let path = plugins_dir.join("database.yaml");
    fs::write(&path, "id: database\nbinary: /custom/database\n").unwrap();

    let installed = InstalledPlugin {
        slug: "database".into(),
        plugin_id: "database".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/home/u/.local/lib/veyron/plugins/database/database").into(),
    };
    assert!(!write_plugin_config(&plugins_dir, &installed).unwrap());

    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "id: database\nbinary: /custom/database\n"
    );
}

// write_plugin_config: a pre-planted symlink is never followed — the write
// reports "not written" and the symlink target stays untouched (M-09 class)
#[test]
fn write_plugin_config_does_not_follow_symlink() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");
    fs::create_dir_all(&plugins_dir).unwrap();
    let victim = tmp.path().join("victim.txt");
    fs::write(&victim, "do not touch").unwrap();
    std::os::unix::fs::symlink(&victim, plugins_dir.join("pwned.yaml")).unwrap();

    let installed = InstalledPlugin {
        slug: "pwned".into(),
        plugin_id: "pwned".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/x/pwned").into(),
    };
    assert!(!write_plugin_config(&plugins_dir, &installed).unwrap());
    assert_eq!(fs::read_to_string(&victim).unwrap(), "do not touch");
}

// write_plugin_config: traversal slug is rejected, nothing written
#[test]
fn write_plugin_config_rejects_traversal_slug() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let installed = InstalledPlugin {
        slug: "../evil".into(),
        plugin_id: "evil".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/x/evil").into(),
    };
    assert!(write_plugin_config(&plugins_dir, &installed).is_err());
    assert!(!tmp.path().join("evil.yaml").exists());
}

// write_plugin_config: creates the plugins.d dir when missing
#[test]
fn write_plugin_config_creates_plugins_dir() {
    use veyron::marketplace::installer::{write_plugin_config, InstalledPlugin};

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("nested").join("plugins.d");

    let installed = InstalledPlugin {
        slug: "ai".into(),
        plugin_id: "ai".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/x/ai").into(),
    };
    write_plugin_config(&plugins_dir, &installed).unwrap();

    assert!(plugins_dir.join("ai.yaml").exists());
}

// remove_plugin_config: removes the drop-in file, returns true
#[test]
fn remove_plugin_config_removes_file() {
    use veyron::marketplace::installer::{
        remove_plugin_config, write_plugin_config, InstalledPlugin,
    };

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let installed = InstalledPlugin {
        slug: "ping-pong".into(),
        plugin_id: "ping-pong".into(),
        version: "0.1.0".into(),
        binary_path: Path::new("/x/ping-pong-rs").into(),
    };
    write_plugin_config(&plugins_dir, &installed).unwrap();

    let removed = remove_plugin_config(&plugins_dir, "ping-pong").unwrap();
    assert!(removed);
    assert!(!plugins_dir.join("ping-pong.yaml").exists());
}

// remove_plugin_config: no drop-in for the slug → false, no error
#[test]
fn remove_plugin_config_missing_is_false() {
    use veyron::marketplace::installer::remove_plugin_config;

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let removed = remove_plugin_config(&plugins_dir, "ghost").unwrap();
    assert!(!removed);
}

// remove_plugin_config: removing the middle drop-in keeps sibling files
#[test]
fn remove_plugin_config_keeps_sibling_dropins() {
    use veyron::marketplace::installer::{
        remove_plugin_config, write_plugin_config, InstalledPlugin,
    };

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");

    let mk = |slug: &str, binary: &str| InstalledPlugin {
        slug: slug.into(),
        plugin_id: slug.into(),
        version: "0.1.0".into(),
        binary_path: Path::new(binary).into(),
    };
    write_plugin_config(&plugins_dir, &mk("ping-pong", "/x/ping-pong-rs")).unwrap();
    write_plugin_config(&plugins_dir, &mk("network", "/x/network")).unwrap();
    write_plugin_config(&plugins_dir, &mk("ai", "/x/ai")).unwrap();

    let removed = remove_plugin_config(&plugins_dir, "network").unwrap();
    assert!(removed);
    assert!(!plugins_dir.join("network.yaml").exists());
    assert!(plugins_dir.join("ping-pong.yaml").exists());
    assert!(plugins_dir.join("ai.yaml").exists());
}

// remove_plugin_config: traversal slug is rejected, nothing deleted
#[test]
fn remove_plugin_config_rejects_traversal_slug() {
    use veyron::marketplace::installer::remove_plugin_config;

    let tmp = tempdir().unwrap();
    let plugins_dir = tmp.path().join("plugins.d");
    let victim = tmp.path().join("victim.yaml");
    fs::write(&victim, "keep").unwrap();

    let err = remove_plugin_config(&plugins_dir, "../victim").unwrap_err();
    assert!(err.to_string().contains("invalid slug"), "got: {err}");
    assert!(
        victim.exists(),
        "traversal must not delete outside plugins.d"
    );
}

// uninstall: traversal slug is rejected, nothing deleted
#[test]
fn uninstall_rejects_traversal_slug() {
    use veyron::marketplace::installer::uninstall;

    let tmp = tempdir().unwrap();
    let victim = tmp.path().join("victim");
    fs::create_dir_all(&victim).unwrap();

    let err = uninstall("../../victim", tmp.path()).unwrap_err();
    assert!(err.to_string().contains("invalid slug"), "got: {err}");
    assert!(
        victim.exists(),
        "traversal must not delete outside plugin dir"
    );
}
