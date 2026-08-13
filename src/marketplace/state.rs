use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::utils::errors::VeyronError;

/// One recorded install in `installed.json` — the explicit state store that
/// replaces filesystem-sniffing `~/.local/lib/veyron/plugins/<slug>` (R10-02).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledEntry {
    pub slug: String,
    pub version: String,
    /// sha256 of the archive that produced this install.
    pub sha256: String,
    /// Unix epoch seconds when the install completed.
    pub installed_at: u64,
    /// Registry URL this plugin was installed from.
    pub source_url: String,
}

/// The on-disk shape of `installed.json`. Serialized with pretty JSON so an
/// operator can read it; unknown future fields are ignored by serde.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledState {
    pub entries: Vec<InstalledEntry>,
}

impl InstalledState {
    pub fn get(&self, slug: &str) -> Option<&InstalledEntry> {
        self.entries.iter().find(|e| e.slug == slug)
    }

    /// Insert or replace the entry for `slug` — one record per plugin.
    fn upsert(&mut self, entry: InstalledEntry) {
        match self.entries.iter_mut().find(|e| e.slug == entry.slug) {
            Some(slot) => *slot = entry,
            None => self.entries.push(entry),
        }
    }

    /// Remove the entry for `slug`, returning it if it existed.
    fn remove(&mut self, slug: &str) -> Option<InstalledEntry> {
        let idx = self.entries.iter().position(|e| e.slug == slug)?;
        Some(self.entries.remove(idx))
    }
}

/// Directory holding `installed.json`. `VEYRON_STATE_DIR` overrides for
/// relocatable setups and tests; otherwise the XDG data dir, mirroring how
/// `plugin_dir()` resolves `VEYRON_PLUGIN_DIR` then `$HOME/.local/lib`.
/// `tmp_dir` is the fallback base when `$HOME` is unset (same convention as
/// `plugin_dir`/registry cache — never the shared `/tmp`, AUDIT M-09).
pub fn state_dir(tmp_dir: &Path) -> PathBuf {
    std::env::var("VEYRON_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("XDG_DATA_HOME")
                .map(|d| PathBuf::from(d).join("veyron"))
                .unwrap_or_else(|_| {
                    dirs_home(tmp_dir)
                        .join(".local")
                        .join("share")
                        .join("veyron")
                })
        })
}

fn state_path(tmp_dir: &Path) -> PathBuf {
    state_dir(tmp_dir).join("installed.json")
}

fn dirs_home(tmp_dir: &Path) -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| tmp_dir.to_path_buf())
}

/// Read the install ledger. A missing file is an empty state; a corrupt file
/// is logged and treated as empty — a broken ledger must never block the CLI
/// (it self-heals on the next install/remove).
pub fn load_state(tmp_dir: &Path) -> InstalledState {
    let path = state_path(tmp_dir);
    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_else(|e| {
            tracing::warn!(
                "corrupt installed state at {}, starting empty: {e}",
                path.display()
            );
            InstalledState::default()
        }),
        Err(_) => InstalledState::default(),
    }
}

/// Write the ledger atomically (temp + rename in the same dir), so a crash
/// mid-write can never leave a half-written `installed.json`.
pub fn save_state(tmp_dir: &Path, state: &InstalledState) -> Result<(), VeyronError> {
    let dir = state_dir(tmp_dir);
    fs::create_dir_all(&dir).map_err(VeyronError::Io)?;
    let path = dir.join("installed.json");
    let tmp = dir.join(".installed.json.tmp");
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| VeyronError::CacheError(format!("serialize installed state: {e}")))?;
    fs::write(&tmp, json).map_err(VeyronError::Io)?;
    fs::rename(&tmp, &path).map_err(VeyronError::Io)?;
    Ok(())
}

/// Record a completed install (upsert by slug). Best-effort callers may treat
/// an error as fatal — a plugin on disk but untracked is exactly the drift
/// this store exists to prevent.
pub fn record_install(tmp_dir: &Path, entry: InstalledEntry) -> Result<(), VeyronError> {
    let mut state = load_state(tmp_dir);
    state.upsert(entry);
    save_state(tmp_dir, &state)
}

/// Drop the entry for `slug`, returning it if it was tracked.
pub fn remove_record(tmp_dir: &Path, slug: &str) -> Result<Option<InstalledEntry>, VeyronError> {
    let mut state = load_state(tmp_dir);
    let removed = state.remove(slug);
    if removed.is_some() {
        save_state(tmp_dir, &state)?;
    }
    Ok(removed)
}

/// Format a unix-epoch timestamp as `YYYY-MM-DD HH:MM:SS` (UTC), dependency-
/// free — chrono would be overkill for one column in the CLI table.
pub fn format_ts(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;

    // civil-from-days (Howard Hinnant's algorithm) — valid for the whole
    // i64 day range, plenty for a 64-bit epoch.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}
