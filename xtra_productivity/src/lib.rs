use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ImplItem, ItemImpl, MetaNameValue, ReturnType};

#[proc_macro_attribute]
pub fn xtra_productivity(attribute: TokenStream, item: TokenStream) -> TokenStream {
    let block = syn::parse::<ItemImpl>(item).unwrap();
    let want_message_impl = if attribute.is_empty() {
        true
    } else {
        let attribute = syn::parse::<MetaNameValue>(attribute).unwrap();
        if !attribute.path.is_ident("message_impl") {
            panic!(
                "Unexpected attribute {:?}",
                attribute.path.get_ident().unwrap()
            )
        }

        matches!(
            attribute.lit,
            syn::Lit::Bool(syn::LitBool { value: true, .. })
        )
    };

    let actor = block.self_ty;

    let code = block
        .items
        .into_iter()
        .filter_map(|block_item| match block_item {
            ImplItem::Method(method) => Some(method),
            _ => None,
        })
        .map(|method| {
            let message_arg = method.sig.inputs[1].clone();

            let message_type = match message_arg {
                // receiver represents self
                FnArg::Receiver(_) => unreachable!("cannot have receiver on second position"),
                FnArg::Typed(ref typed) => typed.ty.clone()
            };

                        let method_return = method.sig.output;
            let method_block = method.block;

            let context_arg = method.sig.inputs.iter().nth(2).map(|fn_arg| quote! { #fn_arg }).unwrap_or_else(|| quote! {
                _ctx: &mut xtra::Context<Self>
            });

            let result_type = match method_return {
                ReturnType::Default => quote! { () },
                ReturnType::Type(_, ref t) => quote! { #t }
            };

            let message_impl = if want_message_impl {
                quote! {
                    impl xtra::Message for #message_type {
                        type Result = #result_type;
                    }
                }
            } else {
                quote! {}
            };

            quote! {
                #message_impl

                #[async_trait]
                impl xtra::Handler<#message_type> for #actor {
                    async fn handle(&mut self, #message_arg, #context_arg) #method_return #method_block
                }
            }
        }).collect::<Vec<_>>();

    (quote! {
        #(#code)*
    })
    .into()
}
