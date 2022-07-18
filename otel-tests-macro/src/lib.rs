use proc_macro::TokenStream;
use quote::quote;
use syn::ItemFn;

#[proc_macro_attribute]
pub fn otel_test(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let fn_item = syn::parse::<ItemFn>(item).unwrap();

    let sig = fn_item.sig;
    let block = fn_item.block;
    let name = &sig.ident;
    let attrs = fn_item.attrs;

    let test = if !attrs
        .iter()
        .any(|attr| attr.path.segments.last().unwrap().ident == "test")
    {
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
