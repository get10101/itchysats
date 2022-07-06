use proc_macro::TokenStream;
use quote::quote;
use syn::ItemFn;

#[proc_macro_attribute]
pub fn traced_test(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let fn_item = syn::parse::<ItemFn>(item).unwrap();

    let sig = fn_item.sig;
    let block = fn_item.block;
    let name = &sig.ident;

    let tokens = quote! {
        #[tokio::test]
        #sig {
            ::daemon_tests::init_tracing();

            {
                tracing::Instrument::instrument(
                    async #block, tracing::info_span!(stringify!(#name))
                ).await;
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            opentelemetry::global::force_flush_tracer_provider();
        }
    };

    tokens.into()
}
