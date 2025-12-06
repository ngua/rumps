//! Derive macros for RUMPS ORM traits.
//!
//! This crate provides proc macros for automatically implementing the RUMPS ORM traits.
//!
//! **For full documentation with examples, see [`rumps::orm`](https://docs.rs/rumps/latest/rumps/orm/).**
//!
//! # Available Derives
//!
//! - `#[derive(ToRumps)]` / `#[derive(FromRumps)]` - Structs/enums to RUMPS key-value pairs
//! - `#[derive(ToValue)]` / `#[derive(FromValue)]` - Unit enums or newtypes to RUMPS values
//! - `#[derive(ToSubscript)]` / `#[derive(FromSubscript)]` - Unit enums or newtypes to subscripts

mod attrs;
mod from_rumps;
mod to_rumps;
mod value;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive `ToRumps` for a struct or newtype.
///
/// This generates an implementation of `rumps_storage::orm::ToRumps` that
/// converts the struct into RUMPS key-value pairs.
///
/// # Named Structs
///
/// For named structs:
/// - Must have `#[rumps(global = "name")]` attribute
/// - At least one field must have `#[rumps(key)]`
///
/// # Newtype Structs
///
/// For newtypes, delegates to the inner type's `ToRumps` impl for key/pairs
/// generation, but requires an explicit `global` to prevent accidental
/// data overwrites:
///
/// ```ignore
/// #[derive(ToRumps)]
/// #[rumps(global = "wrapped_user")]  // Required - separate storage
/// struct WrappedUser(User);
///
/// #[derive(ToRumps)]
/// #[rumps(global = "user")]  // Shares storage with User
/// struct UserAlias(User);
/// ```
///
/// # Example
///
/// ```ignore
/// #[derive(ToRumps)]
/// #[rumps(global = "user")]
/// struct User {
///     #[rumps(key)]
///     id: u64,
///     name: String,
///     age: u32,
/// }
/// ```
#[proc_macro_derive(ToRumps, attributes(rumps))]
pub fn derive_to_rumps(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    to_rumps::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `FromRumps` for a struct or newtype.
///
/// This generates an implementation of `rumps_storage::orm::FromRumps` that
/// reconstructs the struct from RUMPS key-value pairs.
///
/// # Named Structs
///
/// For named structs:
/// - Must have `#[rumps(global = "name")]` attribute
///
/// # Newtype Structs
///
/// For newtypes, delegates to the inner type's `FromRumps` impl for parsing,
/// but requires an explicit `global` to prevent reading from wrong storage:
///
/// ```ignore
/// #[derive(FromRumps)]
/// #[rumps(global = "wrapped_user")]  // Required
/// struct WrappedUser(User);
/// ```
///
/// # Example
///
/// ```ignore
/// #[derive(FromRumps)]
/// #[rumps(global = "user")]
/// struct User {
///     #[rumps(key)]
///     id: u64,
///     name: String,
///     #[rumps(default)]
///     age: u32,
/// }
/// ```
#[proc_macro_derive(FromRumps, attributes(rumps))]
pub fn derive_from_rumps(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    from_rumps::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `ToValue` for a unit enum or newtype struct.
///
/// This generates an implementation of `rumps_types::orm::ToValue`.
///
/// For unit enums, variant names are converted to string values.
/// For newtypes, delegates to the inner type's `ToValue` impl.
///
/// # Requirements
///
/// - Unit enum: all variants must have no fields
/// - Newtype: single-field tuple struct where the field implements `ToValue`
///
/// # Example
///
/// ```ignore
/// #[derive(ToValue)]
/// enum Status {
///     Active,
///     Inactive,
///     Pending,
/// }
///
/// // Status::Active.to_val() == Value::String("Active".into())
/// ```
#[proc_macro_derive(ToValue, attributes(rumps))]
pub fn derive_to_value(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    value::expand_to_value(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `FromValue` for a unit enum or newtype struct.
///
/// This generates an implementation of `rumps_types::orm::FromValue`.
///
/// For unit enums, parses variant names from string values.
/// For newtypes, delegates to the inner type's `FromValue` impl.
///
/// # Requirements
///
/// - Unit enum: all variants must have no fields
/// - Newtype: single-field tuple struct where the field implements `FromValue`
///
/// # Example
///
/// ```ignore
/// #[derive(FromValue)]
/// enum Status {
///     Active,
///     Inactive,
///     Pending,
/// }
///
/// // Status::from_val(&Value::String("Active".into())) == Ok(Status::Active)
/// ```
#[proc_macro_derive(FromValue, attributes(rumps))]
pub fn derive_from_value(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    value::expand_from_value(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `ToSubscript` for a unit enum or newtype struct.
///
/// This generates an implementation of `rumps_types::orm::ToSubscript`.
///
/// For unit enums, variant names are converted to string subscripts.
/// For newtypes, delegates to the inner type's `ToSubscript` impl.
///
/// # Requirements
///
/// - Unit enum: all variants must have no fields
/// - Newtype: single-field tuple struct where the field implements `ToSubscript`
///
/// # Example
///
/// ```ignore
/// #[derive(ToSubscript)]
/// enum Priority {
///     Low,
///     Medium,
///     High,
/// }
///
/// // Priority::High.to_sub() == Subscript::String("High".into())
/// ```
#[proc_macro_derive(ToSubscript, attributes(rumps))]
pub fn derive_to_subscript(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    value::expand_to_subscript(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive `FromSubscript` for a unit enum or newtype struct.
///
/// This generates an implementation of `rumps_types::orm::FromSubscript`.
///
/// For unit enums, parses variant names from string subscripts.
/// For newtypes, delegates to the inner type's `FromSubscript` impl.
///
/// # Requirements
///
/// - Unit enum: all variants must have no fields
/// - Newtype: single-field tuple struct where the field implements `FromSubscript`
///
/// # Example
///
/// ```ignore
/// #[derive(FromSubscript)]
/// enum Priority {
///     Low,
///     Medium,
///     High,
/// }
///
/// // Priority::from_sub(&Subscript::String("High".into())) == Ok(Priority::High)
/// ```
#[proc_macro_derive(FromSubscript, attributes(rumps))]
pub fn derive_from_subscript(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    value::expand_from_subscript(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
