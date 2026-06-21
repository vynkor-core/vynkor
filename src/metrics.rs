use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the Prometheus recorder once. Safe to call multiple times (idempotent).
pub fn init_metrics() {
    HANDLE.get_or_init(|| {
        PrometheusBuilder::new()
            .install_recorder()
            .expect("failed to install Prometheus recorder")
    });
}

/// Render current metrics in Prometheus text format.
pub fn render() -> String {
    HANDLE.get().map(|h| h.render()).unwrap_or_default()
}
