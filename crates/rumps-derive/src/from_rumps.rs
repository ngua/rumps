//! Implementation of the `FromRumps` derive macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, Fields};

use crate::attrs::{
    parse_variants, ContainerAttrs, DefaultValue, ParsedFields, RenameAll,
    VariantFields, VariantInfo,
};

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
        syn::Data::Enum(data) => {
            let container = ContainerAttrs::from_attrs(&input.attrs)?;
            let global = container.global_or_err(input.ident.span())?;
            let variants = parse_variants(&data.variants)?;
            expand_enum(
                name,
                impl_generics,
                ty_generics,
                where_clause,
                global,
                &variants,
                &container,
            )
        }
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
    let rename_all = container.rename_all;

    let from_pairs_body = gen_from_pairs(&fields, name, rename_all);

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::FromRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;

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
    rename_all: RenameAll,
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
    let pairs_processing = gen_pairs_fold(fields, rename_all);

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
            let sub_name = f.subscript_name(rename_all);
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
fn gen_pairs_fold(fields: &ParsedFields, rename_all: RenameAll) -> TokenStream {
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
    let update_logic = gen_update_logic(fields, rename_all);

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
fn gen_update_logic(
    fields: &ParsedFields,
    rename_all: RenameAll,
) -> TokenStream {
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
            let sub_name = f.subscript_name(rename_all);
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
            let sub_name = f.subscript_name(rename_all);
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

/// Expand `FromRumps` derive for enums.
fn expand_enum(
    name: &syn::Ident,
    impl_generics: syn::ImplGenerics,
    ty_generics: syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    global: &syn::LitStr,
    variants: &[VariantInfo],
    container: &ContainerAttrs,
) -> syn::Result<TokenStream> {
    if container.untagged {
        expand_untagged_enum(
            name,
            impl_generics,
            ty_generics,
            where_clause,
            global,
            variants,
            container,
        )
    } else {
        expand_tagged_enum(
            name,
            impl_generics,
            ty_generics,
            where_clause,
            global,
            variants,
            container,
        )
    }
}

/// Expand `FromRumps` for tagged enums (default behavior).
fn expand_tagged_enum(
    name: &syn::Ident,
    impl_generics: syn::ImplGenerics,
    ty_generics: syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    global: &syn::LitStr,
    variants: &[VariantInfo],
    container: &ContainerAttrs,
) -> syn::Result<TokenStream> {
    let enum_name_str = name.to_string();
    let rename_all = container.rename_all;

    // Generate match arms for each variant
    let variant_arms = variants
        .iter()
        .map(|v| gen_enum_from_pairs_arm(v, rename_all))
        .collect::<Vec<_>>();

    // Collect all variant tag names for error message
    let variant_names: Vec<_> =
        variants.iter().map(|v| v.tag_name(rename_all)).collect();
    let variant_list = variant_names.join(", ");

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::FromRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;

            fn from_pairs<__I>(
                prefix: &::rumps_types::Key,
                mut pairs: __I,
            ) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError>
            where
                __I: ::std::iter::Iterator<Item = (::rumps_types::Key, ::rumps_types::Value)>,
            {
                // Collect all pairs for analysis
                let all_pairs: ::std::vec::Vec<_> = pairs.collect();

                // Determine the variant tag. Two cases:
                // 1. Top-level: `prefix` = `[tag, key_fields...]` (from `to_key()`)
                //    -> tag is at `prefix.get(0)`
                // 2. Embedded: `prefix` = `[parent_stuff..., field_name]` (subtree)
                //    -> tag is at `first_pair.key.get(prefix.len())`
                //
                // We detect which case by checking if `prefix.get(0)` is a known variant tag.
                let tag_from_prefix = prefix.get(0).and_then(|s| match s {
                    ::rumps_types::Subscript::String(st) => ::std::option::Option::Some(st.as_str()),
                    _ => ::std::option::Option::None,
                });

                // Check if prefix[0] is one of our variant tags
                let known_tags: &[&str] = &[#(#variant_names),*];
                let is_top_level = tag_from_prefix
                    .map(|t| known_tags.contains(&t))
                    .unwrap_or(false);

                let (tag, effective_prefix) = if is_top_level {
                    // Top-level: tag is in prefix, use prefix as-is
                    // Safe: is_top_level is only true when tag_from_prefix.is_some()
                    (tag_from_prefix.unwrap(), prefix.clone())
                } else {
                    // Embedded: extract tag from pairs, build effective_prefix
                    let t = all_pairs
                        .first()
                        .and_then(|(k, _)| k.get(prefix.len()))
                        .and_then(|s| match s {
                            ::rumps_types::Subscript::String(st) => ::std::option::Option::Some(st.as_str()),
                            _ => ::std::option::Option::None,
                        })
                        .ok_or_else(|| ::rumps_types::orm::DecodeError::Custom(
                            ::std::format!("missing variant tag for enum {}", #enum_name_str)
                        ))?;
                    let mut ep = prefix.clone();
                    ep.push(::rumps_types::Subscript::from(t));
                    (t, ep)
                };

                // Use effective_prefix in the match arms
                let prefix = &effective_prefix;

                match tag {
                    #(#variant_arms)*
                    other => ::std::result::Result::Err(::rumps_types::orm::DecodeError::Custom(
                        ::std::format!(
                            "unknown {} variant: `{}` (expected one of: {})",
                            #enum_name_str, other, #variant_list
                        )
                    )),
                }
            }
        }
    })
}

/// Expand `FromRumps` for untagged enums.
///
/// Untagged enums try each variant in declaration order until one succeeds.
fn expand_untagged_enum(
    name: &syn::Ident,
    impl_generics: syn::ImplGenerics,
    ty_generics: syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    global: &syn::LitStr,
    variants: &[VariantInfo],
    container: &ContainerAttrs,
) -> syn::Result<TokenStream> {
    let enum_name_str = name.to_string();
    let rename_all = container.rename_all;

    // Generate try-parse expressions for each variant
    let try_variants: Vec<_> = variants
        .iter()
        .map(|v| gen_untagged_try_variant(v, rename_all))
        .collect();

    // Collect variant names for error message
    let variant_names: Vec<_> =
        variants.iter().map(|v| v.ident.to_string()).collect();
    let variant_list = variant_names.join(", ");

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::FromRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;

            fn from_pairs<__I>(
                prefix: &::rumps_types::Key,
                pairs: __I,
            ) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError>
            where
                __I: ::std::iter::Iterator<Item = (::rumps_types::Key, ::rumps_types::Value)>,
            {
                // Collect all pairs for analysis (needed for multiple parse attempts)
                let all_pairs: ::std::vec::Vec<_> = pairs.collect();

                // Try each variant in order until one succeeds
                #(#try_variants)*

                // All variants failed
                ::std::result::Result::Err(::rumps_types::orm::DecodeError::Custom(
                    ::std::format!(
                        "data did not match any {} variant (tried: {})",
                        #enum_name_str, #variant_list
                    )
                ))
            }
        }
    })
}

/// Generate a try-parse block for one variant of an untagged enum.
fn gen_untagged_try_variant(
    v: &VariantInfo,
    rename_all: RenameAll,
) -> TokenStream {
    let var_ident = &v.ident;
    let field_rename = v.attrs.rename_all.unwrap_or(rename_all);

    match &v.fields {
        VariantFields::Unit => {
            // Unit variant: succeeds if pairs is empty or contains only a marker
            quote! {
                {
                    let is_empty_or_marker = all_pairs.is_empty()
                        || (all_pairs.len() == 1
                            && all_pairs.first()
                                .map(|(k, v)| {
                                    k == prefix
                                        && matches!(v, ::rumps_types::Value::String(s) if s.is_empty())
                                })
                                .unwrap_or(false));

                    if is_empty_or_marker {
                        return ::std::result::Result::Ok(Self::#var_ident);
                    }
                }
            }
        }
        VariantFields::Tuple(fields) if fields.len() == 1 => {
            // Single-field tuple: try to parse the value directly
            // Use nested match to extract type safely
            fields.first().map(|f| &f.ty).map_or_else(
                || quote! {},
                |ty| {
                    quote! {
                        {
                            // Try to find value at prefix
                            let val_opt = all_pairs
                                .iter()
                                .find(|(k, _)| k == prefix)
                                .map(|(_, v)| v);

                            if let ::std::option::Option::Some(val) = val_opt {
                                if let ::std::result::Result::Ok(field_0) =
                                    <#ty as ::rumps_types::orm::FromValue>::from_val(val)
                                {
                                    return ::std::result::Result::Ok(Self::#var_ident(field_0));
                                }
                            }
                        }
                    }
                },
            )
        }
        VariantFields::Tuple(fields) => {
            // Multi-field tuple: try to parse each field by index
            let field_parsers: Vec<_> = fields
                .iter()
                .filter(|f| !f.attrs.key)
                .map(|f| {
                    let idx = f.index;
                    let ty = &f.ty;
                    let binding = quote::format_ident!("__f{}", idx);
                    quote! {
                        let #binding = {
                            let mut k = prefix.clone();
                            k.push(::rumps_types::Subscript::from(#idx as i64));
                            all_pairs
                                .iter()
                                .find(|(pk, _)| pk == &k)
                                .ok_or(())
                                .and_then(|(_, v)| {
                                    <#ty as ::rumps_types::orm::FromValue>::from_val(v)
                                        .map_err(|_| ())
                                })?
                        };
                    }
                })
                .collect();

            let bindings: Vec<_> = fields
                .iter()
                .filter(|f| !f.attrs.key)
                .map(|f| {
                    let idx = f.index;
                    quote::format_ident!("__f{}", idx)
                })
                .collect();

            quote! {
                {
                    let try_result: ::std::result::Result<Self, ()> = (|| {
                        #(#field_parsers)*
                        ::std::result::Result::Ok(Self::#var_ident(#(#bindings),*))
                    })();

                    if let ::std::result::Result::Ok(val) = try_result {
                        return ::std::result::Result::Ok(val);
                    }
                }
            }
        }
        VariantFields::Struct(pf) => {
            // Struct variant: try to parse required fields
            let required_parsers: Vec<_> = pf
                .value_fields
                .iter()
                .filter(|f| !is_option_type(&f.ty) && f.attrs.default.is_none())
                .map(|f| {
                    let ident = &f.ident;
                    let ty = &f.ty;
                    let sub_name = f.subscript_name(field_rename);
                    quote! {
                        let #ident = {
                            let mut k = prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            all_pairs
                                .iter()
                                .find(|(pk, _)| pk == &k)
                                .ok_or(())
                                .and_then(|(_, v)| {
                                    <#ty as ::rumps_types::orm::FromValue>::from_val(v)
                                        .map_err(|_| ())
                                })?
                        };
                    }
                })
                .collect();

            let optional_parsers: Vec<_> = pf
                .value_fields
                .iter()
                .filter(|f| is_option_type(&f.ty) || f.attrs.default.is_some())
                .map(|f| {
                    let ident = &f.ident;
                    let ty = &f.ty;
                    let sub_name = f.subscript_name(field_rename);
                    let is_opt = is_option_type(ty);
                    let inner_ty = if is_opt {
                        extract_option_inner(ty)
                            .cloned()
                            .unwrap_or_else(|| ty.clone())
                    } else {
                        ty.clone()
                    };

                    let default_expr = f.attrs.default.as_ref().map(|d| match d {
                        DefaultValue::Trait => quote! { ::std::default::Default::default() },
                        DefaultValue::Expr(e) => quote! { #e },
                    });

                    if is_opt {
                        quote! {
                            let #ident = {
                                let mut k = prefix.clone();
                                k.push(::rumps_types::Subscript::from(#sub_name));
                                all_pairs
                                    .iter()
                                    .find(|(pk, _)| pk == &k)
                                    .and_then(|(_, v)| {
                                        <#inner_ty as ::rumps_types::orm::FromValue>::from_val(v).ok()
                                    })
                            };
                        }
                    } else {
                        quote! {
                            let #ident = {
                                let mut k = prefix.clone();
                                k.push(::rumps_types::Subscript::from(#sub_name));
                                all_pairs
                                    .iter()
                                    .find(|(pk, _)| pk == &k)
                                    .and_then(|(_, v)| {
                                        <#ty as ::rumps_types::orm::FromValue>::from_val(v).ok()
                                    })
                                    .unwrap_or_else(|| #default_expr)
                            };
                        }
                    }
                })
                .collect();

            // Key field extraction from prefix
            let key_parsers: Vec<_> = pf
                .key_fields
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let ident = &f.ident;
                    let ty = &f.ty;
                    quote! {
                        let #ident = prefix.get(#i)
                            .ok_or(())
                            .and_then(|s| {
                                <#ty as ::rumps_types::orm::FromSubscript>::from_sub(s)
                                    .map_err(|_| ())
                            })?;
                    }
                })
                .collect();

            let skip_defaults: Vec<_> = pf
                .skip_fields
                .iter()
                .map(|f| {
                    let ident = &f.ident;
                    quote! { let #ident = ::std::default::Default::default(); }
                })
                .collect();

            let all_idents: Vec<_> = pf
                .key_fields
                .iter()
                .chain(pf.value_fields.iter())
                .chain(pf.skip_fields.iter())
                .map(|f| &f.ident)
                .collect();

            quote! {
                {
                    let try_result: ::std::result::Result<Self, ()> = (|| {
                        #(#key_parsers)*
                        #(#required_parsers)*
                        #(#optional_parsers)*
                        #(#skip_defaults)*
                        ::std::result::Result::Ok(Self::#var_ident { #(#all_idents),* })
                    })();

                    if let ::std::result::Result::Ok(val) = try_result {
                        return ::std::result::Result::Ok(val);
                    }
                }
            }
        }
    }
}

/// Generate a single `from_pairs` match arm for an enum variant.
fn gen_enum_from_pairs_arm(
    v: &VariantInfo,
    rename_all: RenameAll,
) -> TokenStream {
    let var_ident = &v.ident;
    let tag = v.tag_name(rename_all);
    // For struct variant fields, use variant's rename_all if specified, else container's
    let field_rename = v.attrs.rename_all.unwrap_or(rename_all);

    match &v.fields {
        VariantFields::Unit => {
            // Unit variant: just return the variant
            quote! {
                #tag => ::std::result::Result::Ok(Self::#var_ident),
            }
        }
        VariantFields::Tuple(fields) => {
            match fields.len() {
                1 => {
                    // Single-field tuple: value is stored directly at prefix
                    let ty = &fields[0].ty;
                    quote! {
                        #tag => {
                            // Find the value at exactly prefix
                            let val = all_pairs
                                .iter()
                                .find(|(k, _)| k == prefix)
                                .map(|(_, v)| v)
                                .ok_or_else(|| ::rumps_types::orm::DecodeError::MissingField {
                                    field: "0"
                                })?;
                            let field_0 = <#ty as ::rumps_types::orm::FromValue>::from_val(val)?;
                            ::std::result::Result::Ok(Self::#var_ident(field_0))
                        }
                    }
                }
                _ => {
                    // Multi-field tuple: values at numeric indices
                    let field_extractions: Vec<_> = fields
                        .iter()
                        .map(|f| {
                            let idx = f.index;
                            let ty = &f.ty;
                            let var_name = format_ident!("field_{}", idx);
                            let idx_i64 = idx as i64;
                            quote! {
                                let #var_name: #ty = {
                                    let mut k = prefix.clone();
                                    k.push(::rumps_types::Subscript::from(#idx_i64));
                                    let val = all_pairs
                                        .iter()
                                        .find(|(key, _)| key == &k)
                                        .map(|(_, v)| v)
                                        .ok_or_else(|| ::rumps_types::orm::DecodeError::MissingField {
                                            field: stringify!(#idx)
                                        })?;
                                    <#ty as ::rumps_types::orm::FromValue>::from_val(val)?
                                };
                            }
                        })
                        .collect();

                    let field_names: Vec<_> = fields
                        .iter()
                        .map(|f| format_ident!("field_{}", f.index))
                        .collect();

                    quote! {
                        #tag => {
                            #(#field_extractions)*
                            ::std::result::Result::Ok(Self::#var_ident(#(#field_names),*))
                        }
                    }
                }
            }
        }
        VariantFields::Struct(pf) => {
            // Struct variant: process like a regular struct but with tag_prefix
            let field_processing =
                gen_enum_struct_variant_body(pf, var_ident, field_rename);
            quote! {
                #tag => {
                    #field_processing
                }
            }
        }
    }
}

/// Generate the body for parsing a struct variant.
fn gen_enum_struct_variant_body(
    fields: &ParsedFields,
    var_ident: &syn::Ident,
    rename_all: RenameAll,
) -> TokenStream {
    // Key fields extracted from the prefix (after the tag)
    let key_field_extractions: Vec<_> = fields
        .key_fields
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let ident = &f.ident;
            let ty = &f.ty;
            let field_name = ident.to_string();
            // Key fields come after the variant tag in prefix
            let pos = i + 1; // +1 because position 0 is the variant tag
            quote! {
                let #ident: #ty = prefix
                    .get(#pos)
                    .ok_or_else(|| ::rumps_types::orm::DecodeError::MissingField { field: #field_name })
                    .and_then(::rumps_types::orm::FromSubscript::from_sub)?;
            }
        })
        .collect();

    // Value fields
    let value_field_extractions: Vec<_> = fields
        .value_fields
        .iter()
        .map(|f| {
            let ident = &f.ident;
            let ty = &f.ty;
            let sub_name = f.subscript_name(rename_all);
            let field_name = ident.to_string();
            let is_option = is_option_type(ty);
            let inner_ty = extract_option_inner(ty).unwrap_or(ty);

            if is_option {
                quote! {
                    let #ident: #ty = {
                        let mut k = prefix.clone();
                        k.push(::rumps_types::Subscript::from(#sub_name));
                        all_pairs
                            .iter()
                            .find(|(key, _)| key == &k)
                            .map(|(_, v)| <#inner_ty as ::rumps_types::orm::FromValue>::from_val(v))
                            .transpose()?
                    };
                }
            } else {
                match &f.attrs.default {
                    Some(DefaultValue::Trait) => quote! {
                        let #ident: #ty = {
                            let mut k = prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            all_pairs
                                .iter()
                                .find(|(key, _)| key == &k)
                                .map(|(_, v)| <#ty as ::rumps_types::orm::FromValue>::from_val(v))
                                .transpose()?
                                .unwrap_or_default()
                        };
                    },
                    Some(DefaultValue::Expr(expr)) => quote! {
                        let #ident: #ty = {
                            let mut k = prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            all_pairs
                                .iter()
                                .find(|(key, _)| key == &k)
                                .map(|(_, v)| <#ty as ::rumps_types::orm::FromValue>::from_val(v))
                                .transpose()?
                                .unwrap_or_else(|| #expr)
                        };
                    },
                    None => quote! {
                        let #ident: #ty = {
                            let mut k = prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            let val = all_pairs
                                .iter()
                                .find(|(key, _)| key == &k)
                                .map(|(_, v)| v)
                                .ok_or_else(|| ::rumps_types::orm::DecodeError::MissingField {
                                    field: #field_name
                                })?;
                            <#ty as ::rumps_types::orm::FromValue>::from_val(val)?
                        };
                    },
                }
            }
        })
        .collect();

    // Skip fields
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

    // TODO: flatten and subtree fields for enum variants (future enhancement)
    // For now, we don't support flatten/subtree in enum struct variants

    // Build variant constructor
    let all_field_names: Vec<_> = fields
        .key_fields
        .iter()
        .chain(fields.value_fields.iter())
        .chain(fields.skip_fields.iter())
        .map(|f| &f.ident)
        .collect();

    quote! {
        #(#key_field_extractions)*
        #(#value_field_extractions)*
        #(#skip_field_vars)*
        ::std::result::Result::Ok(Self::#var_ident { #(#all_field_names),* })
    }
}
