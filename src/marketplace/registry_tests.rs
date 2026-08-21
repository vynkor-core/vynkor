use super::*;
use crate::marketplace::state::{InstalledEntry, InstalledState};
use semver::Version;

fn make_entry(min: &str, max: &str) -> RegistryEntry {
    RegistryEntry {
        id: "001".into(),
        slug: "stt-whisper".into(),
        name: "Whisper STT".into(),
        description: "Speech-to-text".into(),
        version: "1.0.0".into(),
        permissions: vec!["audio_stream".into()],
        archive_url: String::new(),
        source_url: String::new(),
        sha256: String::new(),
        min_kernel_version: min.into(),
        max_kernel_version: max.into(),
        signature: String::new(),
        status: "stable".into(),
    }
}

// A `make_entry` whose signature verifies under TEST_PUB_HEX. Each fixed
// test vector signs the full canonical message
// `slug:version:sha256:status:archive_url:min:max` (S1) — see the
// constants at the bottom of this module.
fn signed_entry(status: &str) -> RegistryEntry {
    let mut entry = make_entry("0.1.0", "*");
    entry.sha256 = "deadbeef".into();
    entry.status = status.into();
    entry.signature = match status {
        "stable" => TEST_SIG_STABLE_HEX.into(),
        "revoked" => TEST_SIG_REVOKED_HEX.into(),
        // other statuses are only ever checked for is_revoked(), never
        // signature-verified — any non-empty value is fine
        _ => TEST_SIG_STABLE_HEX.into(),
    };
    entry
}

#[test]
fn compat_ok() {
    let entry = make_entry("0.3.0", "1.0.0");
    let kernel = Version::parse("0.5.0").unwrap();
    assert!(check_kernel_compatibility(&entry, &kernel).is_ok());
}

#[test]
fn compat_below_min() {
    let entry = make_entry("0.3.0", "1.0.0");
    let kernel = Version::parse("0.2.0").unwrap();
    let err = check_kernel_compatibility(&entry, &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel >= 0.3.0") && msg.contains("you are running 0.2.0"),
        "unexpected: {msg}"
    );
}

#[test]
fn compat_above_max() {
    let entry = make_entry("0.3.0", "1.0.0");
    let kernel = Version::parse("2.0.0").unwrap();
    let err = check_kernel_compatibility(&entry, &kernel).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("requires Veyron kernel <= 1.0.0") && msg.contains("you are running 2.0.0"),
        "unexpected: {msg}"
    );
}

#[test]
fn compat_star_max() {
    let entry = make_entry("0.1.0", "*");
    let kernel = Version::parse("99.0.0").unwrap();
    assert!(check_kernel_compatibility(&entry, &kernel).is_ok());
}

#[test]
fn entry_is_revoked_only_for_revoked_status() {
    assert!(signed_entry("revoked").is_revoked());
    for status in ["stable", "beta", "deprecated", "hidden", ""] {
        assert!(!signed_entry(status).is_revoked(), "status {status:?}");
    }
}

#[test]
fn cache_path_lives_in_state_dir() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        cache_path(tmp.path()),
        state_dir(tmp.path()).join("registry-cache.json")
    );
}

#[test]
fn cache_without_schema_version_is_treated_as_empty() {
    // an unversioned file is ambiguous — reject it rather than misread it
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry-cache.json");
    fs::write(&cache, r#"{"last_check": 123, "entries": []}"#).unwrap();
    assert!(read_cache_file(&cache).is_none());
}

#[tokio::test]
async fn fetch_writes_versioned_cache_and_reads_it() {
    let mut server = mockito::Server::new_async().await;
    let body = serde_json::to_string(&[signed_entry("stable")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry-cache.json");
    let url = format!("{}/registry.json", server.url());

    // First call: fetches from network, verifies, writes the cache.
    let result = fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].slug, "stt-whisper");
    assert!(cache.exists(), "cache file should be written");

    let parsed = read_cache_file(&cache).expect("cache should parse");
    assert_eq!(parsed.schema_version, REGISTRY_CACHE_SCHEMA_VERSION);
    assert!(parsed.last_check > 0, "last_check should be recorded");
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(parsed.entries[0].status, "stable");

    mock.assert_async().await;

    // Second call within TTL: reads from disk (mock not called again).
    let cached = fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert_eq!(cached[0].slug, "stt-whisper");
}

#[tokio::test]
async fn refresh_bypasses_ttl() {
    let mut server = mockito::Server::new_async().await;
    let body = serde_json::to_string(&[signed_entry("stable")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(2)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    // refresh = true forces network even though cache is fresh
    fetch_registry_from(
        &url,
        true,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();

    mock.assert_async().await;
}

#[tokio::test]
async fn stale_verified_cache_used_on_network_failure() {
    let mut server = mockito::Server::new_async().await;
    let body = serde_json::to_string(&[signed_entry("stable")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    // populate the cache from a trusted fetch
    fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    mock.assert_async().await;

    // Use a URL that will definitely fail — TTL=0 makes the cache stale.
    let result = fetch_registry_from(
        "http://127.0.0.1:1",
        false,
        &cache,
        0,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await;
    assert!(result.is_ok(), "should fall back to stale verified cache");
    assert_eq!(result.unwrap()[0].slug, "stt-whisper");
}

#[tokio::test]
async fn no_cache_network_failure_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");

    let result = fetch_registry_from(
        "http://127.0.0.1:1",
        false,
        &cache,
        3600,
        None,
        &InstalledState::default(),
    )
    .await;
    assert!(
        result.is_err(),
        "should error when no cache and network fails"
    );
}

#[tokio::test]
async fn unverified_entries_are_not_cached() {
    let mut server = mockito::Server::new_async().await;
    // unsigned entry — verify_entries drops it
    let body = serde_json::to_string(&[make_entry("0.1.0", "*")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    // live fetch still returns the entry (list/search show the live doc)
    let result = fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.len(), 1);
    mock.assert_async().await;

    // but nothing was cached — a later offline fetch has nothing to serve
    assert!(!cache.exists(), "unverified entries must not be cached");
    let result = fetch_registry_from(
        "http://127.0.0.1:1",
        false,
        &cache,
        0,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await;
    assert!(result.is_err(), "no verified snapshot to fall back on");
}

#[tokio::test]
async fn all_unverified_refetch_keeps_previous_cache() {
    let mut trusted = mockito::Server::new_async().await;
    let trusted_body = serde_json::to_string(&[signed_entry("stable")]).unwrap();
    let trusted_mock = trusted
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&trusted_body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let trusted_url = format!("{}/registry.json", trusted.url());
    fetch_registry_from(
        &trusted_url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    trusted_mock.assert_async().await;

    // an all-unverified refetch (compromised channel / wrong key) must not
    // clobber the good snapshot
    let mut evil = mockito::Server::new_async().await;
    let evil_body = serde_json::to_string(&[make_entry("0.1.0", "*")]).unwrap();
    let evil_mock = evil
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&evil_body)
        .expect(1)
        .create_async()
        .await;
    let evil_url = format!("{}/registry.json", evil.url());
    fetch_registry_from(
        &evil_url,
        true,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    evil_mock.assert_async().await;

    // stale fallback still serves the ORIGINAL verified entry
    let result = fetch_registry_from(
        "http://127.0.0.1:1",
        false,
        &cache,
        0,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert_eq!(result[0].slug, "stt-whisper");
    assert_eq!(result[0].signature, TEST_SIG_STABLE_HEX);
}

#[tokio::test]
async fn revoked_entries_stay_cached_and_survive_expiry() {
    let mut server = mockito::Server::new_async().await;
    // revoked but signed — the cache keeps it (revocation outlives TTL)
    let body = serde_json::to_string(&[signed_entry("revoked")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    mock.assert_async().await;

    // cache holds the revoked entry even after the TTL expired
    let parsed = read_cache_file(&cache).unwrap();
    assert_eq!(parsed.entries.len(), 1);
    assert!(parsed.entries[0].is_revoked());

    // stale fallback still surfaces it — install must refuse it
    let result = fetch_registry_from(
        "http://127.0.0.1:1",
        false,
        &cache,
        0,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert!(result[0].is_revoked());
}

#[tokio::test]
async fn foreign_schema_version_is_treated_as_empty() {
    let mut server = mockito::Server::new_async().await;
    let body = serde_json::to_string(&[signed_entry("stable")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    // plant a cache from a hypothetical future kernel
    write_cache_file(
        &cache,
        &RegistryCache {
            schema_version: REGISTRY_CACHE_SCHEMA_VERSION + 1,
            ..RegistryCache::default()
        },
    )
    .unwrap();

    // fresh TTL, but the schema is foreign → must refetch
    let url = format!("{}/registry.json", server.url());
    fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    mock.assert_async().await;
}

#[tokio::test]
async fn cache_snapshots_installed_versions_per_plugin() {
    let mut server = mockito::Server::new_async().await;
    let body = serde_json::to_string(&[signed_entry("stable")]).unwrap();

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    let mut installed = InstalledState::default();
    installed.entries.push(InstalledEntry {
        slug: "stt-whisper".into(),
        version: "0.9.0".into(),
        sha256: "abc".into(),
        installed_at: 1_700_000_000,
        source_url: "https://registry.example".into(),
    });

    fetch_registry_from(&url, false, &cache, 3600, Some(TEST_PUB_HEX), &installed)
        .await
        .unwrap();
    mock.assert_async().await;

    let parsed = read_cache_file(&cache).unwrap();
    let info = parsed
        .plugins
        .get("stt-whisper")
        .expect("per-plugin info recorded");
    assert_eq!(info.installed_version.as_deref(), Some("0.9.0"));
    assert!(info.last_check > 0);
    assert_eq!(parsed.plugins.len(), 1);
}

#[test]
fn v2_map_form_parses_flattens_and_folds_revoked_list() {
    let doc = r#"{
      "meta": { "apiVersion": 2, "lastUpdated": "2026-08-13" },
      "revoked": ["evil@1.0.0"],
      "ai": {
        "id": "001",
        "name": "AI",
        "description": "chat",
        "permissions": [],
        "status": "beta",
        "source_url": "https://example.com/ai-src.zip",
        "versions": {
          "0.1.0": {
            "archive_url": "https://example.com/ai.zip",
            "sha256": "deadbeef",
            "signature": "abcd",
            "min_kernel_version": "0.1.0",
            "max_kernel_version": "*"
          }
        }
      },
      "evil": {
        "name": "Evil",
        "versions": {
          "1.0.0": {
            "archive_url": "https://example.com/evil.zip",
            "sha256": "cafebabe",
            "min_kernel_version": "0.0.1",
            "max_kernel_version": "*"
          }
        }
      }
    }"#;

    let parsed = parse_registry_document(doc).unwrap();
    assert_eq!(parsed.entries.len(), 2);

    let ai = parsed.entries.iter().find(|e| e.slug == "ai").unwrap();
    assert_eq!(ai.id, "001");
    assert_eq!(ai.version, "0.1.0");
    assert_eq!(ai.status, "beta");
    assert_eq!(ai.archive_url, "https://example.com/ai.zip");
    assert_eq!(ai.max_kernel_version, "*");
    assert!(!ai.is_revoked());

    let evil = parsed.entries.iter().find(|e| e.slug == "evil").unwrap();
    assert_eq!(evil.version, "1.0.0");
    assert_eq!(evil.id, "evil", "id falls back to the slug");
    assert!(
        evil.is_revoked(),
        "document-level revoked list folds into entry status"
    );
}

#[test]
fn v2_meta_is_carried_into_document() {
    let doc = r#"{
      "meta": { "api_version": 2, "last_updated": "2026-08-13" },
      "ai": { "name": "AI", "versions": {} }
    }"#;
    let parsed = parse_registry_document(doc).unwrap();
    let meta = parsed.meta.expect("meta parsed");
    assert_eq!(meta.api_version, Some(2));
    assert_eq!(meta.last_updated.as_deref(), Some("2026-08-13"));
    assert!(parsed.entries.is_empty(), "no versions → no entries");
}

#[test]
fn resolves_relative_archive_urls_against_base_url() {
    let mut entries = vec![RegistryEntry {
        archive_url: "dist/ai/versions/0.1.0/ai-0.1.0.zip".into(),
        ..make_entry("0.1.0", "*")
    }];
    resolve_relative_archive_urls(
        &mut entries,
        "https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json",
    );
    assert_eq!(
        entries[0].archive_url,
        "https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/dist/ai/versions/0.1.0/ai-0.1.0.zip"
    );
}

#[test]
fn resolves_relative_archive_urls_against_trailing_slash_base() {
    let mut entries = vec![RegistryEntry {
        archive_url: "ai.zip".into(),
        ..make_entry("0.1.0", "*")
    }];
    resolve_relative_archive_urls(&mut entries, "https://example.com/marketplace/");
    assert_eq!(
        entries[0].archive_url,
        "https://example.com/marketplace/ai.zip"
    );
}

#[test]
fn leaves_absolute_archive_urls_untouched() {
    let mut entries = vec![RegistryEntry {
        archive_url: "https://cdn.example.com/ai.zip".into(),
        ..make_entry("0.1.0", "*")
    }];
    resolve_relative_archive_urls(&mut entries, "https://example.com/registry.json");
    assert_eq!(entries[0].archive_url, "https://cdn.example.com/ai.zip");
}

#[test]
fn non_url_base_leaves_relative_entries_untouched() {
    let mut entries = vec![RegistryEntry {
        archive_url: "dist/ai.zip".into(),
        ..make_entry("0.1.0", "*")
    }];
    resolve_relative_archive_urls(&mut entries, "not-a-url");
    assert_eq!(entries[0].archive_url, "dist/ai.zip");
}

#[tokio::test]
async fn fetch_keeps_relative_archive_url_as_served() {
    // S1: the signature binds the as-served (relative) archive_url, so
    // fetch must not resolve it — install resolves after verification.
    let mut server = mockito::Server::new_async().await;
    let body = format!(
        r#"{{
          "meta": {{ "apiVersion": 2, "lastUpdated": "2026-08-13" }},
          "revoked": [],
          "stt-whisper": {{
            "name": "Whisper STT",
            "status": "stable",
            "versions": {{
              "1.0.0": {{
                "archive_url": "dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip",
                "sha256": "deadbeef",
                "signature": "{sig}",
                "min_kernel_version": "0.1.0",
                "max_kernel_version": "*"
              }}
            }}
          }}
        }}"#,
        sig = TEST_SIG_RELATIVE_HEX
    );

    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(&body)
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry-cache.json");
    let url = format!("{}/registry.json", server.url());

    let result = fetch_registry_from(
        &url,
        false,
        &cache,
        3600,
        Some(TEST_PUB_HEX),
        &InstalledState::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.len(), 1);
    // the relative URL survives fetch untouched — and the entry verified
    // (it made it into the cache in its raw form)
    assert_eq!(
        result[0].archive_url,
        "dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip"
    );
    let parsed = read_cache_file(&cache).unwrap();
    assert_eq!(parsed.entries.len(), 1);
    assert_eq!(
        parsed.entries[0].archive_url,
        "dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip"
    );

    mock.assert_async().await;
}

#[tokio::test]
async fn empty_response_body_gives_actionable_error() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("")
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    let err = fetch_registry_from(&url, false, &cache, 3600, None, &InstalledState::default())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("registry response body was empty"),
        "unexpected: {msg}"
    );

    mock.assert_async().await;
}

#[tokio::test]
async fn malformed_response_body_reports_parse_error() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/registry.json")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body("{ not json")
        .expect(1)
        .create_async()
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("registry.json");
    let url = format!("{}/registry.json", server.url());

    let err = fetch_registry_from(&url, false, &cache, 3600, None, &InstalledState::default())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("parse registry JSON"), "unexpected: {msg}");

    mock.assert_async().await;
}

// Fixed Ed25519 test vectors. Each signature signs the FULL canonical
// message (S1): `{slug}:{version}:{sha256}:{status}:{archive_url}:{min}:
// {max}`. Independent of MAINTAINER_PUBLIC_KEY_HEX — never reuse a real
// signing key in tests. Generated offline; the vector tests below pin the
// exact message format (a runtime-signing helper would share
// `signed_message` with the verifier and mask a format bug).
const TEST_PUB_HEX: &str = "f739009793489cbdcdc85b4e1c7ea240ba5b7d77c063dde74ba5fc276141e05a";
// signs "stt-whisper:1.0.0:deadbeef:stable::0.1.0:*" (archive_url empty)
const TEST_SIG_STABLE_HEX: &str = "bf705a101881df3974d3ee3b497bce053d5a12d51fca4e2011fe4a07b8006525b8cb793c10cdea0418a9f2b7640fcd70b9346accf38ba3e1a39079938f550c05";
// signs "stt-whisper:1.0.0:deadbeef:revoked::0.1.0:*"
const TEST_SIG_REVOKED_HEX: &str = "fe5e4d32772345c35fc808219c938ce317047177e4e6530c8a5c4aeccd522f6e8658cdec082c48bf0c8da19039586b0e96f225e8c45eedd79951d0ef9a3ac60e";
// signs "stt-whisper:1.0.0:deadbeef:stable:dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip:0.1.0:*"
const TEST_SIG_RELATIVE_HEX: &str = "f46ace42954edf4e5648aafef24e076224bf6187f4adbce9204c66a81ad170adadadab3395067dc3ba57a7c9aaf50cf6fe045f9383acce4ea4d30d8823e73b06";

/// A `signed_entry` with `status`/`archive_url`/compat bounds exactly as
/// the stable vector signed them — mutating any bound field must break it.
fn signed_stable_entry() -> RegistryEntry {
    signed_entry("stable")
}

#[test]
fn signature_verifies_with_matching_key_and_message() {
    let entry = signed_stable_entry();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_ok());
}

#[test]
fn signature_rejected_when_sha256_tampered() {
    // Same signature, but sha256 no longer matches what was signed —
    // simulates a compromised registry host swapping the archive/hash
    // while leaving an old valid-looking signature in place.
    let mut entry = signed_stable_entry();
    entry.sha256 = "cafebabe".into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
}

#[test]
fn signature_rejected_when_status_tampered() {
    // S1 regression: a compromised channel flips revoked → stable; the
    // signature must no longer verify, so the is_revoked gate can't be
    // bypassed by an attacker who controls the serving channel.
    let mut entry = signed_stable_entry();
    entry.status = "revoked".into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
}

#[test]
fn signature_rejected_when_archive_url_tampered() {
    // S1 regression: a compromised channel redirects archive_url to an
    // arbitrary URL (request forgery) — the signature must break.
    let mut entry = signed_stable_entry();
    entry.archive_url = "https://evil.example/plugin.zip".into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
}

#[test]
fn signature_rejected_when_kernel_bounds_tampered() {
    // S1: loosening min/max_kernel_version must break the signature too.
    let mut entry = signed_stable_entry();
    entry.min_kernel_version = "0.0.1".into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
    let mut entry = signed_stable_entry();
    entry.max_kernel_version = "9.9.9".into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
}

#[test]
fn relative_archive_url_verifies_in_as_served_form() {
    // The stable vector's relative-URL sibling: a registry v2 entry whose
    // archive_url is relative verifies in that raw form (S1) — install
    // resolves it after verification.
    let mut entry = signed_stable_entry();
    entry.archive_url = "dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip".into();
    entry.signature = TEST_SIG_RELATIVE_HEX.into();
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_ok());
}

#[test]
fn signature_rejected_when_empty() {
    let mut entry = make_entry("0.1.0", "*");
    entry.sha256 = "deadbeef".into();
    assert!(entry.signature.is_empty());
    assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
}

#[test]
fn signature_rejected_with_wrong_public_key() {
    let entry = signed_stable_entry();
    // Pinned maintainer key, not the test key that actually signed this.
    assert!(verify_entry_signature(&entry, None).is_err());
}
