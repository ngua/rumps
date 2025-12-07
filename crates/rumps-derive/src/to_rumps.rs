//! Implementation of the `ToRumps` derive macro.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{DeriveInput, Fields};

use crate::attrs::{
    parse_variants, ContainerAttrs, ParsedFields, RenameAll, VariantFields,
    VariantInfo,
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
                            impl #impl_generics ::rumps_storage::orm::ToRumps for #name #ty_generics #where_clause {
                                const GLOBAL: &'static str = #global;

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
    fields.require_key_field(f.span())?;
    let rename_all = container.rename_all;

    let to_key_body = gen_to_key(&fields);
    let to_pairs_body = gen_to_pairs(&fields, rename_all);

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::ToRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;

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
fn gen_to_pairs(fields: &ParsedFields, rename_all: RenameAll) -> TokenStream {
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
        let sub_name = f.subscript_name(rename_all);
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
        let sub_name = f.subscript_name(rename_all);
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

/// Expand `ToRumps` derive for enums.
fn expand_enum(
    name: &syn::Ident,
    impl_generics: syn::ImplGenerics,
    ty_generics: syn::TypeGenerics,
    where_clause: Option<&syn::WhereClause>,
    global: &syn::LitStr,
    variants: &[VariantInfo],
    container: &ContainerAttrs,
) -> syn::Result<TokenStream> {
    let rename_all = container.rename_all;
    let untagged = container.untagged;

    let to_key_arms = variants
        .iter()
        .map(|v| gen_enum_to_key_arm(v, rename_all, untagged))
        .collect::<Vec<_>>();

    let to_pairs_arms = variants
        .iter()
        .map(|v| gen_enum_to_pairs_arm(v, rename_all, untagged))
        .collect::<Vec<_>>();

    Ok(quote! {
        impl #impl_generics ::rumps_storage::orm::ToRumps for #name #ty_generics #where_clause {
            const GLOBAL: &'static str = #global;

            fn to_key(&self) -> ::rumps_types::Key {
                match self {
                    #(#to_key_arms)*
                }
            }

            fn to_pairs(&self, prefix: &::rumps_types::Key) -> ::std::vec::Vec<(::rumps_types::Key, ::rumps_types::Value)> {
                match self {
                    #(#to_pairs_arms)*
                }
            }
        }
    })
}

/// Generate a single `to_key` match arm for an enum variant.
fn gen_enum_to_key_arm(
    v: &VariantInfo,
    rename_all: RenameAll,
    untagged: bool,
) -> TokenStream {
    let var_ident = &v.ident;
    let tag = v.tag_name(rename_all);

    match &v.fields {
        VariantFields::Unit => {
            if untagged {
                // Untagged unit: empty key
                quote! {
                    Self::#var_ident => ::rumps_types::Key::new(),
                }
            } else {
                quote! {
                    Self::#var_ident => ::rumps_types::Key::from(
                        ::std::vec![::rumps_types::Subscript::from(#tag)]
                    ),
                }
            }
        }
        VariantFields::Tuple(fields) => {
            let bindings: Vec<_> = fields
                .iter()
                .map(|f| {
                    let idx = syn::Index::from(f.index);
                    quote::format_ident!("__f{}", idx)
                })
                .collect();

            // For tuple variants, key fields are marked with #[rumps(key)]
            let key_subs: Vec<_> = fields
                .iter()
                .zip(bindings.iter())
                .filter(|(f, _)| f.attrs.key)
                .map(|(_, b)| quote! { ::rumps_types::orm::ToSubscript::to_sub(#b) })
                .collect();

            let all_subs = if untagged {
                // Untagged: only key fields
                key_subs
            } else {
                std::iter::once(quote! { ::rumps_types::Subscript::from(#tag) })
                    .chain(key_subs)
                    .collect::<Vec<_>>()
            };

            quote! {
                Self::#var_ident(#(#bindings),*) => ::rumps_types::Key::from(
                    ::std::vec![#(#all_subs),*]
                ),
            }
        }
        VariantFields::Struct(pf) => {
            let field_idents: Vec<_> =
                pf.key_fields.iter().map(|f| &f.ident).collect();
            let key_subs: Vec<_> = pf
                .key_fields
                .iter()
                .map(|f| {
                    let ident = &f.ident;
                    quote! { ::rumps_types::orm::ToSubscript::to_sub(#ident) }
                })
                .collect();

            let all_subs = if untagged {
                // Untagged: only key fields
                key_subs
            } else {
                std::iter::once(quote! { ::rumps_types::Subscript::from(#tag) })
                    .chain(key_subs)
                    .collect::<Vec<_>>()
            };

            // Handle empty key fields - can't do `{ , .. }`
            let pattern = if field_idents.is_empty() {
                quote! { Self::#var_ident { .. } }
            } else {
                quote! { Self::#var_ident { #(#field_idents),*, .. } }
            };

            quote! {
                #pattern => ::rumps_types::Key::from(
                    ::std::vec![#(#all_subs),*]
                ),
            }
        }
    }
}

/// Generate a single `to_pairs` match arm for an enum variant.
///
/// Handles both top-level and embedded usage:
/// - Top-level: `insert` calls `to_pairs(to_key())` where `to_key()` = `[tag, key_fields...]`
/// - Embedded: parent calls `to_pairs(subtree_prefix)` where prefix does NOT include tag
///
/// We detect which case by checking if `prefix.get(0)` equals the variant's tag.
fn gen_enum_to_pairs_arm(
    v: &VariantInfo,
    rename_all: RenameAll,
    untagged: bool,
) -> TokenStream {
    let var_ident = &v.ident;
    let tag = v.tag_name(rename_all);
    // For struct variant fields, use variant's rename_all if specified, else container's
    let field_rename = v.attrs.rename_all.unwrap_or(rename_all);

    // Generate the prefix computation that handles both top-level and embedded cases
    let prefix_setup = if untagged {
        // Untagged: no tag in prefix, just use prefix directly
        quote! {
            let effective_prefix = prefix.clone();
        }
    } else {
        quote! {
            let tag_sub = ::rumps_types::Subscript::from(#tag);
            // Check if prefix already starts with this variant's tag (top-level case)
            // vs needs the tag added (embedded case)
            let effective_prefix = if prefix.get(0) == ::std::option::Option::Some(&tag_sub) {
                prefix.clone()
            } else {
                let mut p = prefix.clone();
                p.push(tag_sub);
                p
            };
        }
    };

    match &v.fields {
        VariantFields::Unit => {
            // Unit variant: just marker at effective_prefix
            quote! {
                Self::#var_ident => {
                    #prefix_setup
                    ::std::vec![(effective_prefix, ::rumps_types::Value::String(::std::string::String::new()))]
                }
            }
        }
        VariantFields::Tuple(fields) => {
            let bindings: Vec<_> = fields
                .iter()
                .map(|f| {
                    let idx = syn::Index::from(f.index);
                    quote::format_ident!("__f{}", idx)
                })
                .collect();

            match fields.len() {
                1 => {
                    // Single-field tuple: store value directly at effective_prefix
                    let binding = &bindings[0];
                    quote! {
                        Self::#var_ident(#binding) => {
                            #prefix_setup
                            ::std::vec![(effective_prefix, ::rumps_types::orm::ToValue::to_val(#binding))]
                        }
                    }
                }
                _ => {
                    // Multi-field tuple: use numeric indices under effective_prefix
                    let stmts: Vec<_> = fields
                        .iter()
                        .zip(bindings.iter())
                        .filter(|(f, _)| !f.attrs.key) // Skip key fields in pairs
                        .map(|(f, b)| {
                            let idx = f.index;
                            quote! {
                                {
                                    let mut k = effective_prefix.clone();
                                    k.push(::rumps_types::Subscript::from(#idx as i64));
                                    pairs.push((k, ::rumps_types::orm::ToValue::to_val(#b)));
                                }
                            }
                        })
                        .collect();

                    quote! {
                        Self::#var_ident(#(#bindings),*) => {
                            #prefix_setup
                            let mut pairs = ::std::vec::Vec::new();
                            pairs.push((effective_prefix.clone(), ::rumps_types::Value::String(::std::string::String::new())));
                            #(#stmts)*
                            pairs
                        }
                    }
                }
            }
        }
        VariantFields::Struct(pf) => {
            // Collect field idents for destructuring (excluding key_fields - they're in the prefix)
            let all_field_idents: Vec<_> = pf
                .value_fields
                .iter()
                .chain(pf.flatten_fields.iter())
                .chain(pf.subtree_fields.iter())
                .map(|f| &f.ident)
                .collect();

            let mut stmts = Vec::new();

            // Value fields - use effective_prefix
            pf.value_fields.iter().for_each(|f| {
                let ident = &f.ident;
                let sub_name = f.subscript_name(field_rename);
                let ty = &f.ty;
                let is_option = is_option_type(ty);

                let stmt = if is_option {
                    quote! {
                        #ident.as_ref().into_iter().for_each(|v| {
                            let mut k = effective_prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            pairs.push((k, ::rumps_types::orm::ToValue::to_val(v)));
                        });
                    }
                } else {
                    quote! {
                        {
                            let mut k = effective_prefix.clone();
                            k.push(::rumps_types::Subscript::from(#sub_name));
                            pairs.push((k, ::rumps_types::orm::ToValue::to_val(#ident)));
                        }
                    }
                };
                stmts.push(stmt);
            });

            // Flatten fields - use effective_prefix
            pf.flatten_fields.iter().for_each(|f| {
                let ident = &f.ident;
                let ty = &f.ty;
                let is_option = is_option_type(ty);

                let stmt = if is_option {
                    quote! {
                        #ident.as_ref().into_iter().for_each(|v| {
                            pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(v, &effective_prefix));
                        });
                    }
                } else {
                    quote! {
                        pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(#ident, &effective_prefix));
                    }
                };
                stmts.push(stmt);
            });

            // Subtree fields - use effective_prefix
            pf.subtree_fields.iter().for_each(|f| {
                let ident = &f.ident;
                let sub_name = f.subscript_name(field_rename);
                let ty = &f.ty;
                let is_option = is_option_type(ty);

                let stmt = if is_option {
                    quote! {
                        #ident.as_ref().into_iter().for_each(|v| {
                            let mut sub_prefix = effective_prefix.clone();
                            sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                            pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(v, &sub_prefix));
                        });
                    }
                } else {
                    quote! {
                        {
                            let mut sub_prefix = effective_prefix.clone();
                            sub_prefix.push(::rumps_types::Subscript::from(#sub_name));
                            pairs.extend(::rumps_storage::orm::ToRumps::to_pairs(#ident, &sub_prefix));
                        }
                    }
                };
                stmts.push(stmt);
            });

            // Handle empty field lists - can't do `{ , .. }`
            let pattern = if all_field_idents.is_empty() {
                quote! { Self::#var_ident { .. } }
            } else {
                quote! { Self::#var_ident { #(#all_field_idents),*, .. } }
            };

            quote! {
                #pattern => {
                    #prefix_setup
                    let mut pairs = ::std::vec::Vec::new();
                    pairs.push((effective_prefix.clone(), ::rumps_types::Value::String(::std::string::String::new())));
                    #(#stmts)*
                    pairs
                }
            }
        }
    }
}
