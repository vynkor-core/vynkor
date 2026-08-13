use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::utils::errors::VeyronError;

/// The official registry URL. Public so the CLI can record it as the install
/// source in the state store when no `registry_url:` override is configured.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json";

/// Ed25519 public key (hex, 32 bytes) pinned at compile time. `sha256` alone
/// (T-11) proves nothing about publisher trust if the channel serving
/// `registry.json` is compromised — the attacker controls the hash and the
/// archive together. The signature closes that gap: it is produced by an
/// offline maintainer key never present on the serving infrastructure, so an
/// attacker who only compromises the registry host/CDN cannot forge it.
///
/// Rotate by re-signing every registry entry with the new key and shipping
/// the new constant in a kernel release; the corresponding private key must
/// never be committed to this repo.
const MAINTAINER_PUBLIC_KEY_HEX: &str =
    "ed8c39a19dcbfed1a3a436b914a8ce9bf2b449c534808ce92c78adcfa2590928";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub permissions: Vec<String>,
    pub archive_url: String,
    pub source_url: String,
    pub sha256: String,
    pub min_kernel_version: String,
    pub max_kernel_version: String,
    /// Ed25519 signature (hex, 64 bytes) over `"{slug}:{version}:{sha256}"`,
    /// produced by the maintainer key whose public half is pinned in
    /// [`MAINTAINER_PUBLIC_KEY_HEX`] (or an operator-configured override for
    /// private registries). Defaults empty for old cached/serialized entries
    /// — `verify_entry_signature` rejects an empty or malformed signature
    /// rather than treating it as "unsigned = trusted".
    #[serde(default)]
    pub signature: String,
}

fn hex_decode(s: &str) -> Result<Vec<u8>, VeyronError> {
    if !s.len().is_multiple_of(2) {
        return Err(VeyronError::Internal("invalid hex: odd length".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| VeyronError::Internal(format!("invalid hex: {e}")))
        })
        .collect()
}

/// The message a maintainer signature is computed over. Binding `slug` and
/// `version` (not just `sha256`) prevents an attacker who controls the
/// serving channel from splicing a valid signature from one entry onto a
/// different entry that happens to share the same archive hash.
fn signed_message(entry: &RegistryEntry) -> String {
    format!("{}:{}:{}", entry.slug, entry.version, entry.sha256)
}

/// Verify `entry.signature` against `public_key_hex` (pass `None` to use the
/// pinned [`MAINTAINER_PUBLIC_KEY_HEX`]). See T-11.
pub fn verify_entry_signature(
    entry: &RegistryEntry,
    public_key_hex: Option<&str>,
) -> Result<(), VeyronError> {
    let key_hex = public_key_hex.unwrap_or(MAINTAINER_PUBLIC_KEY_HEX);

    let key_bytes = hex_decode(key_hex)?;
    let key_bytes: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| VeyronError::Internal("marketplace public key must be 32 bytes".into()))?;
    let verifying_key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|e| VeyronError::Internal(format!("invalid marketplace public key: {e}")))?;

    let sig_bytes = hex_decode(&entry.signature).map_err(|_| {
        VeyronError::Internal(format!(
            "Plugin '{}' has a malformed signature. Aborting — do not proceed.",
            entry.slug
        ))
    })?;
    let sig_bytes: [u8; 64] = sig_bytes.try_into().map_err(|_| {
        VeyronError::Internal(format!(
            "Plugin '{}' signature must be 64 bytes. Aborting — do not proceed.",
            entry.slug
        ))
    })?;
    let signature = Signature::from_bytes(&sig_bytes);

    verifying_key
        .verify_strict(signed_message(entry).as_bytes(), &signature)
        .map_err(|_| {
            VeyronError::Internal(format!(
                "Plugin '{}' failed signature verification — the maintainer signature does not \
                 match slug/version/sha256. Aborting — do not proceed.",
                entry.slug
            ))
        })
}

fn cache_path(tmp_dir: &std::path::Path) -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home(tmp_dir).join(".cache"));
    base.join("veyron").join("registry.json")
}

/// `tmp_dir`: fallback base when `$HOME` is unset. Must be the kernel's
/// private scratch dir (`Config::tmp_dir`), never the shared, world-writable
/// `/tmp` (AUDIT M-09) — matches the hardening already applied to
/// `default_pid_path`/`default_socket_path` in `utils::config`.
fn dirs_home(tmp_dir: &std::path::Path) -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tmp_dir.to_path_buf())
}

fn cache_is_fresh(path: &PathBuf, ttl: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .map(|age| age < ttl)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn read_cache(path: &PathBuf) -> Option<Vec<RegistryEntry>> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(path: &PathBuf, entries: &[RegistryEntry]) -> Result<(), VeyronError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| VeyronError::CacheError(format!("create cache dir: {e}")))?;
    }
    let json = serde_json::to_string(entries)
        .map_err(|e| VeyronError::CacheError(format!("serialize registry: {e}")))?;
    fs::write(path, json).map_err(|e| VeyronError::CacheError(format!("write cache: {e}")))?;
    Ok(())
}

async fn fetch_from_network(url: &str) -> Result<Vec<RegistryEntry>, VeyronError> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| VeyronError::NetworkError(format!("fetch registry: {e}")))?;

    if !response.status().is_success() {
        return Err(VeyronError::NetworkError(format!(
            "registry fetch returned HTTP {}",
            response.status()
        )));
    }

    let body = response
        .text()
        .await
        .map_err(|e| VeyronError::NetworkError(format!("read registry response: {e}")))?;

    if body.trim().is_empty() {
        return Err(VeyronError::NetworkError(format!(
            "registry response body was empty (fetched from {url})"
        )));
    }

    serde_json::from_str(&body)
        .map_err(|e| VeyronError::NetworkError(format!("parse registry JSON: {e}")))
}

/// Fetch the plugin registry, using disk cache when fresh enough.
///
/// `refresh = true` bypasses the TTL and re-fetches from the network unconditionally.
/// On network failure, falls back to stale cache with a warning. Returns error only
/// when network fails *and* no cache exists.
pub async fn fetch_registry(
    refresh: bool,
    cache_ttl_secs: u64,
    tmp_dir: &std::path::Path,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    fetch_registry_with_url(DEFAULT_REGISTRY_URL, refresh, cache_ttl_secs, tmp_dir).await
}

/// Like `fetch_registry` but accepts a custom URL to support private registry overrides
/// set via `registry_url:` in `config.yaml`.
pub async fn fetch_registry_with_url(
    url: &str,
    refresh: bool,
    cache_ttl_secs: u64,
    tmp_dir: &std::path::Path,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    fetch_registry_from(url, refresh, &cache_path(tmp_dir), cache_ttl_secs).await
}

/// Internal implementation — separated so tests can inject a URL and cache path.
pub(crate) async fn fetch_registry_from(
    url: &str,
    refresh: bool,
    path: &std::path::Path,
    cache_ttl_secs: u64,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    let path = path.to_path_buf();
    let ttl = Duration::from_secs(cache_ttl_secs);

    if !refresh && cache_is_fresh(&path, ttl) {
        if let Some(entries) = read_cache(&path) {
            return Ok(entries);
        }
    }

    match fetch_from_network(url).await {
        Ok(entries) => {
            if let Err(e) = write_cache(&path, &entries) {
                tracing::warn!("failed to write registry cache: {e}");
            }
            Ok(entries)
        }
        Err(network_err) => {
            if let Some(entries) = read_cache(&path) {
                tracing::warn!("registry fetch failed ({}); using stale cache", network_err);
                Ok(entries)
            } else {
                Err(network_err)
            }
        }
    }
}

/// Check whether `running_kernel` falls within the plugin's stated compatibility range.
pub fn check_kernel_compatibility(
    entry: &RegistryEntry,
    running_kernel: &Version,
) -> Result<(), VeyronError> {
    let min = Version::parse(&entry.min_kernel_version).map_err(|e| {
        VeyronError::Incompatible(format!(
            "Plugin '{}' has invalid min_kernel_version '{}': {e}",
            entry.slug, entry.min_kernel_version
        ))
    })?;

    if *running_kernel < min {
        return Err(VeyronError::Incompatible(format!(
            "Plugin '{}' requires Veyron kernel >= {}, you are running {}",
            entry.slug, min, running_kernel
        )));
    }

    if entry.max_kernel_version != "*" {
        let max = Version::parse(&entry.max_kernel_version).map_err(|e| {
            VeyronError::Incompatible(format!(
                "Plugin '{}' has invalid max_kernel_version '{}': {e}",
                entry.slug, entry.max_kernel_version
            ))
        })?;

        if *running_kernel > max {
            return Err(VeyronError::Incompatible(format!(
                "Plugin '{}' requires Veyron kernel <= {}, you are running {}",
                entry.slug, max, running_kernel
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        }
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
            msg.contains("requires Veyron kernel >= 0.3.0")
                && msg.contains("you are running 0.2.0"),
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
            msg.contains("requires Veyron kernel <= 1.0.0")
                && msg.contains("you are running 2.0.0"),
            "unexpected: {msg}"
        );
    }

    #[test]
    fn compat_star_max() {
        let entry = make_entry("0.1.0", "*");
        let kernel = Version::parse("99.0.0").unwrap();
        assert!(check_kernel_compatibility(&entry, &kernel).is_ok());
    }

    #[tokio::test]
    async fn fetch_writes_and_reads_cache() {
        let mut server = mockito::Server::new_async().await;
        let entries = vec![make_entry("0.1.0", "*")];
        let body = serde_json::to_string(&entries).unwrap();

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

        // First call: fetches from network and writes cache
        let result = fetch_registry_from(&url, false, &cache, 3600)
            .await
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "stt-whisper");
        assert!(cache.exists(), "cache file should be written");

        mock.assert_async().await;

        // Second call within TTL: reads from disk (mock not called again)
        let cached = fetch_registry_from(&url, false, &cache, 3600)
            .await
            .unwrap();
        assert_eq!(cached[0].slug, "stt-whisper");
    }

    #[tokio::test]
    async fn refresh_bypasses_ttl() {
        let mut server = mockito::Server::new_async().await;
        let entries = vec![make_entry("0.1.0", "*")];
        let body = serde_json::to_string(&entries).unwrap();

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

        fetch_registry_from(&url, false, &cache, 3600)
            .await
            .unwrap();
        // refresh = true forces network even though cache is fresh
        fetch_registry_from(&url, true, &cache, 3600).await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn stale_cache_used_on_network_failure() {
        let entries = vec![make_entry("0.1.0", "*")];
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("registry.json");

        // Pre-populate cache
        write_cache(&cache, &entries).unwrap();

        // Use a URL that will definitely fail
        let result = fetch_registry_from("http://127.0.0.1:1", false, &cache, 3600).await;
        assert!(result.is_ok(), "should fall back to stale cache");
        assert_eq!(result.unwrap()[0].slug, "stt-whisper");
    }

    #[tokio::test]
    async fn no_cache_network_failure_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("registry.json");

        let result = fetch_registry_from("http://127.0.0.1:1", false, &cache, 3600).await;
        assert!(
            result.is_err(),
            "should error when no cache and network fails"
        );
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

        let err = fetch_registry_from(&url, false, &cache, 3600)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("registry response body was empty"),
            "unexpected: {msg}"
        );

        mock.assert_async().await;
    }

    // Fixed Ed25519 test vector: signs `"stt-whisper:1.0.0:deadbeef"`.
    // Independent of MAINTAINER_PUBLIC_KEY_HEX — never reuse a real signing
    // key in tests.
    const TEST_PUB_HEX: &str = "6c4850b5614a1b4d91591408aff0cf9c40e9f00f845a7371506689851d82a864";
    const TEST_SIG_HEX: &str = "9b9700219f9ed1a2b5ade515a3c130b20e096c42d5f5e39d1a06b1975065e59dabf25827147842f51794a635c29849f6cca2f28933a96bd750db56a298b09e0f";

    #[test]
    fn signature_verifies_with_matching_key_and_message() {
        let mut entry = make_entry("0.1.0", "*");
        entry.sha256 = "deadbeef".into();
        entry.signature = TEST_SIG_HEX.into();
        assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_ok());
    }

    #[test]
    fn signature_rejected_when_sha256_tampered() {
        // Same signature, but sha256 no longer matches what was signed —
        // simulates a compromised registry host swapping the archive/hash
        // while leaving an old valid-looking signature in place.
        let mut entry = make_entry("0.1.0", "*");
        entry.sha256 = "cafebabe".into();
        entry.signature = TEST_SIG_HEX.into();
        assert!(verify_entry_signature(&entry, Some(TEST_PUB_HEX)).is_err());
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
        let mut entry = make_entry("0.1.0", "*");
        entry.sha256 = "deadbeef".into();
        entry.signature = TEST_SIG_HEX.into();
        // Pinned maintainer key, not the test key that actually signed this.
        assert!(verify_entry_signature(&entry, None).is_err());
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

        let err = fetch_registry_from(&url, false, &cache, 3600)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("parse registry JSON"), "unexpected: {msg}");

        mock.assert_async().await;
    }
}
