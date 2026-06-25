use tracing_subscriber::{fmt, EnvFilter};

/// Initialize tracing. `RUST_LOG` (if set) wins; otherwise fall back to the
/// kernel's configured log level (`config.log_level` / `--debug`).
pub fn init(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .init();
}
