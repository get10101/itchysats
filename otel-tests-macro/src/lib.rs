use proc_macro::TokenStream;
use quote::quote;
use syn::ItemFn;

/// Like `#[tokio::test]`, but instrument the test with a span matching the test name, and export
/// the span over OTLP to Jaeger for local debugging. To enable the span exporting, the
/// `ITCHYSATS_TEST_INSTRUMENTATION` env var should be set to `1`. It is disabled by default for CI,
/// as these tests may fail if OTLP exporting is enabled but Jaeger is not active, since this will
/// slow everything down and lead to timeouts in the tests triggering.
#[proc_macro_attribute]
pub fn otel_test(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let fn_item = syn::parse::<ItemFn>(item).unwrap();

    let sig = fn_item.sig;
    let block = fn_item.block;
    let name = &sig.ident;
    let attrs = fn_item.attrs;

    let test = if attrs.is_empty() {
        quote!(#[otel_tests::__reexport::tokio::test])
    } else {
        quote!()
    };

    let tokens = quote! {
        #test
        #(#attrs)*
        #sig {
            ::otel_tests::init_tracing(module_path!());

            let caught = {
                ::otel_tests::__reexport::futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(tracing::Instrument::instrument(
                    async #block, tracing::info_span!(stringify!(#name))
                ))).await
            };

            // Give the otel thread time to receive the spans before flush
            #[allow(clippy::disallowed_methods)]
            ::otel_tests::__reexport::tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            // If this is the last test that's running, the main thread might exit. Then, the
            // opentelemetry exporter thread might not have exported all of its spans yet, leading
            // to some dropped spans. This ensures in most cases that it happens.
            ::otel_tests::__reexport::opentelemetry::global::force_flush_tracer_provider();

            if let Err(e) = caught {
                panic!("{:#?}", e);
            }
        }
    };

    tokens.into()
}
