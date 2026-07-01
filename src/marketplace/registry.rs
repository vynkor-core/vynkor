use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use semver::Version;
use serde::{Deserialize, Serialize};

use crate::utils::errors::VeyronError;

const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json";

const CACHE_TTL: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEntry {
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
}

fn cache_path() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs_home().join(".cache"));
    base.join("veyron").join("registry.json")
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

fn cache_is_fresh(path: &PathBuf) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .map(|age| age < CACHE_TTL)
                .unwrap_or(false)
        })
        .unwrap_or(false)
}

fn read_cache(path: &PathBuf) -> Option<Vec<PluginEntry>> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn write_cache(path: &PathBuf, entries: &[PluginEntry]) -> Result<(), VeyronError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| VeyronError::CacheError(format!("create cache dir: {e}")))?;
    }
    let json = serde_json::to_string(entries)
        .map_err(|e| VeyronError::CacheError(format!("serialize registry: {e}")))?;
    fs::write(path, json).map_err(|e| VeyronError::CacheError(format!("write cache: {e}")))?;
    Ok(())
}

async fn fetch_from_network(url: &str) -> Result<Vec<PluginEntry>, VeyronError> {
    let response = reqwest::get(url)
        .await
        .map_err(|e| VeyronError::NetworkError(format!("fetch registry: {e}")))?;

    if !response.status().is_success() {
        return Err(VeyronError::NetworkError(format!(
            "registry fetch returned HTTP {}",
            response.status()
        )));
    }

    response
        .json::<Vec<PluginEntry>>()
        .await
        .map_err(|e| VeyronError::NetworkError(format!("parse registry JSON: {e}")))
}

/// Fetch the plugin registry, using disk cache when fresh enough.
///
/// `refresh = true` bypasses the TTL and re-fetches from the network unconditionally.
/// On network failure, falls back to stale cache with a warning. Returns error only
/// when network fails *and* no cache exists.
pub async fn fetch_registry(refresh: bool) -> Result<Vec<PluginEntry>, VeyronError> {
    fetch_registry_with_url(REGISTRY_URL, refresh).await
}

/// Like `fetch_registry` but accepts a custom URL to support private registry overrides
/// set via `registry_url:` in `config.yaml`.
pub async fn fetch_registry_with_url(
    url: &str,
    refresh: bool,
) -> Result<Vec<PluginEntry>, VeyronError> {
    fetch_registry_from(url, refresh, &cache_path()).await
}

/// Internal implementation — separated so tests can inject a URL and cache path.
pub(crate) async fn fetch_registry_from(
    url: &str,
    refresh: bool,
    path: &std::path::Path,
) -> Result<Vec<PluginEntry>, VeyronError> {
    let path = path.to_path_buf();

    if !refresh && cache_is_fresh(&path) {
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
    entry: &PluginEntry,
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

    fn make_entry(min: &str, max: &str) -> PluginEntry {
        PluginEntry {
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
        let result = fetch_registry_from(&url, false, &cache).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "stt-whisper");
        assert!(cache.exists(), "cache file should be written");

        mock.assert_async().await;

        // Second call within TTL: reads from disk (mock not called again)
        let cached = fetch_registry_from(&url, false, &cache).await.unwrap();
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

        fetch_registry_from(&url, false, &cache).await.unwrap();
        // refresh = true forces network even though cache is fresh
        fetch_registry_from(&url, true, &cache).await.unwrap();

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
        let result = fetch_registry_from("http://127.0.0.1:1", false, &cache).await;
        assert!(result.is_ok(), "should fall back to stale cache");
        assert_eq!(result.unwrap()[0].slug, "stt-whisper");
    }

    #[tokio::test]
    async fn no_cache_network_failure_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("registry.json");

        let result = fetch_registry_from("http://127.0.0.1:1", false, &cache).await;
        assert!(
            result.is_err(),
            "should error when no cache and network fails"
        );
    }
}
