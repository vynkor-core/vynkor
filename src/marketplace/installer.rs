use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use indicatif::{ProgressBar, ProgressStyle};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::marketplace::registry::{
    check_kernel_compatibility, verify_entry_signature, RegistryEntry,
};
use crate::marketplace::state::{load_state, record_install, InstalledEntry};
use crate::proto::veyron::PermissionType;
use crate::utils::errors::VeyronError;

// permission strings accepted in plugin.json: every proto enum variant in both
// the documented lowercase form (storage) and the PERMISSION_-prefixed proto
// name (PERMISSION_STORAGE), so a future proto permission can't silently break
// installs. UNKNOWN stays excluded. prost 0.13+ has no values(); probe codes,
// stopping after a run of misses so the reserved gap (7) is fine.
fn known_permissions() -> &'static [String] {
    static KNOWN: OnceLock<Vec<String>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        let mut out = Vec::new();
        let mut misses = 0;
        for i in 0i32.. {
            match PermissionType::try_from(i) {
                Ok(pt) => {
                    let name = pt.as_str_name();
                    if name != "PERMISSION_UNKNOWN" {
                        out.push(name.to_string());
                        out.push(
                            name.strip_prefix("PERMISSION_")
                                .unwrap_or(name)
                                .to_ascii_lowercase(),
                        );
                    }
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
    })
}

#[derive(Debug, Deserialize)]
pub struct InstallManifest {
    pub plugin_id: String,
    pub version: String,
    pub permissions: Vec<String>,
    pub binary: String,
    pub kernel_compatibility_range: KernelCompatRange,
    pub events: Option<Vec<String>>,
    pub actions: Option<Vec<ActionSpec>>,
    /// Plugin IDs that must be loaded and registered before this plugin starts.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Manifest v2 extraction allowlist: the archive entries to extract into
    /// the plugin dir. Empty = not declared (legacy extract-everything).
    #[serde(default)]
    pub files: Vec<String>,
}

/// A single entry in a v2 manifest's `actions` array. Legacy manifests declare
/// actions as plain strings; v2 manifests declare objects with a per-action
/// `permission` (the permission a *caller* must hold to invoke the action,
/// T-19 anti-laundering) plus optional JSON-Schema `input`/`output`. The
/// kernel accepts both forms — the untagged enum parses whichever is present.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ActionSpec {
    Legacy(String),
    V2(ActionSpecV2),
}

#[derive(Debug, Deserialize)]
pub struct ActionSpecV2 {
    pub name: String,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
}

impl ActionSpec {
    pub fn name(&self) -> &str {
        match self {
            ActionSpec::Legacy(s) => s,
            ActionSpec::V2(spec) => &spec.name,
        }
    }

    pub fn permission(&self) -> Option<&str> {
        match self {
            ActionSpec::Legacy(_) => None,
            ActionSpec::V2(spec) => spec.permission.as_deref(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct KernelCompatRange {
    pub min: String,
    pub max: String,
}

/// `tmp_dir`: fallback base when both `VEYRON_PLUGIN_DIR` and `$HOME` are unset.
/// Must be the kernel's private scratch dir (`Config::tmp_dir`), never the
/// shared, world-writable `/tmp` (AUDIT M-09) — matches the hardening already
/// applied to `default_pid_path`/`default_socket_path` in `utils::config`.
pub fn plugin_dir(tmp_dir: &Path) -> PathBuf {
    std::env::var("VEYRON_PLUGIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join(".local/lib/veyron/plugins"),
            Err(_) => tmp_dir.join("plugins"),
        })
}

/// Staging directory for an in-progress install. Deliberately placed inside
/// `plugin_dir()` rather than `/tmp` — the final install step atomically
/// renames this directory into place, and `fs::rename` fails with EXDEV
/// ("Invalid cross-device link") when source and destination are on
/// different filesystems, which `/tmp` (often tmpfs) commonly is relative
/// to the plugin directory.
fn tmp_install_dir(plugin_dir: &Path, slug: &str) -> PathBuf {
    plugin_dir.join(format!(".install-tmp-{slug}"))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What `install` placed on disk, so the CLI can point the operator at it and
/// write a per-plugin drop-in auto-spawn config.
pub struct InstalledPlugin {
    pub slug: String,
    pub plugin_id: String,
    pub version: String,
    /// Absolute path of the installed executable (dest / manifest.binary).
    pub binary_path: PathBuf,
}

/// Execute the 8-step atomic installation pipeline for a plugin.
#[allow(clippy::too_many_arguments)]
pub async fn install(
    entries: &[RegistryEntry],
    target: &str,
    tmp_dir: &Path,
    max_archive_bytes: u64,
    max_extracted_bytes: u64,
    max_archive_entries: usize,
    marketplace_public_key: Option<&str>,
    source_url: &str,
) -> Result<InstalledPlugin, VeyronError> {
    // Step 1 — Resolve metadata
    let entry = entries
        .iter()
        .find(|e| e.slug == target || e.id == target)
        .ok_or_else(|| {
            VeyronError::Internal(format!(
                "Plugin '{target}' not found. Run 'vyn plugin search <query>' to browse."
            ))
        })?;

    // R10-03 — a revoked entry is never installable, whether it came from a
    // fresh fetch or the stale cache: revocation outlives the cache TTL.
    if entry.is_revoked() {
        return Err(VeyronError::Internal(format!(
            "Plugin '{}' v{} is revoked by the maintainer. Aborting — do not install.",
            entry.slug, entry.version
        )));
    }

    let kernel_ver = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| VeyronError::Internal(format!("parse kernel version: {e}")))?;

    // Step 2 — Kernel version compatibility check
    check_kernel_compatibility(entry, &kernel_ver)
        .map_err(|e| VeyronError::Incompatible(format!("{e}. Upgrade Veyron first.")))?;

    let plugin_base = plugin_dir(tmp_dir);

    // R10-02 — same version already installed (state + dir present): warn and
    // skip the whole pipeline instead of re-downloading/re-extracting.
    let dest = plugin_base.join(&entry.slug);
    if let Some(already) = skip_reinstall(tmp_dir, &entry.slug, &entry.version, &dest) {
        println!(
            "✓ '{slug}' v{version} is already installed at {dest}/ — nothing to re-install.",
            slug = entry.slug,
            version = entry.version,
            dest = dest.display(),
        );
        return Ok(already);
    }

    let stage_dir = tmp_install_dir(&plugin_base, &entry.slug);
    let _ = fs::remove_dir_all(&stage_dir);
    fs::create_dir_all(&stage_dir).map_err(VeyronError::Io)?;

    // Step 3 — Download to $TMPDIR
    let bytes =
        match download_with_progress(&entry.archive_url, &entry.slug, max_archive_bytes).await {
            Ok(b) => b,
            Err(e) => {
                let _ = fs::remove_dir_all(&stage_dir);
                return Err(e);
            }
        };

    let archive_path = stage_dir.join(format!("{}.zip", entry.slug));
    if let Err(e) = fs::write(&archive_path, &bytes) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Io(e));
    }

    // Step 4 — SHA-256 integrity check
    let actual_hash = hex_encode(&Sha256::digest(&bytes));
    if actual_hash != entry.sha256 {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Internal(format!(
            "Archive integrity check failed. Expected {}, got {}. Aborting — do not proceed.",
            entry.sha256, actual_hash
        )));
    }

    // Step 4b — Maintainer signature check (T-11). Independent of the sha256
    // above: a compromised registry-serving channel controls both the
    // archive and its hash, but not the offline maintainer signing key.
    if let Err(e) = verify_entry_signature(entry, marketplace_public_key) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(e);
    }

    // Step 5 — Extract to temporary folder (zip-slip protection)
    let extract_dir = stage_dir.join("extracted");
    if let Err(e) = fs::create_dir_all(&extract_dir) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Io(e));
    }

    // Manifest v2 `files` extraction allowlist, read from the archive's
    // plugin.json IN MEMORY before extraction. `None` (no plugin.json, no
    // `files` key, or unreadable) = legacy extract-everything.
    let allowlist = read_files_allowlist(&archive_path);

    if let Err(e) = extract_zip(
        &archive_path,
        &extract_dir,
        max_extracted_bytes,
        max_archive_entries,
        allowlist.as_ref(),
    ) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(e);
    }

    // Step 6 — Atomic move to plugin directory
    if let Err(e) = fs::create_dir_all(&plugin_base) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Io(e));
    }

    let bak = plugin_base.join(format!("{}.bak", entry.slug));
    let had_existing = dest.exists();

    if had_existing {
        if bak.exists() {
            let _ = fs::remove_dir_all(&bak);
        }
        if let Err(e) = fs::rename(&dest, &bak) {
            let _ = fs::remove_dir_all(&stage_dir);
            return Err(VeyronError::Io(e));
        }
    }

    if let Err(e) = fs::rename(&extract_dir, &dest) {
        if had_existing {
            let _ = fs::rename(&bak, &dest);
        }
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Io(e));
    }

    // Step 7 — Final validation of plugin.json
    let manifest_path = dest.join("plugin.json");
    let manifest = match validate_manifest(&manifest_path, &kernel_ver) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&dest);
            if had_existing {
                let _ = fs::rename(&bak, &dest);
            }
            let _ = fs::remove_dir_all(&stage_dir);
            return Err(e);
        }
    };

    if had_existing {
        let _ = fs::remove_dir_all(&bak);
    }
    let _ = fs::remove_dir_all(&stage_dir);

    // R10-02 — record in the explicit state store; a plugin on disk but
    // untracked is exactly the drift this store exists to prevent.
    record_install(
        tmp_dir,
        InstalledEntry {
            slug: entry.slug.clone(),
            version: manifest.version.clone(),
            sha256: actual_hash,
            installed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            source_url: source_url.to_string(),
        },
    )?;

    // Step 8 — Success output
    let dest_str = dest.display();
    println!(
        "✓ Installed {} v{} to {dest_str}/\n   Auto-spawn entry: plugins.d/{}.yaml",
        entry.slug, manifest.version, entry.slug
    );

    Ok(InstalledPlugin {
        slug: entry.slug.clone(),
        plugin_id: manifest.plugin_id,
        version: manifest.version,
        binary_path: dest.join(manifest.binary),
    })
}

/// R10-02 — skip the whole install pipeline when the state store says `slug`
/// is already installed at `version` *and* the install dir still exists. A
/// missing dir (half-deleted install) falls through so `install` repairs it.
/// Rebuilds `InstalledPlugin` from the live manifest so the caller can still
/// write the auto-spawn drop-in config.
pub fn skip_reinstall(
    tmp_dir: &Path,
    slug: &str,
    version: &str,
    dest: &Path,
) -> Option<InstalledPlugin> {
    let state = load_state(tmp_dir);
    let tracked = state.get(slug)?;
    if tracked.version != version || !dest.exists() {
        return None;
    }
    let kernel_ver = Version::parse(env!("CARGO_PKG_VERSION")).ok()?;
    let manifest = validate_manifest(&dest.join("plugin.json"), &kernel_ver).ok()?;
    Some(InstalledPlugin {
        slug: slug.to_string(),
        plugin_id: manifest.plugin_id,
        version: manifest.version,
        binary_path: dest.join(manifest.binary),
    })
}

async fn download_with_progress(
    url: &str,
    slug: &str,
    max_archive_bytes: u64,
) -> Result<Vec<u8>, VeyronError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| VeyronError::NetworkError(format!("download {slug}: {e}")))?;

    if !resp.status().is_success() {
        return Err(VeyronError::NetworkError(format!(
            "download {slug}: HTTP {}",
            resp.status()
        )));
    }

    if let Some(len) = resp.content_length() {
        if len > max_archive_bytes {
            return Err(VeyronError::Internal(format!(
                "download {slug}: archive size {len} exceeds max {max_archive_bytes} bytes"
            )));
        }
    }

    let total = resp.content_length().unwrap_or(0);
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let mut bytes: Vec<u8> = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| VeyronError::NetworkError(format!("stream {slug}: {e}")))?
    {
        if bytes.len() as u64 + chunk.len() as u64 > max_archive_bytes {
            pb.finish_and_clear();
            return Err(VeyronError::Internal(format!(
                "download {slug}: archive exceeds max {max_archive_bytes} bytes"
            )));
        }
        pb.inc(chunk.len() as u64);
        bytes.extend_from_slice(&chunk);
    }
    pb.finish_and_clear();

    Ok(bytes)
}

/// Manifest v2 `files` extraction allowlist, read from `plugin.json` inside
/// the archive without extracting it. Returns `None` when there is no
/// allowlist (no plugin.json, no `files` key, empty list, or any read/parse
/// error) — the caller then falls back to extract-everything.
fn read_files_allowlist(archive: &Path) -> Option<HashSet<String>> {
    let file = fs::File::open(archive).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut entry = zip.by_name("plugin.json").ok()?;
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&buf).ok()?;
    let files = value.get("files")?.as_array()?;
    let set: HashSet<String> = files
        .iter()
        .filter_map(|f| f.as_str().map(str::to_string))
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

pub fn extract_zip(
    archive: &Path,
    dest: &Path,
    max_extracted_bytes: u64,
    max_archive_entries: usize,
    allowlist: Option<&HashSet<String>>,
) -> Result<(), VeyronError> {
    let file = fs::File::open(archive).map_err(VeyronError::Io)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| VeyronError::Internal(format!("open zip: {e}")))?;

    if zip.len() > max_archive_entries {
        return Err(VeyronError::Internal(format!(
            "Malformed archive: {} entries exceeds max {max_archive_entries}. Aborting.",
            zip.len()
        )));
    }

    let canon_dest = dest.canonicalize().map_err(VeyronError::Io)?;
    let mut total_extracted: u64 = 0;

    // Allowlisted names not yet seen in the archive; empty when no allowlist.
    let mut missing: HashSet<String> = allowlist.cloned().unwrap_or_default();

    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| VeyronError::Internal(format!("read zip entry: {e}")))?;
        let name = entry.name().to_owned();

        // Reject absolute paths, ".." components, and Windows prefix/root components.
        let candidate = Path::new(&name);
        let unsafe_components = candidate.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        });
        if candidate.is_absolute() || unsafe_components {
            return Err(VeyronError::Internal(format!(
                "Malformed archive: path traversal detected in entry '{name}'. Aborting."
            )));
        }

        // Manifest v2 `files` allowlist: skip entries not named in it. The
        // zip-slip check above still applies to every entry, allowlisted or not.
        if let Some(list) = allowlist {
            if !list.contains(&name) {
                continue;
            }
            missing.remove(&name);
        }

        // Skip symlinks — do not follow them during extraction.
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                continue;
            }
        }

        let out = dest.join(candidate);

        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(VeyronError::Io)?;
            // Verify dir stayed inside dest after creation (catches symlink races).
            let canon_out = out.canonicalize().map_err(VeyronError::Io)?;
            if !canon_out.starts_with(&canon_dest) {
                return Err(VeyronError::Internal(format!(
                    "Malformed archive: entry '{name}' escapes extraction dir. Aborting."
                )));
            }
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(VeyronError::Io)?;
                let canon_parent = parent.canonicalize().map_err(VeyronError::Io)?;
                if !canon_parent.starts_with(&canon_dest) {
                    return Err(VeyronError::Internal(format!(
                        "Malformed archive: entry '{name}' escapes extraction dir. Aborting."
                    )));
                }
            }
            let mut out_file = fs::File::create(&out).map_err(VeyronError::Io)?;
            // Cap on actual bytes written, not the entry's declared/compressed
            // size — a zip bomb lies about (or omits) the true decompressed
            // size, so the limit must be enforced on the copy itself.
            let budget = max_extracted_bytes.saturating_sub(total_extracted) + 1;
            let written = io::copy(&mut entry.by_ref().take(budget), &mut out_file)
                .map_err(VeyronError::Io)?;
            total_extracted += written;
            if total_extracted > max_extracted_bytes {
                return Err(VeyronError::Internal(format!(
                    "Malformed archive: decompressed size exceeds max {max_extracted_bytes} bytes. Aborting."
                )));
            }
            // restore stored unix mode (exec bit) — fs::File::create gives 0644,
            // so a plugin binary would otherwise extract non-executable
            #[cfg(unix)]
            if let Some(mode) = entry.unix_mode() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o7777))
                    .map_err(VeyronError::Io)?;
            }
        }
    }

    if let Some(missing_name) = missing.iter().next() {
        return Err(VeyronError::Internal(format!(
            "Malformed archive: manifest `files` lists '{missing_name}' which is missing from the archive. Aborting."
        )));
    }

    Ok(())
}

/// Remove an installed plugin's directory from `plugin_dir()` and drop its
/// record from the state store.
///
/// This only deletes files on disk — it does not stop a running instance or
/// edit `config.yaml`. Callers should stop the plugin first if the kernel is
/// running it.
pub fn uninstall(slug: &str, tmp_dir: &Path) -> Result<(), VeyronError> {
    validate_slug(slug)?;
    let tracked = crate::marketplace::state::remove_record(tmp_dir, slug)?;
    let dest = plugin_dir(tmp_dir).join(slug);

    if dest.exists() {
        fs::remove_dir_all(&dest).map_err(VeyronError::Io)?;
        println!("✓ Removed {slug} from {}/", dest.display());
        return Ok(());
    }

    // dir already gone — the state record is what makes this a success (R10-02)
    if tracked.is_some() {
        println!(
            "⚠ '{slug}' dir was already missing at {} — removed it from the install state.",
            dest.display()
        );
        return Ok(());
    }

    Err(VeyronError::PluginNotFound(format!(
        "'{slug}' is not installed at {}",
        dest.display()
    )))
}

/// Reject slugs that could escape `plugin_dir`/`plugins_dir` via path
/// traversal (`../`, `/`, `..`, empty). Applied to every path a slug is
/// joined into — the CLI `remove` target is operator input, and registry
/// slugs are remote-controlled, so neither may shape a filesystem path.
fn validate_slug(slug: &str) -> Result<(), VeyronError> {
    let ok = !slug.is_empty()
        && slug.len() <= 64
        && slug != "."
        && slug != ".."
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
    if ok {
        Ok(())
    } else {
        Err(VeyronError::InvalidPluginId(format!(
            "invalid slug '{slug}': only [A-Za-z0-9._-], no path separators"
        )))
    }
}

/// Write a per-plugin drop-in config `plugins_dir/<slug>.yaml` (R10-01) so the
/// kernel auto-spawns the installed plugin. Returns whether the file was
/// written — an existing file (operator-tuned, or a planted symlink) is left
/// untouched. `create_new` (O_CREAT|O_EXCL) never follows a symlink, so a
/// pre-planted link cannot redirect the write onto an arbitrary target
/// (AUDIT M-09 class).
pub fn write_plugin_config(
    plugins_dir: &Path,
    installed: &InstalledPlugin,
) -> Result<bool, VeyronError> {
    validate_slug(&installed.slug)?;
    fs::create_dir_all(plugins_dir).map_err(VeyronError::Io)?;
    let path = plugins_dir.join(format!("{}.yaml", installed.slug));

    // network plugin opens outbound sockets — the sandbox netns would block
    // its egress, so the generated entry must not suggest it
    let sandbox = installed.plugin_id != "network";

    let body =
        format!(
        "# auto-spawn entry written by `vyn plugin install {}` — edit to tune, remove to disable\n\
         id: {}\n\
         binary: {}\n\
         restart: on-failure\n\
         max_restarts: 5\n\
         sandbox: {}\n",
        installed.slug, installed.plugin_id, installed.binary_path.display(), sandbox
    );
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            use std::io::Write;
            f.write_all(body.as_bytes()).map_err(VeyronError::Io)?;
            Ok(true)
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(VeyronError::Io(e)),
    }
}

/// Delete the drop-in config for `slug`. Returns whether a file was removed.
pub fn remove_plugin_config(plugins_dir: &Path, slug: &str) -> Result<bool, VeyronError> {
    validate_slug(slug)?;
    let path = plugins_dir.join(format!("{slug}.yaml"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(VeyronError::Io(e)),
    }
}

pub fn validate_manifest(
    path: &Path,
    kernel_ver: &Version,
) -> Result<InstallManifest, VeyronError> {
    let data = fs::read_to_string(path).map_err(|_| {
        VeyronError::Internal("Invalid plugin.json: file not found or unreadable.".into())
    })?;

    let manifest: InstallManifest = serde_json::from_str(&data)
        .map_err(|e| VeyronError::Internal(format!("Invalid plugin.json: {e}")))?;

    let compat_entry = RegistryEntry {
        id: String::new(),
        slug: manifest.plugin_id.clone(),
        name: String::new(),
        description: String::new(),
        version: manifest.version.clone(),
        permissions: manifest.permissions.clone(),
        archive_url: String::new(),
        source_url: String::new(),
        sha256: String::new(),
        min_kernel_version: manifest.kernel_compatibility_range.min.clone(),
        max_kernel_version: manifest.kernel_compatibility_range.max.clone(),
        signature: String::new(),
        status: "stable".into(),
    };
    check_kernel_compatibility(&compat_entry, kernel_ver)?;

    let known = known_permissions();
    for perm in &manifest.permissions {
        if !known.contains(perm) {
            return Err(VeyronError::Internal(format!(
                "Plugin '{}' declares unknown permission '{perm}'.",
                manifest.plugin_id
            )));
        }
    }

    // Manifest v2: every per-action `permission` must resolve to a known
    // permission. Fail-closed — an unknown per-action permission refuses the
    // whole plugin, so a typo can't silently downgrade the action to
    // unrestricted.
    if let Some(actions) = &manifest.actions {
        for spec in actions {
            if let ActionSpec::V2(spec) = spec {
                if let Some(p) = &spec.permission {
                    if crate::auth::permissions::resolve_permission(p).is_none() {
                        return Err(VeyronError::Internal(format!(
                            "Plugin '{}' declares unknown action permission '{}' for action '{}'.",
                            manifest.plugin_id, p, spec.name
                        )));
                    }
                }
            }
        }
    }

    Ok(manifest)
}
