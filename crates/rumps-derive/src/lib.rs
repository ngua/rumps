//! Derive macros for RUMPS ORM traits.
//!
//! This crate provides proc macros for automatically implementing the RUMPS ORM traits:
//!
//! - `#[derive(ToRumps)]` - Convert structs to RUMPS key-value pairs
//! - `#[derive(FromRumps)]` - Parse structs from RUMPS key-value pairs
//! - `#[derive(ToValue)]` - Convert unit enums or newtypes to RUMPS values
//! - `#[derive(FromValue)]` - Parse unit enums or newtypes from RUMPS values
//! - `#[derive(ToSubscript)]` - Convert unit enums or newtypes to RUMPS subscripts
//! - `#[derive(FromSubscript)]` - Parse unit enums or newtypes from RUMPS subscripts
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
//!
//! ## Newtype structs
//!
//! Newtypes transparently delegate to their inner type:
//!
//! ```ignore
//! use rumps_derive::{ToValue, FromValue, ToSubscript, FromSubscript};
//!
//! #[derive(ToValue, FromValue)]
//! struct UserId(u64);
//!
//! #[derive(ToSubscript, FromSubscript)]
//! struct Email(String);
//! ```

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
