use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const SESSION_NONCE_LEN: usize = 16;
pub const MAC_TAG_LEN: usize = 32;

type HmacSha256 = Hmac<Sha256>;

/// Derive the per-connection MAC key. Inputs are known to both the kernel and
/// the plugin: the shared `jwt_secret`, the per-registration `nonce` from the
/// ack, and the registered `plugin_id`.
pub fn derive_session_key(jwt_secret: &[u8], nonce: &[u8], plugin_id: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(nonce), jwt_secret);
    let mut info = b"veyron-frame-mac-v1|".to_vec();
    info.extend_from_slice(plugin_id.as_bytes());
    let mut okm = [0u8; 32];
    // HKDF-SHA256 expand of 32 bytes never exceeds the length limit -> safe.
    hk.expand(&info, &mut okm)
        .expect("HKDF expand of 32 bytes is always valid");
    okm
}

/// HMAC-SHA256 over `header || payload`.
pub fn compute_tag(key: &[u8; 32], header: &[u8], payload: &[u8]) -> [u8; MAC_TAG_LEN] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(header);
    mac.update(payload);
    let out = mac.finalize().into_bytes();
    let mut tag = [0u8; MAC_TAG_LEN];
    tag.copy_from_slice(&out);
    tag
}

/// Constant-time verification of a tag over `header || payload`.
pub fn verify_tag(key: &[u8; 32], header: &[u8], payload: &[u8], tag: &[u8]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(header);
    mac.update(payload);
    mac.verify_slice(tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_is_deterministic_and_input_sensitive() {
        let k1 = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "plugin-a");
        let k2 = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "plugin-a");
        assert_eq!(k1, k2, "same inputs -> same key");

        assert_ne!(
            k1,
            derive_session_key(b"secret", b"nonce-bbbbbbbbbb", "plugin-a")
        );
        assert_ne!(
            k1,
            derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "plugin-b")
        );
        assert_ne!(
            k1,
            derive_session_key(b"other!", b"nonce-aaaaaaaaaa", "plugin-a")
        );
    }

    #[test]
    fn tag_round_trips() {
        let key = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "p");
        let header = [1u8; 44];
        let payload = b"hello".to_vec();
        let tag = compute_tag(&key, &header, &payload);
        assert!(verify_tag(&key, &header, &payload, &tag));
    }

    #[test]
    fn tampering_is_detected() {
        let key = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "p");
        let header = [1u8; 44];
        let payload = b"hello".to_vec();
        let tag = compute_tag(&key, &header, &payload);

        // flipped payload
        assert!(!verify_tag(&key, &header, b"hellp", &tag));
        // flipped header byte
        let mut h2 = header;
        h2[8] ^= 0xff;
        assert!(!verify_tag(&key, &h2, &payload, &tag));
        // flipped tag byte
        let mut t2 = tag;
        t2[0] ^= 0xff;
        assert!(!verify_tag(&key, &header, &payload, &t2));
        // wrong key
        let other = derive_session_key(b"secret", b"nonce-bbbbbbbbbb", "p");
        assert!(!verify_tag(&other, &header, &payload, &tag));
    }

    #[test]
    fn verify_rejects_wrong_length_tag() {
        let key = derive_session_key(b"secret", b"nonce-aaaaaaaaaa", "p");
        assert!(!verify_tag(&key, &[0u8; 44], b"x", &[0u8; 8]));
    }
}
