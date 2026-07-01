use std::sync::OnceLock;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

type FilterHandle = reload::Handle<EnvFilter, Registry>;

static LOG_FILTER_HANDLE: OnceLock<FilterHandle> = OnceLock::new();

/// Initialize tracing with a reloadable log filter. `RUST_LOG` (if set) wins;
/// otherwise falls back to the kernel's configured log level.
/// Set `LOG_FORMAT=json` for structured JSON output.
/// When the `otel` feature is compiled in and `OTEL_EXPORTER_OTLP_ENDPOINT` env var is
/// set, an OTLP/gRPC exporter is also installed as an additional subscriber layer.
///
/// Layer ordering: reload → fmt → [otel when feature+env enabled].
/// `reload::Layer<EnvFilter, Registry>` is bound to `Registry` as S and must be first.
/// The OTel layer is generic over S and is created inline in each branch so the
/// compiler infers the correct S per branch without type-annotation conflicts.
pub fn init(level: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let (reloadable, handle) = reload::Layer::new(filter);
    LOG_FILTER_HANDLE.set(handle).ok();

    let json = std::env::var("LOG_FORMAT").as_deref() == Ok("json");

    #[cfg(feature = "otel")]
    if let Some(tracer) = setup_otel_tracer() {
        // Create the OTel layer inside each branch so the compiler infers S separately.
        // OpenTelemetryLayer<S, T> is generic; `T` = Tracer which is Clone.
        if json {
            let otel = tracing_opentelemetry::layer().with_tracer(tracer);
            Registry::default()
                .with(reloadable)
                .with(fmt::layer().json().with_target(true).with_file(true).with_line_number(true))
                .with(otel)
                .init();
        } else {
            let otel = tracing_opentelemetry::layer().with_tracer(tracer);
            Registry::default()
                .with(reloadable)
                .with(fmt::layer().with_target(true).with_file(true).with_line_number(true))
                .with(otel)
                .init();
        }
        return;
    }

    // Default path: no OTel (feature off or env var absent).
    if json {
        Registry::default()
            .with(reloadable)
            .with(fmt::layer().json().with_target(true).with_file(true).with_line_number(true))
            .init();
    } else {
        Registry::default()
            .with(reloadable)
            .with(fmt::layer().with_target(true).with_file(true).with_line_number(true))
            .init();
    }
}

/// Update the runtime log level. Returns true if the reload handle is available.
/// Has no effect if `init` was not called (e.g., in tests that don't init tracing).
pub fn set_log_level(level: &str) -> bool {
    if let Some(handle) = LOG_FILTER_HANDLE.get() {
        handle.modify(|f| *f = EnvFilter::new(level)).is_ok()
    } else {
        false
    }
}

/// Build an OTLP tracer from the endpoint env var. Returns `None` when the var is
/// absent or the pipeline fails to build (logs a warning in that case).
#[cfg(feature = "otel")]
fn setup_otel_tracer() -> Option<opentelemetry_sdk::trace::Tracer> {
    use opentelemetry_otlp::WithExportConfig;

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(endpoint);

    let tracer_provider = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(exporter)
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .map_err(|e| tracing::warn!("failed to install OTel OTLP exporter: {e}"))
        .ok()?;

    Some(opentelemetry::trace::TracerProvider::tracer(
        &tracer_provider,
        "veyron",
    ))
}
