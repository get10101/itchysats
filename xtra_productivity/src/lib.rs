use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{FnArg, GenericParam, ImplItem, ItemImpl, ItemStruct, ReturnType, WherePredicate};

#[proc_macro_attribute]
pub fn xtra_productivity(_attribute: TokenStream, item: TokenStream) -> TokenStream {
    let block = syn::parse::<ItemImpl>(item).unwrap();

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

            quote! {
                impl xtra::Message for #message_type {
                    type Result = #result_type;
                }

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

#[proc_macro_derive(Actor)]
pub fn derive_actor(item: TokenStream) -> TokenStream {
    let struct_item = syn::parse::<ItemStruct>(item).unwrap();

    let ty = struct_item.ident;

    for param in &struct_item.generics.params {
        if let GenericParam::Type(ty) = param {
            if !ty.bounds.is_empty() {
                return syn::Error::new(
                    ty.bounds.span(),
                    r#"Move bounds to where clause when using the `Actor` custom derive"#,
                )
                .to_compile_error()
                .into();
            }
        }
    }

    let generic_types = &struct_item
        .generics
        .params
        .iter()
        .filter_map(|param| match param {
            GenericParam::Type(ty) => Some(ty.ident.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    let where_clauses = struct_item
        .generics
        .where_clause
        .map(|clause| {
            let clauses = clause
                .predicates
                .into_iter()
                .flat_map(|predicate| match predicate {
                    WherePredicate::Type(pt) => Some({
                        let ty = pt.bounded_ty;
                        let bounds = pt.bounds;

                        quote! {
                            #ty: #bounds + Send + 'static
                        }
                    }),
                    _ => None,
                });

            quote! {
                #(#clauses),*
            }
        })
        .unwrap_or_else(|| {
            quote! {
                #(#generic_types: Send + 'static),*
            }
        });

    (quote! {
        impl<#(#generic_types),*> xtra::Actor for #ty<#(#generic_types),*> where #where_clauses {

        }
    })
    .into()
}
