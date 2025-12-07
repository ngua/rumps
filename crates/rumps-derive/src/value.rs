//! Implementation of `ToValue`/`FromValue` and `ToSubscript`/`FromSubscript`
//! derive macros for unit enums and newtype structs.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Fields};

use crate::attrs::{ContainerAttrs, RenameAll, VariantAttrs};

/// Get the serialized name for a variant.
///
/// If `#[rumps(rename = "...")]` is set:
/// - If it's a case convention (e.g., `"snake-case"`), apply that to the variant name
/// - Otherwise, use it as a literal string
///
/// If no rename is set, apply `rename_all` to the variant name.
fn variant_name(
    ident: &str,
    attrs: &VariantAttrs,
    rename_all: RenameAll,
) -> String {
    match &attrs.rename {
        Some(r) => RenameAll::from_str(r)
            .map(|case| case.apply(ident))
            .unwrap_or_else(|| r.clone()),
        None => rename_all.apply(ident),
    }
}

/// Expand `#[derive(ToValue)]` for unit enums or newtype structs.
pub fn expand_to_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    match &input.data {
        syn::Data::Enum(data) => {
            let container = ContainerAttrs::from_attrs(&input.attrs)?;
            let rename_all = container.rename_all;

            data.variants
                .iter()
                .try_fold(Vec::new(), |mut arms, v| match &v.fields {
                    Fields::Unit => {
                        let var = &v.ident;
                        let vattrs = VariantAttrs::from_attrs(&v.attrs)?;
                        let s = variant_name(&var.to_string(), &vattrs, rename_all);
                        arms.push(
                            quote! { Self::#var => ::rumps_types::Value::String(#s.into()), },
                        );
                        Ok(arms)
                    }
                    _ => Err(syn::Error::new_spanned(
                        v,
                        "ToValue cannot be derived for enums with data variants. \
                         Data enums expand to multiple key-value pairs; use ToRumps instead. \
                         For custom serialization (e.g., JSON), implement ToValue manually.",
                    )),
                })
                .map(|arms| {
                    quote! {
                        impl #impl_generics ::rumps_types::orm::ToValue for #name #ty_generics #where_clause {
                            fn to_val(&self) -> ::rumps_types::Value {
                                match self { #(#arms)* }
                            }
                        }
                    }
                })
        }

        syn::Data::Struct(data) => match &data.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f
                .unnamed
                .first()
                .ok_or_else(|| syn::Error::new_spanned(input, "expected single field"))
                .map(|field| {
                    let inner = &field.ty;
                    quote! {
                        impl #impl_generics ::rumps_types::orm::ToValue for #name #ty_generics #where_clause {
                            fn to_val(&self) -> ::rumps_types::Value {
                                <#inner as ::rumps_types::orm::ToValue>::to_val(&self.0)
                            }
                        }
                    }
                }),
            _ => Err(syn::Error::new_spanned(
                input,
                "ToValue can only be derived for newtype structs (single-field tuple structs)",
            )),
        },

        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "ToValue cannot be derived for unions",
        )),
    }
}

/// Expand `#[derive(FromValue)]` for unit enums or newtype structs.
pub fn expand_from_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    match &input.data {
        syn::Data::Enum(data) => {
            let container = ContainerAttrs::from_attrs(&input.attrs)?;
            let rename_all = container.rename_all;

            // Collect expected variant names for error message
            let expected: Vec<String> = data
                .variants
                .iter()
                .filter_map(|v| {
                    VariantAttrs::from_attrs(&v.attrs)
                        .ok()
                        .map(|vattrs| variant_name(&v.ident.to_string(), &vattrs, rename_all))
                })
                .collect();
            let expected_msg = expected.join(", ");

            data.variants
                .iter()
                .try_fold(Vec::new(), |mut arms, v| match &v.fields {
                    Fields::Unit => {
                        let var = &v.ident;
                        let vattrs = VariantAttrs::from_attrs(&v.attrs)?;
                        let s = variant_name(&var.to_string(), &vattrs, rename_all);
                        arms.push(quote! { #s => ::std::result::Result::Ok(Self::#var), });
                        Ok(arms)
                    }
                    _ => Err(syn::Error::new_spanned(
                        v,
                        "FromValue cannot be derived for enums with data variants. \
                         Data enums expand to multiple key-value pairs; use FromRumps instead. \
                         For custom deserialization (e.g., JSON), implement FromValue manually.",
                    )),
                })
                .map(|arms| {
                    quote! {
                        impl #impl_generics ::rumps_types::orm::FromValue for #name #ty_generics #where_clause {
                            fn from_val(v: &::rumps_types::Value) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                                match v {
                                    ::rumps_types::Value::String(s) => match s.as_str() {
                                        #(#arms)*
                                        other => ::std::result::Result::Err(::rumps_types::orm::DecodeError::Custom(
                                            ::std::format!("unknown {} variant: `{}` (expected one of: {})", #name_str, other, #expected_msg)
                                        )),
                                    },
                                    other => ::std::result::Result::Err(::rumps_types::orm::DecodeError::WrongValueType {
                                        expected: "String",
                                        actual: ::std::format!("{:?}", other),
                                    }),
                                }
                            }
                        }
                    }
                })
        }

        syn::Data::Struct(data) => match &data.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f
                .unnamed
                .first()
                .ok_or_else(|| syn::Error::new_spanned(input, "expected single field"))
                .map(|field| {
                    let inner = &field.ty;
                    quote! {
                        impl #impl_generics ::rumps_types::orm::FromValue for #name #ty_generics #where_clause {
                            fn from_val(v: &::rumps_types::Value) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                                <#inner as ::rumps_types::orm::FromValue>::from_val(v).map(Self)
                            }
                        }
                    }
                }),
            _ => Err(syn::Error::new_spanned(
                input,
                "FromValue can only be derived for newtype structs (single-field tuple structs)",
            )),
        },

        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "FromValue cannot be derived for unions",
        )),
    }
}

/// Expand `#[derive(ToSubscript)]` for unit enums or newtype structs.
pub fn expand_to_subscript(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    match &input.data {
        syn::Data::Enum(data) => {
            let container = ContainerAttrs::from_attrs(&input.attrs)?;
            let rename_all = container.rename_all;

            data.variants
                .iter()
                .try_fold(Vec::new(), |mut arms, v| match &v.fields {
                    Fields::Unit => {
                        let var = &v.ident;
                        let vattrs = VariantAttrs::from_attrs(&v.attrs)?;
                        let s = variant_name(&var.to_string(), &vattrs, rename_all);
                        arms.push(
                            quote! { Self::#var => ::rumps_types::Subscript::String(#s.into()), },
                        );
                        Ok(arms)
                    }
                    _ => Err(syn::Error::new_spanned(
                        v,
                        "ToSubscript cannot be derived for enums with data variants. \
                         Data enums expand to multiple key-value pairs; use ToRumps instead.",
                    )),
                })
                .map(|arms| {
                    quote! {
                        impl #impl_generics ::rumps_types::orm::ToSubscript for #name #ty_generics #where_clause {
                            fn to_sub(&self) -> ::rumps_types::Subscript {
                                match self { #(#arms)* }
                            }
                        }
                    }
                })
        }

        syn::Data::Struct(data) => match &data.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f
                .unnamed
                .first()
                .ok_or_else(|| syn::Error::new_spanned(input, "expected single field"))
                .map(|field| {
                    let inner = &field.ty;
                    quote! {
                        impl #impl_generics ::rumps_types::orm::ToSubscript for #name #ty_generics #where_clause {
                            fn to_sub(&self) -> ::rumps_types::Subscript {
                                <#inner as ::rumps_types::orm::ToSubscript>::to_sub(&self.0)
                            }
                        }
                    }
                }),
            _ => Err(syn::Error::new_spanned(
                input,
                "ToSubscript can only be derived for newtype structs (single-field tuple structs)",
            )),
        },

        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "ToSubscript cannot be derived for unions",
        )),
    }
}

/// Expand `#[derive(FromSubscript)]` for unit enums or newtype structs.
pub fn expand_from_subscript(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    match &input.data {
        syn::Data::Enum(data) => {
            let container = ContainerAttrs::from_attrs(&input.attrs)?;
            let rename_all = container.rename_all;

            // Collect expected variant names for error message
            let expected: Vec<String> = data
                .variants
                .iter()
                .filter_map(|v| {
                    VariantAttrs::from_attrs(&v.attrs)
                        .ok()
                        .map(|vattrs| variant_name(&v.ident.to_string(), &vattrs, rename_all))
                })
                .collect();
            let expected_msg = expected.join(", ");

            data.variants
                .iter()
                .try_fold(Vec::new(), |mut arms, v| match &v.fields {
                    Fields::Unit => {
                        let var = &v.ident;
                        let vattrs = VariantAttrs::from_attrs(&v.attrs)?;
                        let s = variant_name(&var.to_string(), &vattrs, rename_all);
                        arms.push(quote! { #s => ::std::result::Result::Ok(Self::#var), });
                        Ok(arms)
                    }
                    _ => Err(syn::Error::new_spanned(
                        v,
                        "FromSubscript cannot be derived for enums with data variants. \
                         Data enums expand to multiple key-value pairs; use FromRumps instead.",
                    )),
                })
                .map(|arms| {
                    quote! {
                        impl #impl_generics ::rumps_types::orm::FromSubscript for #name #ty_generics #where_clause {
                            fn from_sub(s: &::rumps_types::Subscript) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                                match s {
                                    ::rumps_types::Subscript::String(st) => match st.as_str() {
                                        #(#arms)*
                                        other => ::std::result::Result::Err(::rumps_types::orm::DecodeError::Custom(
                                            ::std::format!("unknown {} variant: `{}` (expected one of: {})", #name_str, other, #expected_msg)
                                        )),
                                    },
                                    other => ::std::result::Result::Err(::rumps_types::orm::DecodeError::WrongSubscriptType {
                                        expected: "String",
                                        actual: ::std::format!("{:?}", other),
                                    }),
                                }
                            }
                        }
                    }
                })
        }

        syn::Data::Struct(data) => match &data.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f
                .unnamed
                .first()
                .ok_or_else(|| syn::Error::new_spanned(input, "expected single field"))
                .map(|field| {
                    let inner = &field.ty;
                    quote! {
                        impl #impl_generics ::rumps_types::orm::FromSubscript for #name #ty_generics #where_clause {
                            fn from_sub(s: &::rumps_types::Subscript) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                                <#inner as ::rumps_types::orm::FromSubscript>::from_sub(s).map(Self)
                            }
                        }
                    }
                }),
            _ => Err(syn::Error::new_spanned(
                input,
                "FromSubscript can only be derived for newtype structs (single-field tuple structs)",
            )),
        },

        syn::Data::Union(_) => Err(syn::Error::new_spanned(
            input,
            "FromSubscript cannot be derived for unions",
        )),
    }
}
