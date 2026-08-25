//! E-01: per-device credentials issued by the host.
//!
//! One row per paired device: a random 32-byte `device_secret` (the frame-MAC
//! input keying material) plus lifecycle metadata. The secret never leaves the
//! host except once, inside the pair payload (QR/link = trusted physical
//! channel). At rest the row is AES-256-GCM ciphertext under a key derived
//! once from jwt_secret — data_dir theft alone reveals nothing, and the raw
//! secret must be recoverable because `derive_session_key` consumes it at
//! every registration.
//!
//! The store is re-read from disk on every check: registrations/upgrades are
//! rare events, and re-reading makes `vyn device revoke` from a separate CLI
//! process take effect on a running kernel with zero IPC.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::utils::errors::VynkorError;

const STORE_FILE: &str = "devices.json";
const HKDF_SALT: &[u8] = b"vynkor-device-store-v1";
const HKDF_INFO: &[u8] = b"aes-256-gcm-key";
const SECRET_AAD: &[u8] = b"vynkor-device-secret";
const NONCE_LEN: usize = 12;
/// 32 bytes hex-encoded — what the pair payload carries.
pub const DEVICE_SECRET_HEX_LEN: usize = 64;

/// Credential lifecycle. `Expired` is computed, not stored: a row past its
/// `expires_at` behaves exactly like a revoked one at every gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    Active,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub device_id: String,
    pub name: String,
    /// unix secs of pairing
    pub created_at: u64,
    /// unix secs when the credential stops working (matches the token TTL)
    pub expires_at: u64,
    pub revoked: bool,
    /// AES-256-GCM(nonce(12) || ciphertext||tag) of the hex secret
    secret_enc: Vec<u8>,
}

impl DeviceRecord {
    pub fn status(&self, now_secs: u64) -> DeviceStatus {
        if self.revoked {
            DeviceStatus::Revoked
        } else if now_secs >= self.expires_at {
            DeviceStatus::Expired
        } else {
            DeviceStatus::Active
        }
    }
}

pub struct DeviceStore {
    path: PathBuf,
    /// HKDF(jwt_master_secret) — the master itself is not retained.
    key: [u8; 32],
}

impl DeviceStore {
    /// Rotation intentionally locks old rows out (re-pair), see RFC E-01 Q3.
    pub fn new(data_dir: &Path, jwt_master_secret: &str) -> Self {
        Self {
            path: data_dir.join(STORE_FILE),
            key: Self::derive_key(jwt_master_secret),
        }
    }

    fn derive_key(jwt_secret: &str) -> [u8; 32] {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), jwt_secret.as_bytes());
        let mut okm = [0u8; 32];
        hk.expand(HKDF_INFO, &mut okm)
            .expect("HKDF expand of 32 bytes is always valid");
        okm
    }

    fn seal(&self, secret_hex: &str) -> Result<Vec<u8>, VynkorError> {
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let mut nonce_bytes = [0u8; NONCE_LEN];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let ct = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: secret_hex.as_bytes(),
                    aad: SECRET_AAD,
                },
            )
            .map_err(|_| VynkorError::Auth("device store encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    fn unseal(&self, enc: &[u8]) -> Result<String, VynkorError> {
        if enc.len() <= NONCE_LEN {
            return Err(VynkorError::Auth("corrupt device store entry".into()));
        }
        let (nonce_bytes, ct) = enc.split_at(NONCE_LEN);
        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&self.key));
        let pt = cipher
            .decrypt(
                Nonce::from_slice(nonce_bytes),
                Payload {
                    msg: ct,
                    aad: SECRET_AAD,
                },
            )
            .map_err(|_| {
                // wrong master secret or tampered row — both mean "cannot trust"
                VynkorError::Auth(
                    "device store decrypt failed — was jwt_secret rotated? re-pair devices \
                     (rm <data_dir>/devices.json)"
                        .into(),
                )
            })?;
        String::from_utf8(pt).map_err(|_| VynkorError::Auth("bad secret encoding".into()))
    }

    fn read_all(&self) -> Result<Vec<DeviceRecord>, VynkorError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(VynkorError::Io(e)),
        };
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<DeviceRecord> =
            serde_json::from_str(&raw).map_err(|e| VynkorError::Internal(e.to_string()))?;
        // fail loud on the first undecryptable row — silently dropping rows
        // would turn revocation into a no-op for exactly the compromised case
        for row in &rows {
            self.unseal(&row.secret_enc)?;
        }
        Ok(rows)
    }

    fn write_all(&self, rows: &[DeviceRecord]) -> Result<(), VynkorError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(VynkorError::Io)?;
        }
        let json =
            serde_json::to_string_pretty(rows).map_err(|e| VynkorError::Internal(e.to_string()))?;
        // temp+rename so a crash mid-write can't shred existing credentials;
        // 0600 before the data lands (secrets at rest)
        let tmp = self.path.with_extension("json.tmp");
        {
            let file = fs::File::create(&tmp).map_err(VynkorError::Io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(VynkorError::Io)?;
            }
            let mut w = file;
            w.write_all(json.as_bytes()).map_err(VynkorError::Io)?;
            w.flush().map_err(VynkorError::Io)?;
        }
        fs::rename(&tmp, &self.path).map_err(VynkorError::Io)?;
        Ok(())
    }

    /// Issue or rotate the credential for `device_id`. The plaintext secret is
    /// returned exactly once for the pair payload.
    pub fn issue(&self, device_id: &str, name: &str, ttl_secs: u64) -> Result<String, VynkorError> {
        let now = now_secs();
        let secret_hex = random_device_secret_hex();
        let mut rows = self.read_all()?;
        let enc = self.seal(&secret_hex)?;
        match rows.iter_mut().find(|r| r.device_id == device_id) {
            Some(row) => {
                // re-pair rotates: fresh secret, fresh expiry, un-revoked
                row.name = name.to_string();
                row.created_at = now;
                row.expires_at = now + ttl_secs;
                row.revoked = false;
                row.secret_enc = enc;
            }
            None => rows.push(DeviceRecord {
                device_id: device_id.to_string(),
                name: name.to_string(),
                created_at: now,
                expires_at: now + ttl_secs,
                revoked: false,
                secret_enc: enc,
            }),
        }
        self.write_all(&rows)?;
        Ok(secret_hex)
    }

    pub fn get(&self, device_id: &str) -> Result<Option<(DeviceRecord, String)>, VynkorError> {
        let rows = self.read_all()?;
        Ok(rows
            .into_iter()
            .find(|r| r.device_id == device_id)
            .map(|row| {
                let secret = self
                    .unseal(&row.secret_enc)
                    .expect("read_all already verified decryptability");
                (row, secret)
            }))
    }

    pub fn list(&self) -> Result<Vec<DeviceRecord>, VynkorError> {
        self.read_all()
    }

    pub fn set_revoked(&self, device_id: &str, revoked: bool) -> Result<bool, VynkorError> {
        let mut rows = self.read_all()?;
        let mut changed = false;
        for row in rows.iter_mut().filter(|r| r.device_id == device_id) {
            row.revoked = revoked;
            changed = true;
        }
        if changed {
            self.write_all(&rows)?;
        }
        Ok(changed)
    }

    pub fn remove(&self, device_id: &str) -> Result<bool, VynkorError> {
        let mut rows = self.read_all()?;
        let before = rows.len();
        rows.retain(|r| r.device_id != device_id);
        if rows.len() != before {
            self.write_all(&rows)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// The gate used by registration and WS upgrade: `Ok(Some(secret))` only
    /// for an active, unexpired credential. Missing → `Ok(None)`; revoked/
    /// expired → `Err`.
    pub fn active_secret(&self, device_id: &str) -> Result<Option<String>, VynkorError> {
        match self.get(device_id)? {
            None => Ok(None),
            Some((row, secret)) => match row.status(now_secs()) {
                DeviceStatus::Active => Ok(Some(secret)),
                DeviceStatus::Revoked => Err(VynkorError::Auth(format!(
                    "device '{device_id}' is revoked — re-pair to restore access"
                ))),
                DeviceStatus::Expired => Err(VynkorError::Auth(format!(
                    "device '{device_id}' credential expired — re-pair"
                ))),
            },
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// 32 CSPRNG bytes as 64 lowercase hex chars.
pub fn random_device_secret_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const MASTER: &str = "master-secret-master-secret-master!!";

    fn tmp_store() -> (tempfile::TempDir, DeviceStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = DeviceStore::new(dir.path(), MASTER);
        (dir, store)
    }

    #[test]
    fn issue_and_recover_round_trip() {
        let (_dir, store) = tmp_store();
        let secret = store.issue("dev-1", "phone", 3600).unwrap();
        assert_eq!(secret.len(), DEVICE_SECRET_HEX_LEN);

        let (row, recovered) = store.get("dev-1").unwrap().unwrap();
        assert_eq!(recovered, secret);
        assert_eq!(row.status(row.created_at), DeviceStatus::Active);
        assert_eq!(row.expires_at - row.created_at, 3600);
    }

    #[test]
    fn secrets_are_never_plaintext_on_disk() {
        let (_dir, store) = tmp_store();
        let secret = store.issue("dev-1", "phone", 60).unwrap();
        let raw = std::fs::read_to_string(store.path.clone()).unwrap();
        assert!(
            !raw.contains(&secret),
            "plaintext device_secret must not land in the store file"
        );
        assert!(!raw.contains(MASTER));
    }

    #[test]
    fn wrong_master_secret_cannot_decrypt() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = DeviceStore::new(dir.path(), MASTER);
            store.issue("dev-1", "phone", 60).unwrap();
        }
        let other = DeviceStore::new(dir.path(), "a-different-master-secret-that-is-long!!");
        assert!(other.get("dev-1").is_err());
        // and the gate fails closed
        assert!(other.active_secret("dev-1").is_err());
    }

    #[test]
    fn revoke_blocks_then_unrevoke_restores() {
        let (_dir, store) = tmp_store();
        store.issue("dev-1", "phone", 3600).unwrap();
        assert!(store.active_secret("dev-1").unwrap().is_some());

        assert!(store.set_revoked("dev-1", true).unwrap());
        let err = store.active_secret("dev-1").unwrap_err();
        assert!(err.to_string().contains("revoked"));

        assert!(store.set_revoked("dev-1", false).unwrap());
        assert!(store.active_secret("dev-1").unwrap().is_some());
    }

    #[test]
    fn expired_rows_are_rejected_like_revoked_ones() {
        let (_dir, store) = tmp_store();
        // pure lifecycle math first — no wall clock involved
        store.issue("dev-1", "phone", 60).unwrap();
        let (row, _) = store.get("dev-1").unwrap().unwrap();
        assert_eq!(row.status(0), DeviceStatus::Active);
        assert_eq!(row.status(u64::MAX), DeviceStatus::Expired);

        // gate-level: wait out a 1-second TTL
        store.issue("dev-2", "phone", 1).unwrap();
        std::thread::sleep(Duration::from_millis(1300));
        let err = store.active_secret("dev-2").unwrap_err();
        assert!(err.to_string().contains("expired"));
    }

    #[test]
    fn unknown_device_is_none_not_error() {
        let (_dir, store) = tmp_store();
        assert!(store.active_secret("ghost").unwrap().is_none());
    }

    #[test]
    fn remove_drops_the_row() {
        let (_dir, store) = tmp_store();
        store.issue("dev-1", "phone", 60).unwrap();
        assert!(store.remove("dev-1").unwrap());
        assert!(store.get("dev-1").unwrap().is_none());
        assert!(!store.remove("dev-1").unwrap());
    }

    #[test]
    fn reissue_rotates_secret_and_clears_revocation() {
        let (_dir, store) = tmp_store();
        let s1 = store.issue("dev-1", "phone", 60).unwrap();
        store.set_revoked("dev-1", true).unwrap();

        let s2 = store.issue("dev-1", "phone-2", 120).unwrap();
        assert_ne!(s1, s2, "re-pair must mint a fresh secret");
        let (row, secret) = store.get("dev-1").unwrap().unwrap();
        assert_eq!(secret, s2);
        assert_eq!(row.name, "phone-2");
        assert_eq!(row.status(row.created_at), DeviceStatus::Active);
    }

    #[test]
    fn multiple_rows_coexist() {
        let (_dir, store) = tmp_store();
        store.issue("dev-a", "a", 60).unwrap();
        store.issue("dev-b", "b", 60).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
        store.set_revoked("dev-a", true).unwrap();
        assert!(store.active_secret("dev-b").unwrap().is_some());
        assert!(store.active_secret("dev-a").is_err());
    }
}
