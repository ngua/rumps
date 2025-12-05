//! Implementation of `ToValue` and `FromValue` derive macros for unit enums.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, Fields};

/// Expand `#[derive(ToValue)]` for unit enums.
pub fn expand_to_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "ToValue can only be derived for enums",
            ))
        }
    };

    // Verify all variants are unit variants
    variants.iter().try_for_each(|v| {
        match &v.fields {
            Fields::Unit => Ok(()),
            _ => Err(syn::Error::new_spanned(
                v,
                "ToValue can only be derived for unit enums (variants without fields)",
            )),
        }
    })?;

    let match_arms: Vec<_> = variants
        .iter()
        .map(|v| {
            let variant = &v.ident;
            let variant_str = variant.to_string();
            quote! {
                Self::#variant => ::rumps_types::Value::String(::std::string::String::from(#variant_str)),
            }
        })
        .collect();

    Ok(quote! {
        impl #impl_generics ::rumps_types::orm::ToValue for #name #ty_generics #where_clause {
            fn to_val(&self) -> ::rumps_types::Value {
                match self {
                    #(#match_arms)*
                }
            }
        }
    })
}

/// Expand `#[derive(FromValue)]` for unit enums.
pub fn expand_from_value(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "FromValue can only be derived for enums",
            ))
        }
    };

    // Verify all variants are unit variants
    variants.iter().try_for_each(|v| {
        match &v.fields {
            Fields::Unit => Ok(()),
            _ => Err(syn::Error::new_spanned(
                v,
                "FromValue can only be derived for unit enums (variants without fields)",
            )),
        }
    })?;

    let match_arms: Vec<_> = variants
        .iter()
        .map(|v| {
            let variant = &v.ident;
            let variant_str = variant.to_string();
            quote! {
                #variant_str => ::std::result::Result::Ok(Self::#variant),
            }
        })
        .collect();

    let variant_names: Vec<_> =
        variants.iter().map(|v| v.ident.to_string()).collect();
    let expected_msg = variant_names.join(", ");

    Ok(quote! {
        impl #impl_generics ::rumps_types::orm::FromValue for #name #ty_generics #where_clause {
            fn from_val(v: &::rumps_types::Value) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                match v {
                    ::rumps_types::Value::String(s) => match s.as_str() {
                        #(#match_arms)*
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
    })
}

/// Expand `#[derive(ToSubscript)]` for unit enums.
pub fn expand_to_subscript(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "ToSubscript can only be derived for enums",
            ))
        }
    };

    // Verify all variants are unit variants
    variants.iter().try_for_each(|v| {
        match &v.fields {
            Fields::Unit => Ok(()),
            _ => Err(syn::Error::new_spanned(
                v,
                "ToSubscript can only be derived for unit enums (variants without fields)",
            )),
        }
    })?;

    let match_arms: Vec<_> = variants
        .iter()
        .map(|v| {
            let variant = &v.ident;
            let variant_str = variant.to_string();
            quote! {
                Self::#variant => ::rumps_types::Subscript::String(::std::string::String::from(#variant_str)),
            }
        })
        .collect();

    Ok(quote! {
        impl #impl_generics ::rumps_types::orm::ToSubscript for #name #ty_generics #where_clause {
            fn to_sub(&self) -> ::rumps_types::Subscript {
                match self {
                    #(#match_arms)*
                }
            }
        }
    })
}

/// Expand `#[derive(FromSubscript)]` for unit enums.
pub fn expand_from_subscript(input: &DeriveInput) -> syn::Result<TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) =
        input.generics.split_for_impl();

    let variants = match &input.data {
        syn::Data::Enum(data) => &data.variants,
        _ => {
            return Err(syn::Error::new_spanned(
                input,
                "FromSubscript can only be derived for enums",
            ))
        }
    };

    // Verify all variants are unit variants
    variants.iter().try_for_each(|v| {
        match &v.fields {
            Fields::Unit => Ok(()),
            _ => Err(syn::Error::new_spanned(
                v,
                "FromSubscript can only be derived for unit enums (variants without fields)",
            )),
        }
    })?;

    let match_arms: Vec<_> = variants
        .iter()
        .map(|v| {
            let variant = &v.ident;
            let variant_str = variant.to_string();
            quote! {
                #variant_str => ::std::result::Result::Ok(Self::#variant),
            }
        })
        .collect();

    let variant_names: Vec<_> =
        variants.iter().map(|v| v.ident.to_string()).collect();
    let expected_msg = variant_names.join(", ");

    Ok(quote! {
        impl #impl_generics ::rumps_types::orm::FromSubscript for #name #ty_generics #where_clause {
            fn from_sub(s: &::rumps_types::Subscript) -> ::std::result::Result<Self, ::rumps_types::orm::DecodeError> {
                match s {
                    ::rumps_types::Subscript::String(st) => match st.as_str() {
                        #(#match_arms)*
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
    })
}
