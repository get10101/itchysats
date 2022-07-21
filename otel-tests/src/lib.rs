use opentelemetry::sdk::trace;
use opentelemetry::sdk::Resource;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use std::sync::Once;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

#[doc(hidden)]
pub mod __reexport {
    pub use futures;
    pub use opentelemetry;
    pub use tokio;
}

pub use otel_tests_macro::otel_test;

static INIT_OTLP_EXPORTER: Once = Once::new();

pub fn init_tracing(module: &'static str) {
    INIT_OTLP_EXPORTER.call_once(|| {
        let env = std::env::var("ITCHYSATS_TEST_INSTRUMENTATION").unwrap_or_default();
        let telemetry = if env == "1" {
            let cfg = trace::Config::default()
                .with_resource(Resource::new([KeyValue::new("service.name", module)]));

            let tracer = opentelemetry_otlp::new_pipeline()
                .tracing()
                .with_trace_config(cfg)
                .with_exporter(
                    opentelemetry_otlp::new_exporter()
                        .grpcio()
                        .with_endpoint("localhost:4317"),
                )
                .install_simple()
                .unwrap();

            Some(
                tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(LevelFilter::DEBUG),
            )
        } else {
            None
        };

        let filter = EnvFilter::from_default_env()
            // apply warning level globally
            .add_directive(LevelFilter::WARN.into())
            // log traces from test itself
            .add_directive(format!("{module}=debug").parse().unwrap())
            .add_directive("wire=trace".parse().unwrap())
            .add_directive("taker=debug".parse().unwrap())
            .add_directive("maker=debug".parse().unwrap())
            .add_directive("daemon=debug".parse().unwrap())
            .add_directive("model=info".parse().unwrap())
            .add_directive("xtra_libp2p=debug".parse().unwrap())
            .add_directive("xtra_libp2p_offer=debug".parse().unwrap())
            .add_directive("xtra_libp2p_ping=debug".parse().unwrap())
            .add_directive("rocket=warn".parse().unwrap());

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);

        Registry::default().with(telemetry).with(fmt_layer).init();
    })
}
