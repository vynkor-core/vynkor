use std::sync::OnceLock;
use tracing_subscriber::layer::{Layer, Layered, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, reload, EnvFilter, Registry};

type FilterHandle = reload::Handle<EnvFilter, Registry>;
// subscriber stack through the reload layer — the S the shared fmt layer is boxed for
type BaseStack = Layered<reload::Layer<EnvFilter, Registry>, Registry>;

static LOG_FILTER_HANDLE: OnceLock<FilterHandle> = OnceLock::new();

/// Install the global tracing subscriber with a reloadable log filter.
/// `RUST_LOG` (if set) wins; otherwise falls back to the kernel's configured
/// log level. Set `LOG_FORMAT=json` for structured JSON output. When the `otel`
/// feature is compiled in and `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set, an
/// OTLP/gRPC exporter is also installed as an additional subscriber layer.
///
/// Returns false when a global subscriber is already installed (e.g. a test
/// initialized tracing first) instead of panicking — the existing subscriber
/// keeps running.
///
/// Layer ordering: reload → fmt → [otel when feature+env enabled].
/// `reload::Layer<EnvFilter, Registry>` is bound to `Registry` as S and must be first.
/// The json/plain choice is made once into a boxed fmt layer, so the field
/// config is not duplicated per branch; the otel tail composes after it.
pub fn try_init(level: &str) -> bool {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let (reloadable, handle) = reload::Layer::new(filter);
    LOG_FILTER_HANDLE.set(handle).ok();

    let json = std::env::var("LOG_FORMAT").as_deref() == Ok("json");
    let fmt_layer: Box<dyn Layer<BaseStack> + Send + Sync> = if json {
        fmt::layer()
            .json()
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .boxed()
    } else {
        fmt::layer()
            .with_target(true)
            .with_file(true)
            .with_line_number(true)
            .boxed()
    };

    #[cfg(feature = "otel")]
    if let Some(tracer) = setup_otel_tracer() {
        return Registry::default()
            .with(reloadable)
            .with(fmt_layer)
            .with(tracing_opentelemetry::layer().with_tracer(tracer))
            .try_init()
            .is_ok();
    }

    // Default path: no OTel (feature off or env var absent).
    Registry::default()
        .with(reloadable)
        .with(fmt_layer)
        .try_init()
        .is_ok()
}

/// Update the runtime log level. Returns true if the reload handle is available.
/// Has no effect if `try_init` was not called (e.g., in tests that don't init tracing).
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
fn setup_otel_tracer() -> Option<opentelemetry_sdk::trace::SdkTracer> {
    use opentelemetry_otlp::WithExportConfig;

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok()?;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| tracing::warn!("failed to build OTel OTLP exporter: {e}"))
        .ok()?;

    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();

    Some(opentelemetry::trace::TracerProvider::tracer(
        &tracer_provider,
        "veyron",
    ))
}
