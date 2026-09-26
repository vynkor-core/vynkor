//! Per-plugin frame-MAC secret. A local plugin must not hold the master
//! `jwt_secret`: that secret also signs JWTs, and registration trusts JWT
//! `permissions` claims, so holding it means minting any permission. Instead
//! the kernel hands each spawned plugin HKDF(master, plugin_id) as
//! `VYN_JWT_SECRET`, and uses the same bytes as the frame-MAC IKM for that
//! plugin_id. The value is hex text because SDKs feed the env string's bytes
//! straight into `derive_session_key`.

use hkdf::Hkdf;
use sha2::Sha256;

const SALT: &[u8] = b"vynkor-plugin-mac-v1";

pub fn plugin_mac_secret(master: &[u8], plugin_id: &str) -> String {
    let hk = Hkdf::<Sha256>::new(Some(SALT), master);
    let mut okm = [0u8; 32];
    hk.expand(plugin_id.as_bytes(), &mut okm)
        .expect("HKDF expand of 32 bytes is always valid");
    okm.iter().map(|b| format!("{b:02x}")).collect()
}
