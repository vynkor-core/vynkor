use std::fs;
use std::io;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use indicatif::{ProgressBar, ProgressStyle};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::marketplace::registry::{
    check_kernel_compatibility, verify_entry_signature, RegistryEntry,
};
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
    pub actions: Option<Vec<String>>,
    /// Plugin IDs that must be loaded and registered before this plugin starts.
    #[serde(default)]
    pub requires: Vec<String>,
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

    // Step 2 — Kernel version compatibility check
    let kernel_ver = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| VeyronError::Internal(format!("parse kernel version: {e}")))?;

    check_kernel_compatibility(entry, &kernel_ver)
        .map_err(|e| VeyronError::Incompatible(format!("{e}. Upgrade Veyron first.")))?;

    let plugin_base = plugin_dir(tmp_dir);
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

    if let Err(e) = extract_zip(
        &archive_path,
        &extract_dir,
        max_extracted_bytes,
        max_archive_entries,
    ) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(e);
    }

    // Step 6 — Atomic move to plugin directory
    let base = plugin_base;
    if let Err(e) = fs::create_dir_all(&base) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(VeyronError::Io(e));
    }

    let dest = base.join(&entry.slug);
    let bak = base.join(format!("{}.bak", entry.slug));
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

pub fn extract_zip(
    archive: &Path,
    dest: &Path,
    max_extracted_bytes: u64,
    max_archive_entries: usize,
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

    Ok(())
}

/// Remove an installed plugin's directory from `plugin_dir()`.
///
/// This only deletes files on disk — it does not stop a running instance or
/// edit `config.yaml`. Callers should stop the plugin first if the kernel is
/// running it.
pub fn uninstall(slug: &str, tmp_dir: &Path) -> Result<(), VeyronError> {
    validate_slug(slug)?;
    let dest = plugin_dir(tmp_dir).join(slug);

    if !dest.exists() {
        return Err(VeyronError::PluginNotFound(format!(
            "'{slug}' is not installed at {}",
            dest.display()
        )));
    }

    fs::remove_dir_all(&dest).map_err(VeyronError::Io)?;
    println!("✓ Removed {slug} from {}/", dest.display());
    Ok(())
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

    Ok(manifest)
}
