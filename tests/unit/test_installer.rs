use std::fs;
use std::io::Write;
use std::path::Path;

use semver::Version;
use tempfile::tempdir;

use veyron::marketplace::installer::{extract_zip, validate_manifest};
use veyron::marketplace::registry::{check_kernel_compatibility, RegistryEntry};

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

    let err = extract_zip(&archive, &dest).unwrap_err();
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

    let err = extract_zip(&archive, &dest).unwrap_err();
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

    extract_zip(&archive, &dest).unwrap();
    assert!(dest.join("plugin.json").exists());
    assert!(dest.join("bin/foo").exists());
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

    let err = extract_zip(&archive, &dest).unwrap_err();
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

    let err = extract_zip(&archive, &dest).unwrap_err();
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
