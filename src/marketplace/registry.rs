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
///
/// v2 (S1): entries now store the archive_url exactly as served (relative
/// URLs are no longer resolved at fetch time — resolution moved to install,
/// after signature verification, because the signature binds the as-served
/// URL). A v1 cache holds resolved URLs whose signatures were computed over
/// the old message and would fail verification under the new one.
pub const REGISTRY_CACHE_SCHEMA_VERSION: u32 = 2;

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
    /// Ed25519 signature (hex, 64 bytes) over the full canonical entry
    /// `"{slug}:{version}:{sha256}:{status}:{archive_url}:{min_kernel_version}:
    /// {max_kernel_version}"` (see [`signed_message`]), produced by the
    /// maintainer key whose public half is pinned in
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
/// different entry that happens to share the same archive hash. Binding the
/// full canonical entry — `status`, `archive_url` (as served, relative URLs
/// included), and the kernel-compat bounds — closes the S1 gaps: a
/// compromised channel can no longer flip `revoked → stable` (the entry still
/// verifies), redirect `archive_url` to an arbitrary URL, or loosen the
/// compat range without breaking the signature.
fn signed_message(entry: &RegistryEntry) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        entry.slug,
        entry.version,
        entry.sha256,
        entry.status,
        entry.archive_url,
        entry.min_kernel_version,
        entry.max_kernel_version,
    )
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
                 match the entry (slug/version/sha256/status/archive_url/kernel-compat). \
                 Aborting — do not proceed.",
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
///
/// S1: resolution happens only at install time, after `verify_entry_signature`
/// — the signature binds the archive_url exactly as served, so a relative URL
/// must be verified in that raw form and resolved afterwards.
pub(crate) fn resolve_relative_archive_urls(entries: &mut [RegistryEntry], base_url: &str) {
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
            // Entries stay as served — relative archive_urls are resolved at
            // install time, after signature verification (S1: the signature
            // binds the as-served URL; resolving here would break it).
            let doc = parse_registry_document(&body)?;
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
#[path = "registry_tests.rs"]
mod tests;
