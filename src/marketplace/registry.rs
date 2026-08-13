use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use ed25519_dalek::{Signature, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::marketplace::state::{load_state, state_dir, InstalledState};
use crate::utils::errors::VeyronError;

/// The official registry URL. Public so the CLI can record it as the install
/// source in the state store when no `registry_url:` override is configured.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/veyron-core/veyron-plugins/main/registry.json";

/// Bump whenever the on-disk cache layout changes incompatibly — a cache
/// written by an older kernel must be read as empty, never misread.
pub const REGISTRY_CACHE_SCHEMA_VERSION: u32 = 1;

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

/// Default lifecycle status — the flat registry form predates `status`, so an
/// absent field must read as the benign value, not `""`.
fn default_status() -> String {
    "stable".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub archive_url: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub min_kernel_version: String,
    #[serde(default)]
    pub max_kernel_version: String,
    /// Ed25519 signature (hex, 64 bytes) over `"{slug}:{version}:{sha256}"`,
    /// produced by the maintainer key whose public half is pinned in
    /// [`MAINTAINER_PUBLIC_KEY_HEX`] (or an operator-configured override for
    /// private registries). Defaults empty for old cached/serialized entries
    /// — `verify_entry_signature` rejects an empty or malformed signature
    /// rather than treating it as "unsigned = trusted".
    #[serde(default)]
    pub signature: String,
    /// Lifecycle status from the registry v2 plan (veyron-plugins roadmap):
    /// `stable` (default), `beta`, `deprecated`, `hidden`, `revoked`. Only
    /// `revoked` is enforced by the kernel — install refuses it no matter how
    /// fresh the cache is (R10-03). In the v2 map form a document-level
    /// `revoked` list is folded into each matching entry's `status` at parse
    /// time, so downstream code only ever sees this field.
    #[serde(default = "default_status")]
    pub status: String,
}

impl RegistryEntry {
    /// `true` when the maintainer revoked this entry — never installable.
    pub fn is_revoked(&self) -> bool {
        self.status == "revoked"
    }
}

/// Registry document metadata (the v2 `meta` object): the authoritative
/// cache-invalidation signal, complementing the mtime TTL. Absent in the
/// current flat-array form.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryMeta {
    /// Registry document format version, when the registry ships one.
    #[serde(default, alias = "apiVersion")]
    pub api_version: Option<u32>,
    /// When the registry document was last updated; compared for equality
    /// against the cached value, so a changed value forces a refetch.
    #[serde(default, alias = "lastUpdated")]
    pub last_updated: Option<String>,
}

/// Per-plugin bookkeeping persisted in the cache (R10-03) — the inputs for
/// offline upgrade detection (`installed vs registry` without a fetch).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedPluginInfo {
    /// Version recorded in `installed.json` at the time of the last check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_version: Option<String>,
    /// Unix epoch seconds of the last successful fetch that saw this slug.
    #[serde(default)]
    pub last_check: u64,
}

/// The on-disk registry cache (R10-03): a versioned wrapper around the
/// fetched registry document, kept alongside `installed.json` in the
/// marketplace state dir. Replaces the unversioned raw mirror that used to
/// live at `~/.cache/veyron/registry.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RegistryCache {
    /// Cache layout version — a parser reading a different version must
    /// refuse the file rather than misread it.
    pub schema_version: u32,
    /// Unix epoch seconds of the last successful network fetch.
    #[serde(default)]
    pub last_check: u64,
    /// Registry document meta, when the registry ships one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<RegistryMeta>,
    /// The entries that verified against the pinned (or configured) maintainer
    /// key at write time — the cache never holds an entry install would
    /// refuse. Revoked entries are kept: revocation must outlive the TTL, not
    /// be forgotten when the cache expires.
    #[serde(default)]
    pub entries: Vec<RegistryEntry>,
    /// Per-slug bookkeeping for offline upgrade detection.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugins: BTreeMap<String, CachedPluginInfo>,
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

/// The cache lives in the marketplace state dir (with `installed.json`), not
/// the XDG cache dir — freshness/revocation policy makes it state, and one
/// `VEYRON_STATE_DIR` override relocates both.
fn cache_path(tmp_dir: &Path) -> PathBuf {
    state_dir(tmp_dir).join("registry-cache.json")
}

fn cache_is_fresh(path: &Path, ttl: Duration) -> bool {
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the cache file. A missing file, unparseable JSON, or a foreign
/// `schema_version` all read as empty — the next successful fetch rewrites it.
fn read_cache_file(path: &Path) -> Option<RegistryCache> {
    let data = fs::read_to_string(path).ok()?;
    match serde_json::from_str::<RegistryCache>(&data) {
        Ok(cache) if cache.schema_version == REGISTRY_CACHE_SCHEMA_VERSION => Some(cache),
        Ok(cache) => {
            tracing::warn!(
                "registry cache schema {} != supported {} — treating as empty",
                cache.schema_version,
                REGISTRY_CACHE_SCHEMA_VERSION
            );
            None
        }
        Err(e) => {
            tracing::warn!(
                "corrupt registry cache at {}, ignoring: {e}",
                path.display()
            );
            None
        }
    }
}

/// Write the cache atomically (temp + rename in the same dir) so a crash
/// mid-write can never leave a half-written `registry-cache.json`.
fn write_cache_file(path: &Path, cache: &RegistryCache) -> Result<(), VeyronError> {
    let parent = path
        .parent()
        .ok_or_else(|| VeyronError::CacheError("registry cache path has no parent dir".into()))?;
    fs::create_dir_all(parent)
        .map_err(|e| VeyronError::CacheError(format!("create cache dir: {e}")))?;
    let tmp = parent.join(".registry-cache.json.tmp");
    let json = serde_json::to_string_pretty(cache)
        .map_err(|e| VeyronError::CacheError(format!("serialize registry cache: {e}")))?;
    fs::write(&tmp, json).map_err(|e| VeyronError::CacheError(format!("write cache: {e}")))?;
    fs::rename(&tmp, path).map_err(|e| VeyronError::CacheError(format!("write cache: {e}")))?;
    Ok(())
}

/// Fetch the raw registry document body, rejecting non-2xx and empty bodies
/// with actionable errors.
async fn fetch_from_network(url: &str) -> Result<String, VeyronError> {
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

    Ok(body)
}

/// A parsed registry document — either the current flat array or the registry
/// v2 map form (veyron-plugins roadmap, "Infrastructure Evolution"). v2's
/// `versions` are flattened into one [`RegistryEntry`] per version, and its
/// root `revoked` list is folded into each matching entry's `status`, so
/// downstream code only ever sees [`RegistryEntry::is_revoked`].
struct RegistryDocument {
    meta: Option<RegistryMeta>,
    entries: Vec<RegistryEntry>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RegistryDocShape {
    /// Current form: `[ { entry }, ... ]`.
    Flat(Vec<RegistryEntry>),
    /// Registry v2 form: `{ "meta": {...}, "revoked": [...], "<slug>": {...} }`.
    Map(RegistryDocMap),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryDocMap {
    #[serde(default)]
    meta: Option<RegistryMeta>,
    /// Slugs or `slug@version` revoked at the document level.
    #[serde(default)]
    revoked: Vec<String>,
    #[serde(flatten)]
    plugins: BTreeMap<String, MapPluginEntry>,
}

#[derive(Deserialize)]
struct MapPluginEntry {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default = "default_status")]
    status: String,
    #[serde(default, alias = "sourceUrl")]
    source_url: String,
    /// version -> delivery metadata (veyron-plugins roadmap "versions object").
    #[serde(default)]
    versions: BTreeMap<String, MapVersion>,
}

#[derive(Deserialize)]
struct MapVersion {
    #[serde(default, alias = "archiveUrl")]
    archive_url: String,
    #[serde(default)]
    sha256: String,
    #[serde(default)]
    signature: String,
    #[serde(default, alias = "minKernelVersion")]
    min_kernel_version: String,
    #[serde(default, alias = "maxKernelVersion")]
    max_kernel_version: String,
}

fn parse_registry_document(body: &str) -> Result<RegistryDocument, VeyronError> {
    let shape: RegistryDocShape = serde_json::from_str(body)
        .map_err(|e| VeyronError::NetworkError(format!("parse registry JSON: {e}")))?;
    Ok(match shape {
        RegistryDocShape::Flat(entries) => RegistryDocument {
            meta: None,
            entries,
        },
        RegistryDocShape::Map(form) => {
            let mut entries = Vec::new();
            for (slug, plugin) in form.plugins {
                for (version, v) in plugin.versions {
                    let status = if form
                        .revoked
                        .iter()
                        .any(|r| r == &slug || r == &format!("{slug}@{version}"))
                    {
                        "revoked".into()
                    } else {
                        plugin.status.clone()
                    };
                    entries.push(RegistryEntry {
                        id: if plugin.id.is_empty() {
                            slug.clone()
                        } else {
                            plugin.id.clone()
                        },
                        slug: slug.clone(),
                        name: plugin.name.clone(),
                        description: plugin.description.clone(),
                        version,
                        permissions: plugin.permissions.clone(),
                        archive_url: v.archive_url,
                        source_url: plugin.source_url.clone(),
                        sha256: v.sha256,
                        min_kernel_version: v.min_kernel_version,
                        max_kernel_version: v.max_kernel_version,
                        signature: v.signature,
                        status,
                    });
                }
            }
            RegistryDocument {
                meta: form.meta,
                entries,
            }
        }
    })
}

/// Filter `entries` down to those whose maintainer signature verifies (T-11),
/// returning `(verified, dropped)` with the dropped slugs+reason for logging.
/// A signature is an entry's trust anchor: the cache never persists an entry
/// install would refuse, so a stale-cache fallback never serves unverified
/// content.
fn verify_entries(
    entries: &[RegistryEntry],
    public_key: Option<&str>,
) -> (Vec<RegistryEntry>, Vec<String>) {
    let mut verified = Vec::new();
    let mut dropped = Vec::new();
    for e in entries {
        match verify_entry_signature(e, public_key) {
            Ok(()) => verified.push(e.clone()),
            Err(err) => dropped.push(format!("{}@{} ({err})", e.slug, e.version)),
        }
    }
    (verified, dropped)
}

/// Per-slug bookkeeping for the cache: the installed version (from the state
/// store) and the fetch time, so offline `vyn plugin list --installed` can
/// later show upgrade availability without a fetch.
fn snapshot_plugins(
    entries: &[RegistryEntry],
    installed: &InstalledState,
    at: u64,
) -> BTreeMap<String, CachedPluginInfo> {
    entries
        .iter()
        .map(|e| {
            (
                e.slug.clone(),
                CachedPluginInfo {
                    installed_version: installed.get(&e.slug).map(|i| i.version.clone()),
                    last_check: at,
                },
            )
        })
        .collect()
}

/// Fetch the plugin registry, using the versioned disk cache when fresh.
///
/// `refresh = true` bypasses the TTL and re-fetches unconditionally.
/// `public_key = None` uses the pinned [`MAINTAINER_PUBLIC_KEY_HEX`].
/// On network failure, falls back to the last *verified* stale cache with a
/// warning (a cache is only ever written from verified entries). Returns error
/// only when the network fails and no usable cache exists.
pub async fn fetch_registry(
    refresh: bool,
    cache_ttl_secs: u64,
    tmp_dir: &Path,
    public_key: Option<&str>,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    fetch_registry_with_url(
        DEFAULT_REGISTRY_URL,
        refresh,
        cache_ttl_secs,
        tmp_dir,
        public_key,
    )
    .await
}

/// Like `fetch_registry` but accepts a custom URL to support private registry
/// overrides set via `registry_url:` in `config.yaml`.
pub async fn fetch_registry_with_url(
    url: &str,
    refresh: bool,
    cache_ttl_secs: u64,
    tmp_dir: &Path,
    public_key: Option<&str>,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    let installed = load_state(tmp_dir);
    fetch_registry_from(
        url,
        refresh,
        &cache_path(tmp_dir),
        cache_ttl_secs,
        public_key,
        &installed,
    )
    .await
}

/// Resolve relative `archive_url` values against the registry's own base URL.
///
/// Registry v2 entries may use relative URLs (e.g.
/// `dist/ai/versions/0.1.0/ai-0.1.0.zip`) so the artifact store can move
/// hosts (GitHub → own VPS → R2, or a community marketplace) with a
/// one-line `registry_url` change and nothing re-published. `base_url` is
/// the URL the registry document was fetched from; entries that already
/// carry an absolute URL are left untouched. Entries whose URL cannot be
/// resolved (malformed base or unjoinable path) are left as-is — the
/// install path surfaces the resulting download error.
fn resolve_relative_archive_urls(entries: &mut [RegistryEntry], base_url: &str) {
    let Ok(base) = url::Url::parse(base_url) else {
        tracing::warn!(
            "registry: cannot resolve relative archive_urls against non-URL base {base_url:?}"
        );
        return;
    };
    for entry in entries {
        // Url::parse fails for relative references — that error IS the signal.
        if url::Url::parse(&entry.archive_url).is_err() {
            if let Ok(resolved) = base.join(&entry.archive_url) {
                entry.archive_url = resolved.to_string();
            }
        }
    }
}

/// Internal implementation — separated so tests can inject a URL, cache path,
/// and installed-state snapshot.
pub(crate) async fn fetch_registry_from(
    url: &str,
    refresh: bool,
    path: &Path,
    cache_ttl_secs: u64,
    public_key: Option<&str>,
    installed: &InstalledState,
) -> Result<Vec<RegistryEntry>, VeyronError> {
    let ttl = Duration::from_secs(cache_ttl_secs);

    if !refresh && cache_is_fresh(path, ttl) {
        if let Some(cache) = read_cache_file(path) {
            if !cache.entries.is_empty() {
                return Ok(cache.entries);
            }
        }
    }

    match fetch_from_network(url).await {
        Ok(body) => {
            let mut doc = parse_registry_document(&body)?;
            resolve_relative_archive_urls(&mut doc.entries, url);
            let (verified, dropped) = verify_entries(&doc.entries, public_key);
            if !dropped.is_empty() {
                if verified.is_empty() {
                    // Never clobber a good snapshot with an all-unverified
                    // fetch (compromised channel / wrong key): keep the
                    // previous cache. The fetched entries still go to the
                    // caller — install re-verifies per entry and fails closed.
                    tracing::warn!(
                        "registry fetch: 0/{} entries verified — keeping previous cache: {dropped:?}",
                        doc.entries.len()
                    );
                } else {
                    tracing::warn!(
                        "registry fetch: dropped {} unverified entries from cache: {dropped:?}",
                        dropped.len()
                    );
                }
            }

            if !verified.is_empty() {
                let cache = RegistryCache {
                    schema_version: REGISTRY_CACHE_SCHEMA_VERSION,
                    last_check: now_secs(),
                    meta: doc.meta,
                    plugins: snapshot_plugins(&verified, installed, now_secs()),
                    entries: verified,
                };
                if let Err(e) = write_cache_file(path, &cache) {
                    tracing::warn!("failed to write registry cache: {e}");
                }
            }
            Ok(doc.entries)
        }
        Err(network_err) => {
            if let Some(cache) = read_cache_file(path) {
                if !cache.entries.is_empty() {
                    tracing::warn!(
                        "registry fetch failed ({}); using stale verified cache",
                        network_err
                    );
                    return Ok(cache.entries);
                }
            }
            Err(network_err)
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

    // A `make_entry` whose signature verifies under TEST_PUB_HEX (the fixed
    // test vector signs "stt-whisper:1.0.0:deadbeef").
    fn signed_entry(status: &str) -> RegistryEntry {
        let mut entry = make_entry("0.1.0", "*");
        entry.sha256 = "deadbeef".into();
        entry.signature = TEST_SIG_HEX.into();
        entry.status = status.into();
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
        assert_eq!(result[0].signature, TEST_SIG_HEX);
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
    async fn fetch_resolves_relative_archive_urls_against_registry_base() {
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
            sig = TEST_SIG_HEX
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
        assert_eq!(
            result[0].archive_url,
            format!(
                "{}/dist/stt-whisper/versions/1.0.0/stt-whisper-1.0.0.zip",
                server.url()
            )
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
}
