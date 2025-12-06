//! Implementation of the `FromRumps` derive macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, Fields};

use crate::attrs::{ContainerAttrs, DefaultValue, ParsedFields};

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
                            impl #impl_generics ::rumps_storage::orm::FromRumps for #name #ty_generics #where_clause {
                                const GLOBAL: &'static str = #global;
                                const KEY_LEN: usize = <#inner as ::rumps_storage::orm::FromRumps>::KEY_LEN;

                                fn from_pairs<__I>(
                                    prefix: &::rumps_types::Key,
                                    pairs: __I,
                                ) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError>
                                where
                                    __I: ::std::iter::Iterator<Item = (::rumps_types::Key, ::rumps_types::Value)>,
                                {
                                    <#inner as ::rumps_storage::orm::FromRumps>::from_pairs(prefix, pairs).map(Self)
                                }
                            }
                        }
                    })
            }
            Fields::Unnamed(_) => Err(syn::Error::new_spanned(
                input,
                "FromRumps can only be derived for newtype structs (single-field tuple structs)",
            )),
            Fields::Unit => Err(syn::Error::new_spanned(
                input,
                "FromRumps cannot be derived for unit structs",
            )),
        },
        syn::Data::Enum(_) => Err(syn::Error::new_spanned(
            input,
            "FromRumps cannot be derived for enums; use FromValue for unit enums",
        )),
        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "FromRumps cannot be derived for unions",
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

    let from_pairs_body = gen_from_pairs(&fields, name);
    let key_len = fields.key_fields.len();

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::FromRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;
            const KEY_LEN: usize = #key_len;

            fn from_pairs<__I>(
                prefix: &::rumps_types::Key,
                mut pairs: __I,
            ) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError>
            where
                __I: ::std::iter::Iterator<Item = (::rumps_types::Key, ::rumps_types::Value)>,
            {
                #from_pairs_body
            }
        }
    })
}

/// Generate the `from_pairs` method body using `try_fold` with explicit accumulator.
fn gen_from_pairs(
    fields: &ParsedFields,
    struct_name: &syn::Ident,
) -> TokenStream {
    // Extract key fields from prefix
    let key_field_extractions: Vec<_> = fields
        .key_fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let ident = &f.ident;
            let ty = &f.ty;
            let field_name = ident.to_string();
            quote! {
                let #ident: #ty = prefix
                    .get(#i)
                    .ok_or_else(|| ::rumps_types::orm::DecodeError::MissingField { field: #field_name })
                    .and_then(::rumps_types::orm::FromSubscript::from_sub)?;
            }
        })
        .collect();

    // Generate the fold-based iteration for value, flatten, and subtree fields
    let pairs_processing = gen_pairs_fold(fields);

    // Build flatten fields from collected pairs
    let flatten_field_builds: Vec<_> = fields
        .flatten_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            let var_name = format_ident!("{}_pairs", ident);
            let inner_ty = extract_option_inner(ty);

            match inner_ty {
                Some(inner) => quote! {
                    let #ident: #ty = if #var_name.is_empty() {
                        ::std::option::Option::None
                    } else {
                        ::std::option::Option::Some(
                            <#inner as ::rumps_storage::orm::FromRumps>::from_pairs(prefix, #var_name.into_iter())?
                        )
                    };
                },
                None => quote! {
                    let #ident: #ty = <#ty as ::rumps_storage::orm::FromRumps>::from_pairs(
                        prefix,
                        #var_name.into_iter()
                    )?;
                },
            }
        })
        .collect();

    // Build subtree fields from collected pairs
    let subtree_field_builds: Vec<_> = fields
        .subtree_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            let var_name = format_ident!("{}_pairs", ident);
            let sub_name = f.subscript_name();
            let inner_ty = extract_option_inner(ty);

            match inner_ty {
                Some(inner) => quote! {
                    let #ident: #ty = if #var_name.is_empty() {
                        ::std::option::Option::None
                    } else {
                        let mut sub_prefix = prefix.clone();
                        sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                        ::std::option::Option::Some(
                            <#inner as ::rumps_storage::orm::FromRumps>::from_pairs(
                                &sub_prefix,
                                #var_name.into_iter()
                            )?
                        )
                    };
                },
                None => quote! {
                    let #ident: #ty = {
                        let mut sub_prefix = prefix.clone();
                        sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                        <#ty as ::rumps_storage::orm::FromRumps>::from_pairs(
                            &sub_prefix,
                            #var_name.into_iter()
                        )?
                    };
                },
            }
        })
        .collect();

    // Final field assignments with default handling for value fields
    let value_field_finals: Vec<_> = fields
        .value_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            let field_name = ident.to_string();
            let is_option = is_option_type(ty);

            match (&f.attrs.default, is_option) {
                (_, true) => quote! {
                    let #ident: #ty = #ident;
                },
                (Some(DefaultValue::Trait), false) => quote! {
                    let #ident: #ty = #ident.unwrap_or_default();
                },
                (Some(DefaultValue::Expr(expr)), false) => quote! {
                    let #ident: #ty = #ident.unwrap_or_else(|| #expr);
                },
                (None, false) => quote! {
                    let #ident: #ty = #ident.ok_or_else(|| {
                        ::rumps_types::orm::DecodeError::MissingField { field: #field_name }
                    })?;
                },
            }
        })
        .collect();

    // Skip fields need defaults
    let skip_field_vars: Vec<_> = fields
        .skip_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            match &f.attrs.default {
                Some(DefaultValue::Expr(expr)) => quote! {
                    let #ident: #ty = #expr;
                },
                _ => quote! {
                    let #ident: #ty = ::std::default::Default::default();
                },
            }
        })
        .collect();

    // Build struct - include ALL fields including skipped ones
    let all_field_names: Vec<_> = fields
        .key_fields
        .iter()
        .chain(fields.value_fields.iter())
        .chain(fields.flatten_fields.iter())
        .chain(fields.subtree_fields.iter())
        .chain(fields.skip_fields.iter())
        .map(|f| &f.ident)
        .collect();

    quote! {
        // Extract key fields from prefix
        #(#key_field_extractions)*

        // Process pairs using try_fold
        #pairs_processing

        // Build flatten fields
        #(#flatten_field_builds)*

        // Build subtree fields
        #(#subtree_field_builds)*

        // Finalize value fields (with defaults)
        #(#value_field_finals)*

        // Skip fields
        #(#skip_field_vars)*

        // Build struct
        ::std::result::Result::Ok(#struct_name {
            #(#all_field_names),*
        })
    }
}

/// Generate the `try_fold` iteration that processes all pairs.
/// Uses an explicit accumulator tuple to avoid mutable capture issues.
fn gen_pairs_fold(fields: &ParsedFields) -> TokenStream {
    let has_value_fields = !fields.value_fields.is_empty();
    let has_flatten_fields = !fields.flatten_fields.is_empty();
    let has_subtree_fields = !fields.subtree_fields.is_empty();

    // If no fields need processing, just consume the iterator
    if !has_value_fields && !has_flatten_fields && !has_subtree_fields {
        return quote! {
            pairs.for_each(|_| {});
        };
    }

    // Generate accumulator tuple type and initial value
    let (acc_types, acc_inits, acc_patterns, acc_returns) =
        gen_accumulator_parts(fields);

    // Generate the match arms for updating the accumulator
    let update_logic = gen_update_logic(fields);

    // Generate field name bindings from accumulator results
    let field_bindings = gen_field_bindings(fields);

    // Use trailing commas to handle single-element tuples correctly
    quote! {
        let (#(#acc_patterns,)*) = pairs.try_fold(
            (#(#acc_inits,)*),
            |(#(mut #acc_patterns,)*), (k, v)| -> ::std::result::Result<(#(#acc_types,)*), ::rumps_types::orm::DecodeError> {
                if k.starts_with(prefix) {
                    let suffix_start = prefix.len();
                    #update_logic
                }
                ::std::result::Result::Ok((#(#acc_returns,)*))
            }
        )?;

        // Rename accumulator vars back to field names
        #field_bindings
    }
}

/// Generate bindings from accumulator vars back to original field names.
fn gen_field_bindings(fields: &ParsedFields) -> TokenStream {
    let value_bindings: Vec<_> = fields
        .value_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let acc_var = format_ident!("__acc_{}", ident);
            quote! { let #ident = #acc_var; }
        })
        .collect();

    let flatten_bindings: Vec<_> = fields
        .flatten_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let var_name = format_ident!("{}_pairs", ident);
            let acc_var = format_ident!("__acc_{}_pairs", ident);
            quote! { let #var_name = #acc_var; }
        })
        .collect();

    let subtree_bindings: Vec<_> = fields
        .subtree_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let var_name = format_ident!("{}_pairs", ident);
            let acc_var = format_ident!("__acc_{}_pairs", ident);
            quote! { let #var_name = #acc_var; }
        })
        .collect();

    quote! {
        #(#value_bindings)*
        #(#flatten_bindings)*
        #(#subtree_bindings)*
    }
}

/// Generate the accumulator parts: types, initial values, pattern names, return expressions.
/// Uses `__acc_` prefix to avoid naming conflicts with field names.
fn gen_accumulator_parts(
    fields: &ParsedFields,
) -> (
    Vec<TokenStream>,
    Vec<TokenStream>,
    Vec<syn::Ident>,
    Vec<TokenStream>,
) {
    let mut types = Vec::new();
    let mut inits = Vec::new();
    let mut patterns = Vec::new();
    let mut returns = Vec::new();

    // Value fields: Option<InnerType>
    fields.value_fields.iter().for_each(|f| {
        let ident = &f.ident;
        let acc_var = format_ident!("__acc_{}", ident);
        let ty = &f.ty;
        let inner_ty = extract_option_inner(ty).unwrap_or(ty);

        types.push(quote! { ::std::option::Option<#inner_ty> });
        inits.push(quote! { ::std::option::Option::None });
        patterns.push(acc_var.clone());
        returns.push(quote! { #acc_var });
    });

    // Flatten fields: Vec<(Key, Value)>
    fields.flatten_fields.iter().for_each(|f| {
        let acc_var = format_ident!("__acc_{}_pairs", f.ident);

        types.push(quote! { ::std::vec::Vec<(::rumps_types::Key, ::rumps_types::Value)> });
        inits.push(quote! { ::std::vec::Vec::new() });
        patterns.push(acc_var.clone());
        returns.push(quote! { #acc_var });
    });

    // Subtree fields: Vec<(Key, Value)>
    fields.subtree_fields.iter().for_each(|f| {
        let acc_var = format_ident!("__acc_{}_pairs", f.ident);

        types.push(quote! { ::std::vec::Vec<(::rumps_types::Key, ::rumps_types::Value)> });
        inits.push(quote! { ::std::vec::Vec::new() });
        patterns.push(acc_var.clone());
        returns.push(quote! { #acc_var });
    });

    (types, inits, patterns, returns)
}

/// Generate the update logic inside the fold closure.
/// Uses `__acc_` prefixed variable names to match the accumulator.
fn gen_update_logic(fields: &ParsedFields) -> TokenStream {
    let has_flatten = !fields.flatten_fields.is_empty();

    // Flatten fields get ALL pairs (cloned)
    let flatten_updates: Vec<_> = fields
        .flatten_fields
        .iter()
        .map(|f| {
            let acc_var = format_ident!("__acc_{}_pairs", f.ident);
            quote! { #acc_var.push((k.clone(), v.clone())); }
        })
        .collect();

    // Value field match arms
    let value_matches: Vec<_> = fields
        .value_fields
        .iter()
        .map(|f| {
            let acc_var = format_ident!("__acc_{}", f.ident);
            let sub_name = f.subscript_name();
            let ty = &f.ty;
            let inner_ty = extract_option_inner(ty).unwrap_or(ty);
            quote! {
                ::std::option::Option::Some(s) if s == &::rumps_types::Subscript::from(#sub_name) => {
                    #acc_var = ::std::option::Option::Some(
                        <#inner_ty as ::rumps_types::orm::FromValue>::from_val(&v)?
                    );
                }
            }
        })
        .collect();

    // Subtree field match arms
    let subtree_matches: Vec<_> = fields
        .subtree_fields
        .iter()
        .map(|f| {
            let acc_var = format_ident!("__acc_{}_pairs", f.ident);
            let sub_name = f.subscript_name();
            quote! {
                ::std::option::Option::Some(s) if s == &::rumps_types::Subscript::from(#sub_name) => {
                    #acc_var.push((k, v));
                }
            }
        })
        .collect();

    let flatten_code = if has_flatten {
        quote! { #(#flatten_updates)* }
    } else {
        quote! {}
    };

    // Only generate match if there are value or subtree fields
    if value_matches.is_empty() && subtree_matches.is_empty() {
        flatten_code
    } else {
        quote! {
            #flatten_code
            match k.get(suffix_start) {
                #(#value_matches)*
                #(#subtree_matches)*
                _ => {}
            }
        }
    }
}

/// Check if a type is `Option<T>`.
fn is_option_type(ty: &syn::Type) -> bool {
    extract_option_inner(ty).is_some()
}

/// Extract the inner type from `Option<T>`.
fn extract_option_inner(ty: &syn::Type) -> Option<&syn::Type> {
    match ty {
        syn::Type::Path(p) => {
            let last = p.path.segments.last()?;
            (last.ident == "Option").then(|| match &last.arguments {
                syn::PathArguments::AngleBracketed(args) => {
                    args.args.first().and_then(|a| match a {
                        syn::GenericArgument::Type(t) => Some(t),
                        _ => None,
                    })
                }
                _ => None,
            })?
        }
        _ => None,
    }
}
