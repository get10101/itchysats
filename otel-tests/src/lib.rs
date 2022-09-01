use std::sync::Once;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;

#[derive(serde::Deserialize)]
struct Config {
    directives: Vec<String>,
}

#[doc(hidden)]
pub mod __reexport {
    pub use futures;
    #[cfg(feature = "otlp")]
    pub use opentelemetry;
    pub use tokio;
}

pub use otel_tests_macro::otel_test;

static INIT_OTLP_EXPORTER: Once = Once::new();

pub fn init_tracing(module: &'static str) {
    let env = std::env::var("ITCHYSATS_TEST_INSTRUMENTATION").unwrap_or_default();

    #[cfg(not(feature = "otlp"))]
    assert!(
        env != "1",
        "ITCHYSATS_TEST_INSTRUMENTATION=1 will not do anything, as tests were compiled \
             without the `otlp` feature"
    );

    INIT_OTLP_EXPORTER.call_once(|| {
        #[cfg(feature = "otlp")]
        let telemetry = if env == "1" {
            use opentelemetry::sdk::trace;
            use opentelemetry::sdk::Resource;
            use opentelemetry::KeyValue;
            use opentelemetry_otlp::WithExportConfig;

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

        let global_directive = LevelFilter::WARN.into();
        let contents = std::fs::read_to_string("otel-tests.toml").unwrap_or_else(|_| {
            panic!(
                "File 'otel-tests.json' should be located in: {:?}",
                std::env::current_dir().unwrap()
            )
        });
        let config: Config =
            toml::from_str(&contents).expect("to be able to parse Config file from toml");

        let mut filter = EnvFilter::from_default_env()
            // apply warning level globally
            .add_directive(global_directive)
            // log traces from test itself
            .add_directive(format!("{module}=debug").parse().unwrap());

        for directive in config.directives.iter() {
            filter = filter.add_directive(directive.parse().unwrap());
        }

        let fmt_layer = tracing_subscriber::fmt::layer()
            .with_test_writer()
            .with_filter(filter);

        #[cfg(feature = "otlp")]
        Registry::default().with(telemetry).with(fmt_layer).init();

        #[cfg(not(feature = "otlp"))]
        Registry::default().with(fmt_layer).init();
    })
}
