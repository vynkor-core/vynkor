use std::io;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use tracing::{info, warn};

#[cfg(target_os = "linux")]
use landlock::{AccessFs, BitFlags, PathBeneath, PathFd};

/// Ceiling on a sandboxed plugin's filesystem access (config `max_fs_access`).
/// Only enforced when `sandbox: true` on a Landlock-capable kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FsAccessMode {
    /// No filesystem restriction (status quo).
    #[default]
    Full,
    /// Exec requirements + `readonly_paths` (read-only) + `writable_paths`.
    ReadOnly,
    /// Exec requirements + `writable_paths` only.
    None,
}

impl FsAccessMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            FsAccessMode::Full => "full",
            FsAccessMode::ReadOnly => "read-only",
            FsAccessMode::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "full" => Some(FsAccessMode::Full),
            "read-only" | "readonly" => Some(FsAccessMode::ReadOnly),
            "none" => Some(FsAccessMode::None),
            _ => None,
        }
    }
}

/// Filesystem restriction to enforce on one plugin, passed shim-ward via env.
pub struct FsRestriction {
    pub mode: FsAccessMode,
    pub readonly_paths: Vec<PathBuf>,
    pub writable_paths: Vec<PathBuf>,
}

const MAX_FS_ACCESS_ENV: &str = "VEYRON_MAX_FS_ACCESS";
const RO_PATHS_ENV: &str = "VEYRON_RO_PATHS";
const RW_PATHS_ENV: &str = "VEYRON_RW_PATHS";
/// Path list separator inside the env vars. Unit separator cannot appear in a
/// path, unlike `:`.
const PATH_SEP: char = '\u{1f}';

/// Serialize a path list for the shim env vars. Split with
/// [`split_paths_env`].
pub fn join_paths_env(paths: &[PathBuf]) -> String {
    let sep = PATH_SEP.to_string();
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<String>>()
        .join(&sep)
}

/// Parse the restriction the supervisor sent via env, or None when the plugin
/// is not filesystem-restricted (`max_fs_access: full` or unset).
#[cfg(target_os = "linux")]
pub fn from_env() -> Option<FsRestriction> {
    let mode = match std::env::var(MAX_FS_ACCESS_ENV) {
        Ok(v) => match FsAccessMode::parse(&v) {
            Some(m) => m,
            None => {
                warn!(value = %v, "unknown max_fs_access in shim env — no filesystem restriction");
                return None;
            }
        },
        Err(_) => return None,
    };
    if mode == FsAccessMode::Full {
        return None;
    }
    Some(FsRestriction {
        mode,
        readonly_paths: parse_paths_env(RO_PATHS_ENV),
        writable_paths: parse_paths_env(RW_PATHS_ENV),
    })
}

/// Build and enforce the Landlock ruleset. Runs in the plugin's `pre_exec`
/// (single-threaded child before exec), so only the plugin and its descendants
/// are restricted — the shim itself stays unrestricted.
///
/// Fails closed: a ruleset that cannot be enforced at all (no Landlock on the
/// kernel) is an error, not a silent downgrade to unrestricted. Kernels that
/// support Landlock but not the newest access rights (e.g. v9 `ResolveUnix`)
/// still enforce the core filesystem rights; the crate's best-effort
/// compatibility drops exactly the unsupported bits.
#[cfg(target_os = "linux")]
pub fn apply(
    restriction: &FsRestriction,
    plugin_binary: &Path,
    socket_path: Option<&Path>,
) -> io::Result<()> {
    use landlock::{
        Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI,
    };

    let access_all = AccessFs::from_all(ABI::V9);
    let access_read = AccessFs::from_read(ABI::V9);
    let access_file_read = AccessFs::ReadFile | AccessFs::Execute;
    let access_file_write = AccessFs::from_file(ABI::V9);
    let access_socket = AccessFs::ResolveUnix;

    let mut created = Ruleset::default()
        .handle_access(access_all)
        .map_err(landlock_err)?
        .create()
        .map_err(landlock_err)?;

    // dynamic loading: ld.so and shared libraries (exec of the plugin binary
    // and any interpreter under its dir is covered by the binary-dir rule)
    for dir in default_system_read_dirs() {
        if let Some(rule) = rule_for(&dir, access_read, access_file_read)? {
            created = created.add_rule(rule).map_err(landlock_err)?;
        }
    }
    // the plugin's own directory: exec of the binary, reads of its plugin.json
    let binary = resolve_binary_path(plugin_binary);
    if let Some(dir) = binary.parent() {
        if !dir.as_os_str().is_empty() {
            if let Some(rule) = rule_for(dir, access_read, access_file_read)? {
                created = created.add_rule(rule).map_err(landlock_err)?;
            }
        }
    }
    // glibc's loader cache; a denied read just makes ld.so scan defaults, but
    // allowing it avoids the ambiguity
    if let Some(rule) = rule_for(Path::new("/etc/ld.so.cache"), access_read, access_file_read)? {
        created = created.add_rule(rule).map_err(landlock_err)?;
    }
    // connecting to the kernel's UDS requires resolving the socket path (ABI
    // v9); older kernels have no such check, and the rule drops harmlessly
    if let Some(sp) = socket_path {
        if let Some(rule) = rule_for(sp, access_socket.into(), access_socket.into())? {
            created = created.add_rule(rule).map_err(landlock_err)?;
        }
    }
    if restriction.mode == FsAccessMode::ReadOnly {
        for path in &restriction.readonly_paths {
            if let Some(rule) = rule_for(path, access_read, access_file_read)? {
                created = created.add_rule(rule).map_err(landlock_err)?;
            }
        }
    }
    for path in &restriction.writable_paths {
        if let Some(rule) = rule_for(path, access_all, access_file_write)? {
            created = created.add_rule(rule).map_err(landlock_err)?;
        }
    }

    let status = created.restrict_self().map_err(landlock_err)?;
    if status.ruleset == RulesetStatus::NotEnforced {
        return Err(io::Error::other(
            "Landlock not enforced — kernel too old or Landlock unavailable",
        ));
    }
    info!(
        ruleset = ?status.ruleset,
        "landlock filesystem restriction enforced"
    );
    Ok(())
}

/// System library dirs needed to exec any dynamically-linked plugin.
#[cfg(target_os = "linux")]
fn default_system_read_dirs() -> Vec<PathBuf> {
    ["/usr/lib", "/usr/lib64", "/lib", "/lib64"]
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

/// Resolve the plugin binary the way execvp would, so the binary-dir rule
/// covers the real on-disk location even when config names a bare command
/// (`python3`) or a relative path (`./target/release/foo`). Falls back to the
/// unresolved path; `rule_for` skips it if it still cannot be found.
#[cfg(target_os = "linux")]
fn resolve_binary_path(binary: &Path) -> PathBuf {
    if binary.is_absolute() {
        return binary.to_path_buf();
    }
    if binary.components().count() > 1 {
        return std::fs::canonicalize(binary).unwrap_or_else(|_| binary.to_path_buf());
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(binary);
            if candidate.exists() {
                return candidate;
            }
        }
    }
    binary.to_path_buf()
}

/// One path-beneath rule, or None when the path does not exist (Landlock rules
/// attach to real objects; a missing path would otherwise fail the spawn).
/// `dir_access` is granted to directories, `file_access` to regular files —
/// granting dir-only rights on a file is rejected by the kernel.
#[cfg(target_os = "linux")]
fn rule_for(
    path: &Path,
    dir_access: BitFlags<AccessFs>,
    file_access: BitFlags<AccessFs>,
) -> io::Result<Option<PathBeneath<PathFd>>> {
    if !path.exists() {
        warn!(path = %path.display(), "landlock path missing — skipping rule");
        return Ok(None);
    }
    let fd = PathFd::new(path).map_err(|e| io::Error::other(e.to_string()))?;
    let access = if path.is_dir() {
        dir_access
    } else {
        file_access
    };
    Ok(Some(PathBeneath::new(fd, access)))
}

#[cfg(target_os = "linux")]
fn landlock_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

fn parse_paths_env(name: &str) -> Vec<PathBuf> {
    std::env::var(name)
        .map(|v| {
            v.split(PATH_SEP)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use landlock::{Access, AccessFs, ABI};

    #[test]
    fn parses_documented_modes() {
        assert_eq!(FsAccessMode::parse("full"), Some(FsAccessMode::Full));
        assert_eq!(
            FsAccessMode::parse("read-only"),
            Some(FsAccessMode::ReadOnly)
        );
        assert_eq!(
            FsAccessMode::parse("readonly"),
            Some(FsAccessMode::ReadOnly)
        );
        assert_eq!(FsAccessMode::parse("none"), Some(FsAccessMode::None));
        assert_eq!(FsAccessMode::parse("everything"), None);
        assert_eq!(FsAccessMode::parse(""), None);
    }

    #[test]
    fn roundtrips_mode_str() {
        for mode in [
            FsAccessMode::Full,
            FsAccessMode::ReadOnly,
            FsAccessMode::None,
        ] {
            assert_eq!(FsAccessMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn join_and_split_path_env_roundtrip() {
        let paths = vec![PathBuf::from("/tmp/a"), PathBuf::from("/home/user/.veyron")];
        assert_eq!(parse_paths_env_v(&join_paths_env(&paths)), paths);
        assert!(parse_paths_env_v("").is_empty());
    }

    fn parse_paths_env_v(value: &str) -> Vec<PathBuf> {
        value
            .split(PATH_SEP)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect()
    }

    #[test]
    fn from_env_ignores_unset_and_full() {
        temp_env::with_vars(
            vec![
                (MAX_FS_ACCESS_ENV, None::<&str>),
                (RO_PATHS_ENV, None),
                (RW_PATHS_ENV, None),
            ],
            || {
                assert!(from_env().is_none());
            },
        );
        temp_env::with_vars(
            vec![
                (MAX_FS_ACCESS_ENV, Some("full")),
                (RO_PATHS_ENV, Some("/tmp")),
                (RW_PATHS_ENV, Some("/tmp")),
            ],
            || {
                assert!(from_env().is_none());
            },
        );
    }

    #[test]
    fn from_env_parses_restriction_and_paths() {
        temp_env::with_vars(
            vec![
                (MAX_FS_ACCESS_ENV, Some("read-only")),
                (RO_PATHS_ENV, Some("/tmp/ro")),
                (RW_PATHS_ENV, Some("/tmp/rw")),
            ],
            || {
                let restriction = from_env().expect("restriction must parse");
                assert_eq!(restriction.mode, FsAccessMode::ReadOnly);
                assert_eq!(restriction.readonly_paths, vec![PathBuf::from("/tmp/ro")]);
                assert_eq!(restriction.writable_paths, vec![PathBuf::from("/tmp/rw")]);
            },
        );
    }

    #[test]
    fn rule_access_rights_per_path_kind() {
        let access_read = AccessFs::from_read(ABI::V9);
        let access_all = AccessFs::from_all(ABI::V9);
        let file_read = AccessFs::ReadFile | AccessFs::Execute;
        let file_write = AccessFs::from_file(ABI::V9);
        let socket: BitFlags<AccessFs> = AccessFs::ResolveUnix.into();

        assert!(access_read.contains(AccessFs::ReadFile));
        assert!(access_read.contains(AccessFs::ReadDir));
        assert!(access_read.contains(AccessFs::Execute));
        assert!(!access_read.contains(AccessFs::WriteFile));
        assert!(!access_read.contains(AccessFs::MakeReg));

        assert!(access_all.contains(AccessFs::WriteFile));
        assert!(access_all.contains(AccessFs::MakeReg));
        assert!(access_all.contains(AccessFs::MakeDir));
        assert!(access_all.contains(AccessFs::RemoveFile));
        assert!(access_all.contains(AccessFs::ResolveUnix));

        // readonly file rules must not carry dir-only or write rights
        assert!(file_read.contains(AccessFs::ReadFile));
        assert!(file_read.contains(AccessFs::Execute));
        assert!(!file_read.contains(AccessFs::ReadDir));
        assert!(!file_read.contains(AccessFs::WriteFile));

        // writable file rules may carry write+truncate but not dir rights
        assert!(file_write.contains(AccessFs::WriteFile));
        assert!(file_write.contains(AccessFs::Truncate));
        assert!(!file_write.contains(AccessFs::ReadDir));
        assert!(!file_write.contains(AccessFs::MakeReg));

        assert!(socket.contains(AccessFs::ResolveUnix));
        assert!(!socket.contains(AccessFs::ReadFile));
        assert!(!socket.contains(AccessFs::WriteFile));
    }

    #[test]
    fn rule_for_skips_missing_paths() {
        let missing = PathBuf::from("/nonexistent/veyron-r9-03");
        let access_read = AccessFs::from_read(ABI::V9);
        assert!(rule_for(&missing, access_read, access_read)
            .unwrap()
            .is_none());

        let dir = std::env::temp_dir().join(format!("veyron-fsacc-dir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(rule_for(&dir, access_read, access_read).unwrap().is_some());
        let file = dir.join("f");
        std::fs::write(&file, b"x").unwrap();
        assert!(rule_for(&file, access_read, access_read).unwrap().is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn system_read_dirs_are_lib_dirs() {
        assert!(default_system_read_dirs().contains(&PathBuf::from("/usr/lib")));
        assert!(default_system_read_dirs().contains(&PathBuf::from("/lib64")));
    }

    #[test]
    fn resolves_bare_binary_via_path() {
        let resolved = resolve_binary_path(Path::new("sh"));
        assert!(
            resolved.is_absolute(),
            "PATH lookup must return an absolute path"
        );
        assert!(resolved.ends_with("sh"));
    }

    #[test]
    fn resolves_relative_dir_binary_via_canonicalize() {
        let dir = std::env::temp_dir().join(format!("veyron-fsacc-resolve-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let exe = sub.join("bin");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        let relative = std::path::Path::new("sub/bin");
        let original_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let resolved = resolve_binary_path(relative);
        std::env::set_current_dir(&original_cwd).unwrap();
        assert_eq!(resolved, exe);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn leaves_absolute_and_unfindable_paths_alone() {
        assert_eq!(
            resolve_binary_path(Path::new("/usr/bin/python3")),
            PathBuf::from("/usr/bin/python3")
        );
        assert_eq!(
            resolve_binary_path(Path::new("/nonexistent/veyron-r9-03-bin")),
            PathBuf::from("/nonexistent/veyron-r9-03-bin")
        );
    }
}
