//! Implementation of the `ToRumps` derive macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Fields};

use crate::attrs::{ContainerAttrs, ParsedFields};

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            Fields::Named(f) => expand_named(input, name, impl_generics, ty_generics, where_clause, f),
            Fields::Unnamed(f) if f.unnamed.len() == 1 => {
                let container = ContainerAttrs::from_attrs(&input.attrs)?;
                let global = container.global_or_err(input.ident.span())?;
                f.unnamed
                    .first()
                    .ok_or_else(|| syn::Error::new_spanned(input, "expected single field"))
                    .map(|field| {
                        let inner = &field.ty;
                        quote! {
                            impl #impl_generics ::rumps_storage::orm::ToRumps for #name #ty_generics #where_clause {
                                const GLOBAL: &'static str = #global;
                                const KEY_LEN: usize = <#inner as ::rumps_storage::orm::ToRumps>::KEY_LEN;

                                fn to_key(&self) -> ::rumps_types::Key {
                                    <#inner as ::rumps_storage::orm::ToRumps>::to_key(&self.0)
                                }

                                fn to_pairs(&self, prefix: &::rumps_types::Key) -> ::std::vec::Vec<(::rumps_types::Key, ::rumps_types::Value)> {
                                    <#inner as ::rumps_storage::orm::ToRumps>::to_pairs(&self.0, prefix)
                                }
                            }
                        }
                    })
            }
            Fields::Unnamed(_) => Err(syn::Error::new_spanned(
                input,
                "ToRumps can only be derived for newtype structs (single-field tuple structs)",
            )),
            Fields::Unit => Err(syn::Error::new_spanned(
                input,
                "ToRumps cannot be derived for unit structs",
            )),
        },
        syn::Data::Enum(_) => Err(syn::Error::new_spanned(
            input,
            "ToRumps cannot be derived for enums; use ToValue for unit enums",
        )),
        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "ToRumps cannot be derived for unions",
        )),
    }
}

fn expand_named(
    input: &DeriveInput,
    name: &syn::Ident,
    impl_generics: syn::ImplGenerics,
    ty_generics: syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    f: &syn::FieldsNamed,
) -> syn::Result<TokenStream> {
    let container = ContainerAttrs::from_attrs(&input.attrs)?;
    let global = container.global_or_err(input.ident.span())?;
    let fields = ParsedFields::from_named(f)?;

    let to_key_body = gen_to_key(&fields);
    let to_pairs_body = gen_to_pairs(&fields);
    let key_len = fields.key_fields.len();

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::ToRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;
            const KEY_LEN: usize = #key_len;

            fn to_key(&self) -> ::rumps_types::Key {
                #to_key_body
            }

            fn to_pairs(&self, prefix: &::rumps_types::Key) -> ::std::vec::Vec<(::rumps_types::Key, ::rumps_types::Value)> {
                #to_pairs_body
            }
        }
    })
}

/// Generate the `to_key` method body.
fn gen_to_key(fields: &ParsedFields) -> TokenStream {
    let subscripts: Vec<_> = fields
        .key_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            quote! {
                ::rumps_types::orm::ToSubscript::to_sub(&self.#ident)
            }
        })
        .collect();

    match subscripts.len() {
        0 => quote! { ::rumps_types::Key::new() },
        _ => quote! {
            ::rumps_types::Key::from(::std::vec![#(#subscripts),*])
        },
    }
}

/// Generate the `to_pairs` method body.
fn gen_to_pairs(fields: &ParsedFields) -> TokenStream {
    let mut statements = Vec::new();

    // Initialize result vector with a marker at the prefix itself.
    // This ensures the record exists even if all optional fields are `None`.
    // The marker uses an empty string value at the exact prefix key.
    statements.push(quote! {
        let mut pairs = ::std::vec::Vec::new();
        pairs.push((prefix.clone(), ::rumps_types::Value::String(::std::string::String::new())));
    });

    // Value fields: store at prefix + field_name
    fields.value_fields.iter().for_each(|f| {
        let ident = &f.ident;
        let sub_name = f.subscript_name();
        let ty = &f.ty;

        // Check if this is an Option<T>
        let is_option = is_option_type(ty);

        let stmt = if is_option {
            // For Option<T>, only store Some values
            quote! {
                self.#ident.as_ref().into_iter().for_each(|v| {
                    let mut k = prefix.clone();
                    k.push(::rumps_types::Subscript::from(#sub_name));
                    pairs.push((k, ::rumps_types::orm::ToValue::to_val(v)));
                });
            }
        } else {
            quote! {
                {
                    let mut k = prefix.clone();
                    k.push(::rumps_types::Subscript::from(#sub_name));
                    pairs.push((k, ::rumps_types::orm::ToValue::to_val(&self.#ident)));
                }
            }
        };
        statements.push(stmt);
    });

    // Flatten fields: call nested to_pairs with same prefix
    fields.flatten_fields.iter().for_each(|f| {
        let ident = &f.ident;
        let ty = &f.ty;
        let is_option = is_option_type(ty);

        let stmt = if is_option {
            quote! {
                self.#ident.as_ref().into_iter().for_each(|v| {
                    pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(v, prefix));
                });
            }
        } else {
            quote! {
                pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(&self.#ident, prefix));
            }
        };
        statements.push(stmt);
    });

    // Subtree fields: call nested to_pairs with prefix + field_name
    fields.subtree_fields.iter().for_each(|f| {
        let ident = &f.ident;
        let sub_name = f.subscript_name();
        let ty = &f.ty;
        let is_option = is_option_type(ty);

        let stmt = if is_option {
            quote! {
                self.#ident.as_ref().into_iter().for_each(|v| {
                    let mut sub_prefix = prefix.clone();
                    sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                    pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(v, &sub_prefix));
                });
            }
        } else {
            quote! {
                {
                    let mut sub_prefix = prefix.clone();
                    sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                    pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(&self.#ident, &sub_prefix));
                }
            }
        };
        statements.push(stmt);
    });

    statements.push(quote! { pairs });

    quote! {
        #(#statements)*
    }
}

/// Check if a type is `Option<T>`.
fn is_option_type(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident == "Option")
            .unwrap_or(false),
        _ => false,
    }
}
