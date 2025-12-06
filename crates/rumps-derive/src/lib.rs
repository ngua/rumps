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
//! # Container Attributes
//!
//! Attributes on structs or enums:
//!
//! - `#[rumps(global = "name")]` - **Required for ToRumps/FromRumps**. The global name for
//!   storage (e.g., `"patient"` for `^patient`).
//! - `#[rumps(rename_all = "case")]` - Apply a naming convention to all fields/variants.
//!   Supported values: `"snake-case"`, `"camel-case"`, `"pascal-case"`, `"train-case"` (kebab),
//!   `"lowercase"`, `"uppercase"`, `"screaming-snake-case"`.
//!
//! # Field Attributes
//!
//! - `#[rumps(key)]` - Field is part of the key path (not stored as a value).
//! - `#[rumps(key, order = N)]` - Explicit ordering for composite keys.
//! - `#[rumps(flatten)]` - Inline nested struct fields at the current level.
//! - `#[rumps(subtree)]` - Store nested struct as a subtree (adds field name to key path).
//! - `#[rumps(rename = "x")]` - Rename this field. Can be:
//!   - A literal string: `rename = "custom_name"` → stores as `"custom_name"`
//!   - A case convention: `rename = "snake-case"` → applies snake_case to field name
//! - `#[rumps(skip)]` - Don't persist this field (uses `Default::default()` on read).
//! - `#[rumps(default)]` - Use `Default::default()` if field is missing on read.
//! - `#[rumps(default = expr)]` - Use the given expression if field is missing on read.
//!
//! # Variant Attributes (for enums)
//!
//! - `#[rumps(rename = "x")]` - Rename this variant's tag. Can be a literal or case convention.
//! - `#[rumps(rename_all = "case")]` - Apply a naming convention to fields within this variant,
//!   overriding the container's `rename_all`.
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
//! ## Renaming with `rename_all`
//!
//! Apply consistent naming conventions to fields and variants:
//!
//! ```ignore
//! // Snake case for struct fields
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "user", rename_all = "camel-case")]
//! struct User {
//!     #[rumps(key)]
//!     user_id: u64,
//!     first_name: String,  // stored as "firstName"
//!     last_name: String,   // stored as "lastName"
//! }
//!
//! // Snake case for enum variants
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "status", rename_all = "snake-case")]
//! enum Status {
//!     IsActive,            // tag: "is_active"
//!     WasCancelled,        // tag: "was_cancelled"
//! }
//!
//! // Unit enum with rename_all
//! #[derive(ToValue, FromValue)]
//! #[rumps(rename_all = "lowercase")]
//! enum Priority {
//!     Low,                 // stored as "low"
//!     High,                // stored as "high"
//! }
//!
//! // Variant-level override
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "event", rename_all = "snake-case")]
//! enum Event {
//!     UserCreated { user_id: u64 },           // fields: "user_id"
//!     #[rumps(rename_all = "camel-case")]
//!     OrderPlaced { order_id: u64 },          // fields: "orderId" (override)
//! }
//!
//! // Field-level case transformation
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "mixed")]
//! struct MixedFields {
//!     #[rumps(key)]
//!     id: u64,
//!     #[rumps(rename = "snake-case")]
//!     SomeFieldName: String,     // -> "some_field_name"
//!     #[rumps(rename = "camel-case")]
//!     another_field: String,     // -> "anotherField"
//!     #[rumps(rename = "custom")]
//!     third: String,             // -> "custom" (literal)
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
//!
//! # Data-Carrying Enums
//!
//! Enums with variants that carry data can derive `ToRumps` and `FromRumps`.
//! This is different from unit enums, which derive `ToValue`/`FromValue`.
//!
//! ## Why Data Enums Can't Be Values
//!
//! A RUMPS `Value` is a single scalar (string, integer, float, boolean, bytes, JSON).
//! Data-carrying enums naturally expand to **multiple** key-value pairs because they
//! need to store:
//! 1. Which variant is active (the "tag")
//! 2. The data within that variant (potentially multiple fields)
//!
//! This is fundamentally a tree structure, not a scalar value. Therefore:
//! - **Unit enums** → derive `ToValue`/`FromValue` (variant name as string)
//! - **Data enums** → derive `ToRumps`/`FromRumps` (tree of key-value pairs)
//!
//! If you need a data enum as a single value, serialize it yourself (e.g., to JSON):
//!
//! ```ignore
//! // Manual implementation for JSON serialization
//! impl ToValue for MyEnum {
//!     fn to_val(&self) -> Value {
//!         Value::Json(serde_json::to_value(self).unwrap())
//!     }
//! }
//! ```
//!
//! ## Basic Data Enum
//!
//! ```ignore
//! use rumps_derive::{ToRumps, FromRumps};
//!
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "status")]
//! enum EmploymentStatus {
//!     Active,                              // Unit variant
//!     OnLeave { reason: String },          // Struct variant
//!     Terminated { date: String, reason: Option<String> },
//! }
//! ```
//!
//! ## Storage Layout
//!
//! The variant tag (name) becomes the first subscript in the key path:
//!
//! | Rust                                      | Storage                           |
//! |-------------------------------------------|-----------------------------------|
//! | `EmploymentStatus::Active`                | `^status["Active"] = ""`          |
//! | `EmploymentStatus::OnLeave { reason }`    | `^status["OnLeave"] = ""`         |
//! |                                           | `^status["OnLeave","reason"] = r` |
//!
//! ## Variant Types
//!
//! ### Unit Variants
//!
//! ```ignore
//! Active,  // Stored as ^global["Active"] = ""
//! ```
//!
//! ### Struct Variants
//!
//! ```ignore
//! OnLeave {
//!     reason: String,
//!     #[rumps(default)]
//!     expected_return: Option<String>,
//! },
//! // Stored as:
//! //   ^global["OnLeave"] = ""
//! //   ^global["OnLeave","reason"] = "sick leave"
//! //   ^global["OnLeave","expected_return"] = "2024-01-15" (if present)
//! ```
//!
//! ### Tuple Variants
//!
//! Single-field tuples store the value directly:
//!
//! ```ignore
//! Error(String),  // ^global["Error"] = "something went wrong"
//! ```
//!
//! Multi-field tuples use numeric indices:
//!
//! ```ignore
//! Point(f64, f64, f64),
//! // Stored as:
//! //   ^global["Point"] = ""
//! //   ^global["Point",0] = 1.0
//! //   ^global["Point",1] = 2.0
//! //   ^global["Point",2] = 3.0
//! ```
//!
//! ## Per-Variant Keys
//!
//! Struct variants can have key fields, making each instance uniquely addressable:
//!
//! ```ignore
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "entity")]
//! enum Entity {
//!     Person {
//!         #[rumps(key)]
//!         id: u64,
//!         name: String,
//!     },
//!     Product {
//!         #[rumps(key)]
//!         sku: String,
//!         name: String,
//!     },
//! }
//!
//! // Entity::Person { id: 123, name: "Alice" }
//! // Key: ["Person", 123]
//! // Storage:
//! //   ^entity["Person",123] = ""
//! //   ^entity["Person",123,"name"] = "Alice"
//! ```
//!
//! ## Variant Rename
//!
//! Use `#[rumps(rename = "...")]` to customize the tag stored in the database:
//!
//! ```ignore
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "event")]
//! enum Event {
//!     #[rumps(rename = "USR")]
//!     UserCreated { id: u64 },
//!
//!     #[rumps(rename = "SYS")]
//!     SystemEvent { code: String },
//! }
//! // UserCreated stored as ^event["USR",...]
//! ```
//!
//! ## Embedded Enums (Subtrees)
//!
//! Enums can be embedded in structs using `#[rumps(subtree)]`:
//!
//! ```ignore
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "worker")]
//! struct Worker {
//!     #[rumps(key)]
//!     id: u64,
//!     name: String,
//!     #[rumps(subtree)]
//!     status: EmploymentStatus,
//! }
//!
//! // Worker { id: 1, name: "Bob", status: OnLeave { reason: "vacation" } }
//! // Storage:
//! //   ^worker[1] = ""
//! //   ^worker[1,"name"] = "Bob"
//! //   ^worker[1,"status","OnLeave"] = ""
//! //   ^worker[1,"status","OnLeave","reason"] = "vacation"
//! ```
//!
//! ## Variant Attributes
//!
//! On enum variants:
//! - `#[rumps(rename = "x")]` - Custom tag name in storage
//!
//! On fields within variants:
//! - `#[rumps(key)]` - Field is part of the key (struct variants only)
//! - `#[rumps(default)]` - Use `Default::default()` if missing
//! - `#[rumps(default = expr)]` - Use expression if missing
//! - `#[rumps(skip)]` - Don't persist this field

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
