use std::fs;
use std::path::Path;

use tempfile::tempdir;

use veyron::marketplace::installer::{skip_reinstall, uninstall};
use veyron::marketplace::state::{
    format_ts, load_state, record_install, remove_record, save_state, InstalledEntry,
    InstalledState,
};

fn entry(slug: &str, version: &str) -> InstalledEntry {
    InstalledEntry {
        slug: slug.into(),
        version: version.into(),
        sha256: "abc123".into(),
        installed_at: 1_700_000_000,
        source_url: "https://registry.example".into(),
    }
}

// state_dir honours VEYRON_STATE_DIR — every test runs inside it.
fn with_state_dir(f: impl FnOnce(&Path)) {
    let tmp = tempdir().unwrap();
    temp_env::with_var(
        "VEYRON_STATE_DIR",
        Some(tmp.path().to_str().unwrap()),
        || f(tmp.path()),
    );
}

#[test]
fn load_state_missing_file_is_empty() {
    with_state_dir(|_| {
        let state = load_state(Path::new("/nonexistent-tmp"));
        assert!(state.entries.is_empty());
    });
}

#[test]
fn record_then_load_roundtrips_entry() {
    with_state_dir(|_| {
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();
        let state = load_state(Path::new("/nonexistent-tmp"));
        let got = state.get("ping-pong").unwrap();
        assert_eq!(got.version, "0.1.0");
        assert_eq!(got.sha256, "abc123");
        assert_eq!(got.source_url, "https://registry.example");
        assert_eq!(got.installed_at, 1_700_000_000);
    });
}

#[test]
fn record_upserts_by_slug() {
    with_state_dir(|_| {
        record_install(Path::new("/nonexistent-tmp"), entry("network", "0.1.0")).unwrap();
        record_install(Path::new("/nonexistent-tmp"), entry("network", "0.2.0")).unwrap();
        let state = load_state(Path::new("/nonexistent-tmp"));
        assert_eq!(state.entries.len(), 1, "one record per slug");
        assert_eq!(state.get("network").unwrap().version, "0.2.0");
    });
}

#[test]
fn remove_record_deletes_entry() {
    with_state_dir(|_| {
        record_install(Path::new("/nonexistent-tmp"), entry("ai", "1.0.0")).unwrap();
        let removed = remove_record(Path::new("/nonexistent-tmp"), "ai").unwrap();
        assert_eq!(removed.unwrap().version, "1.0.0");
        assert!(load_state(Path::new("/nonexistent-tmp"))
            .get("ai")
            .is_none());
    });
}

#[test]
fn remove_untracked_slug_returns_none_without_writing() {
    with_state_dir(|dir| {
        let removed = remove_record(Path::new("/nonexistent-tmp"), "ghost").unwrap();
        assert!(removed.is_none());
        assert!(
            !dir.join("installed.json").exists(),
            "no-op remove writes nothing"
        );
    });
}

#[test]
fn save_state_writes_pretty_json() {
    with_state_dir(|dir| {
        let mut state = InstalledState::default();
        state.entries.push(entry("db", "0.1.0"));
        save_state(Path::new("/nonexistent-tmp"), &state).unwrap();
        let raw = fs::read_to_string(dir.join("installed.json")).unwrap();
        assert!(raw.contains("  \"slug\""), "expected pretty JSON:\n{raw}");
        let parsed: InstalledState = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.get("db").unwrap().slug, "db");
    });
}

#[test]
fn corrupt_state_file_loads_empty() {
    with_state_dir(|dir| {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("installed.json"), "{ not json").unwrap();
        assert!(load_state(Path::new("/nonexistent-tmp")).entries.is_empty());
    });
}

#[test]
fn format_ts_is_utc_civil_time() {
    assert_eq!(format_ts(1_700_000_000), "2023-11-14 22:13:20");
    assert_eq!(format_ts(0), "1970-01-01 00:00:00");
}

fn write_valid_manifest(dir: &Path, version: &str) {
    fs::write(
        dir.join("plugin.json"),
        format!(
            r#"{{
                "plugin_id": "ping-pong",
                "version": "{version}",
                "permissions": [],
                "binary": "ping-pong",
                "kernel_compatibility_range": {{"min": "0.1.0", "max": "*"}}
            }}"#
        ),
    )
    .unwrap();
}

// same version + dir present → skip (reinstall is a no-op)
#[test]
fn skip_reinstall_when_same_version_and_dir_present() {
    with_state_dir(|_| {
        let dir = tempdir().unwrap();
        write_valid_manifest(dir.path(), "0.1.0");
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();

        let skipped = skip_reinstall(
            Path::new("/nonexistent-tmp"),
            "ping-pong",
            "0.1.0",
            dir.path(),
        )
        .unwrap();
        assert_eq!(skipped.slug, "ping-pong");
        assert_eq!(skipped.version, "0.1.0");
    });
}

// same version but dir gone (half-deleted install) → reinstall to repair
#[test]
fn skip_reinstall_when_dir_missing() {
    with_state_dir(|_| {
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();
        let missing = tempdir().unwrap().path().join("gone");
        assert!(skip_reinstall(
            Path::new("/nonexistent-tmp"),
            "ping-pong",
            "0.1.0",
            &missing
        )
        .is_none());
    });
}

// different version → upgrade path, never skipped
#[test]
fn skip_reinstall_on_version_bump() {
    with_state_dir(|_| {
        let dir = tempdir().unwrap();
        write_valid_manifest(dir.path(), "0.1.0");
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();
        assert!(skip_reinstall(
            Path::new("/nonexistent-tmp"),
            "ping-pong",
            "0.2.0",
            dir.path()
        )
        .is_none());
    });
}

// untracked slug → never skipped
#[test]
fn skip_reinstall_when_untracked() {
    with_state_dir(|_| {
        let dir = tempdir().unwrap();
        write_valid_manifest(dir.path(), "0.1.0");
        assert!(
            skip_reinstall(Path::new("/nonexistent-tmp"), "fresh", "0.1.0", dir.path()).is_none()
        );
    });
}

// remove tolerates a missing dir when the state still tracks it (R10-02)
#[test]
fn uninstall_tolerates_missing_dir() {
    with_state_dir(|_| {
        let tmp = tempdir().unwrap();
        // plugin dir does not exist at all
        let plugin_dir = tmp.path().join("plugins");
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();
        temp_env::with_var(
            "VEYRON_PLUGIN_DIR",
            Some(plugin_dir.to_str().unwrap()),
            || {
                uninstall("ping-pong", Path::new("/nonexistent-tmp")).unwrap();
                assert!(
                    load_state(Path::new("/nonexistent-tmp"))
                        .get("ping-pong")
                        .is_none(),
                    "state entry dropped"
                );
            },
        );
    });
}

// remove with neither state nor dir stays a hard error
#[test]
fn uninstall_unknown_plugin_errors() {
    with_state_dir(|_| {
        let tmp = tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        temp_env::with_var(
            "VEYRON_PLUGIN_DIR",
            Some(plugin_dir.to_str().unwrap()),
            || {
                let err = uninstall("ghost", Path::new("/nonexistent-tmp")).unwrap_err();
                assert!(
                    err.to_string().contains("not installed"),
                    "unexpected: {err}"
                );
            },
        );
    });
}

// remove deletes both the dir and the state entry
#[test]
fn uninstall_removes_dir_and_state() {
    with_state_dir(|_| {
        let tmp = tempdir().unwrap();
        let plugin_dir = tmp.path().join("plugins");
        let dest = plugin_dir.join("ping-pong");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("binary"), b"x").unwrap();
        record_install(Path::new("/nonexistent-tmp"), entry("ping-pong", "0.1.0")).unwrap();

        temp_env::with_var(
            "VEYRON_PLUGIN_DIR",
            Some(plugin_dir.to_str().unwrap()),
            || {
                uninstall("ping-pong", Path::new("/nonexistent-tmp")).unwrap();
                assert!(!dest.exists(), "plugin dir removed");
                assert!(
                    load_state(Path::new("/nonexistent-tmp"))
                        .get("ping-pong")
                        .is_none(),
                    "state entry removed"
                );
            },
        );
    });
}
