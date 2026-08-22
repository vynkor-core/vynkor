use super::helpers::{run_soak, SoakStats};

/// Stability soak: hammers the kernel with concurrent ping loops for
/// `VYN_SOAK_SECS` seconds (default 5 in CI, set to 86400 for 24 h soak).
///
/// Asserts:
///   - at least one ping was sent
///   - ping success rate ≥ 99 %
///   - no tasks panicked
#[tokio::test]
async fn kernel_remains_stable_under_sustained_load() {
    let secs = std::env::var("VYN_SOAK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(5);

    let stats: SoakStats = run_soak("/tmp/vynkor_soak.sock", 19299, secs).await;

    assert!(stats.total_pings > 0, "soak produced no pings in {secs}s");
    assert_eq!(stats.panics, 0, "kernel task panicked during soak");

    let success_rate = stats.successful_pings as f64 / stats.total_pings as f64;
    assert!(
        success_rate > 0.99,
        "ping success rate below 99%: {}/{} ({:.1}%)",
        stats.successful_pings,
        stats.total_pings,
        success_rate * 100.0,
    );
}
