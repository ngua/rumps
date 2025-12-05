//! Derive macros for RUMPS ORM traits.
//!
//! This crate provides proc macros for automatically implementing the RUMPS ORM traits:
//!
//! - `#[derive(ToRumps)]` - Convert structs to RUMPS key-value pairs
//! - `#[derive(FromRumps)]` - Parse structs from RUMPS key-value pairs
//! - `#[derive(ToValue)]` - Convert unit enums to RUMPS values
//! - `#[derive(FromValue)]` - Parse unit enums from RUMPS values
//! - `#[derive(ToSubscript)]` - Convert unit enums to RUMPS subscripts
//! - `#[derive(FromSubscript)]` - Parse unit enums from RUMPS subscripts
//!
//! # Struct Attributes
//!
//! Container-level attributes on the struct:
//!
//! - `#[rumps(global = "name")]` - **Required**. The global name for storage (e.g., `"patient"`
//!   for `^patient`).
//!
//! # Field Attributes
//!
//! - `#[rumps(key)]` - Field is part of the key path (not stored as a value).
//! - `#[rumps(key, order = N)]` - Explicit ordering for composite keys.
//! - `#[rumps(flatten)]` - Inline nested struct fields at the current level.
//! - `#[rumps(subtree)]` - Store nested struct as a subtree (adds field name to key path).
//! - `#[rumps(rename = "x")]` - Use a custom name for the subscript.
//! - `#[rumps(skip)]` - Don't persist this field (uses `Default::default()` on read).
//! - `#[rumps(default)]` - Use `Default::default()` if field is missing on read.
//! - `#[rumps(default = expr)]` - Use the given expression if field is missing on read.
//!
//! # Examples
//!
//! ## Basic struct
//!
//! ```ignore
//! use rumps_derive::{ToRumps, FromRumps};
//!
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "user")]
//! struct User {
//!     #[rumps(key)]
//!     id: u64,
//!     name: String,
//!     email: String,
//! }
//! ```
//!
//! ## Composite keys
//!
//! ```ignore
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "patient")]
//! struct Patient {
//!     #[rumps(key, order = 0)]
//!     dept: String,
//!     #[rumps(key, order = 1)]
//!     id: u64,
//!     name: String,
//! }
//! ```
//!
//! ## Nested structs
//!
//! ```ignore
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "patient")]
//! struct Patient {
//!     #[rumps(key)]
//!     id: u64,
//!     name: String,
//!
//!     #[rumps(flatten)]   // Fields inlined at same level
//!     address: Address,
//!
//!     #[rumps(subtree)]   // Stored under "contact" subscript
//!     contact: Contact,
//! }
//! ```
//!
//! ## Unit enums
//!
//! ```ignore
//! use rumps_derive::{ToValue, FromValue};
//!
//! #[derive(ToValue, FromValue)]
//! enum Priority {
//!     Low,
//!     Medium,
//!     High,
//! }
//! ```

mod attrs;
mod from_rumps;
mod to_rumps;
mod value;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput};

/// Derive `ToRumps` for a struct.
///
/// This generates an implementation of `rumps_storage::orm::ToRumps` that
/// converts the struct into RUMPS key-value pairs.
///
/// # Requirements
///
/// - Must have `#[rumps(global = "name")]` attribute
/// - At least one field must have `#[rumps(key)]`
/// - Cannot be used on enums, tuple structs, or unit structs
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

/// Derive `FromRumps` for a struct.
///
/// This generates an implementation of `rumps_storage::orm::FromRumps` that
/// reconstructs the struct from RUMPS key-value pairs.
///
/// # Requirements
///
/// - Must have `#[rumps(global = "name")]` attribute
/// - Cannot be used on enums, tuple structs, or unit structs
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

/// Derive `ToValue` for a unit enum.
///
/// This generates an implementation of `rumps_types::orm::ToValue` that
/// converts enum variants to string values (using the variant name).
///
/// # Requirements
///
/// - Must be an enum
/// - All variants must be unit variants (no fields)
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

/// Derive `FromValue` for a unit enum.
///
/// This generates an implementation of `rumps_types::orm::FromValue` that
/// parses enum variants from string values.
///
/// # Requirements
///
/// - Must be an enum
/// - All variants must be unit variants (no fields)
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

/// Derive `ToSubscript` for a unit enum.
///
/// This generates an implementation of `rumps_types::orm::ToSubscript` that
/// converts enum variants to string subscripts (using the variant name).
///
/// # Requirements
///
/// - Must be an enum
/// - All variants must be unit variants (no fields)
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

/// Derive `FromSubscript` for a unit enum.
///
/// This generates an implementation of `rumps_types::orm::FromSubscript` that
/// parses enum variants from string subscripts.
///
/// # Requirements
///
/// - Must be an enum
/// - All variants must be unit variants (no fields)
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
