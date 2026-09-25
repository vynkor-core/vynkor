use vynkor::auth::plugin_key::plugin_mac_secret;

const MASTER: &[u8] = b"unit-test-master-secret-at-least-32-bytes";
const KAT_K_P: &str = "c22d6e2a62c9c0d1b5a47bcf98de02eb4ce12af214d672506ec2dbebf84beb54";

#[test]
fn derived_secret_is_64_lowercase_hex() {
    let s = plugin_mac_secret(MASTER, "telegram");
    assert_eq!(s.len(), 64);
    assert!(
        s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "{s}"
    );
}

#[test]
fn derived_secret_is_deterministic() {
    assert_eq!(
        plugin_mac_secret(MASTER, "agent"),
        plugin_mac_secret(MASTER, "agent")
    );
}

#[test]
fn derived_secret_differs_per_plugin() {
    assert_ne!(
        plugin_mac_secret(MASTER, "agent"),
        plugin_mac_secret(MASTER, "telegram")
    );
}

#[test]
fn derived_secret_differs_per_master() {
    assert_ne!(
        plugin_mac_secret(MASTER, "agent"),
        plugin_mac_secret(b"another-master-secret-at-least-32-bytes!!", "agent")
    );
}

#[test]
fn derived_secret_never_equals_or_contains_master() {
    let s = plugin_mac_secret(MASTER, "agent");
    assert!(!s.as_bytes().windows(MASTER.len()).any(|w| w == MASTER));
}

#[test]
fn derived_secret_known_answer() {
    // Pins the constants (salt "vynkor-plugin-mac-v1", info = plugin_id) so a
    // silent change breaks every deployed plugin loudly here instead.
    assert_eq!(plugin_mac_secret(b"k", "p"), KAT_K_P);
}
