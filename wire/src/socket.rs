use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

/// Resolves the same private runtime directory the kernel uses, so SDKs
/// pick the identical default socket location when `VEYRON_SOCKET_PATH`
/// is unset. Order: `$XDG_RUNTIME_DIR`, `/run/user/<uid>`, `~/.veyron/run`.
pub fn default_private_dir() -> Option<PathBuf> {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(runtime_dir));
    }

    let uid = nix::unistd::Uid::current().as_raw();
    let run_user_dir = PathBuf::from(format!("/run/user/{uid}"));
    if run_user_dir.is_dir() {
        return Some(run_user_dir);
    }

    if let Ok(home) = std::env::var("HOME") {
        let dir = PathBuf::from(home).join(".veyron").join("run");
        if std::fs::create_dir_all(&dir).is_ok()
            && std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok()
        {
            return Some(dir);
        }
    }

    None
}

pub fn default_socket_path() -> String {
    default_private_dir()
        .map(|dir| dir.join("veyron.sock").to_string_lossy().to_string())
        .unwrap_or_default()
}
