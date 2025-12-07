//! RUMPS: A reimagining of the MUMPS database system in Rust.
//!
//! This crate provides a unified public API for the RUMPS storage system,
//! re-exporting types from `rumps-types` and `rumps-storage`.
//!
//! # Quick Start
//!
//! ```
//! # tokio_test::block_on(async {
//! use rumps::{Database, global, local, key, Value};
//!
//! // Create an in-memory database
//! let db = Database::in_memory()?;
//!
//! // Locals can be set directly (no transaction needed)
//! db.set(&local!("CACHE"), &key!["user", 123], Value::from("data")).await?;
//!
//! // Globals require transactions
//! db.transaction(|txn| async move {
//!     txn.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Alice")).await?;
//!     txn.set(&global!("PATIENT"), &key![123, "AGE"], Value::from(30)).await?;
//!     Ok(())
//! }).await?;
//!
//! // Read values
//! let name = db.get(&global!("PATIENT"), &key![123, "NAME"]).await?;
//! assert_eq!(name, Some(Value::from("Alice")));
//! # Ok::<(), rumps::Error>(())
//! # });
//! ```
//!
//! # Two Namespaces
//!
//! - **Globals** (`^NAME`): Persistent, backed by disk. Writes require transactions.
//! - **Locals** (`NAME`): Ephemeral, memory-only. Can be modified directly.
//!
//! # Macros
//!
//! - [`global!`] - Create a global variable name: `global!("PATIENT")` → `^PATIENT`
//! - [`local!`] - Create a local variable name: `local!("TEMP")` → `TEMP`
//! - [`key!`] - Create a key path: `key![123, "NAME"]` → `(123, NAME)`
//! - [`json`] - Create JSON values for subscripts or values

// Re-export types from rumps-storage
pub use rumps_storage::{
    // B-tree stats
    BTreeStats,
    // Transactions
    ConflictStrategy,
    // Database
    Database,
    DatabaseBuilder,
    DatabaseStats,
    // Error handling
    Error,
    IsolationLevel,
    // Storage engine
    PageCacheStats,
    Result,
    StorageConfig,
    StorageError,
    StorageMetadata,
    SyncMode,
    Transaction,
    TransactionBuilder,
    TransactionContext,
    TransactionPriority,
    WalWriterConfig,
};
// Re-export types from rumps-types
pub use rumps_types::{
    // Macros
    global,
    json,
    key,
    local,
    // Core types
    DataStatus,
    Key,
    Name,
    Subscript,
    Value,
};

/// ORM-like traits and derive macros for converting Rust types to/from RUMPS storage.
///
/// This module re-exports traits from both `rumps-types::orm` and
/// `rumps-storage::orm` for convenient access, including derive macros.
///
/// # Derive Macros
///
/// With the `derive` feature (enabled by default), you get access to:
///
/// - [`ToRumps`] / [`FromRumps`] - Convert structs/enums to/from RUMPS key-value pairs
/// - [`ToValue`] / [`FromValue`] - Convert unit enums or newtypes to/from RUMPS values
/// - [`ToSubscript`] / [`FromSubscript`] - Convert unit enums or newtypes to/from subscripts
///
/// # Container Attributes
///
/// Attributes on structs or enums:
///
/// - `#[rumps(global = "name")]` - **Required for `ToRumps`/`FromRumps`**. The global name
///   for storage (e.g., `"patient"` for `^patient`).
/// - `#[rumps(rename_all = "case")]` - Apply a naming convention to all fields/variants.
///   Supported: `"snake-case"`, `"camel-case"`, `"pascal-case"`, `"train-case"` (kebab),
///   `"lowercase"`, `"uppercase"`, `"screaming-snake-case"`.
/// - `#[rumps(untagged)]` - **(Enums only)** Don't store variant tag in key; try variants
///   in order during deserialization.
///
/// # Field Attributes
///
/// - `#[rumps(key)]` - Field is part of the key path (not stored as a value).
/// - `#[rumps(key, order = N)]` - Explicit ordering for composite keys.
/// - `#[rumps(flatten)]` - Inline nested struct fields at the current level.
/// - `#[rumps(subtree)]` - Store nested struct as a subtree (adds field name to key path).
/// - `#[rumps(rename = "x")]` - Rename this field. Can be a literal or case convention.
/// - `#[rumps(skip)]` - Don't persist this field (uses `Default::default()` on read).
/// - `#[rumps(default)]` - Use `Default::default()` if field is missing on read.
/// - `#[rumps(default = expr)]` - Use the given expression if field is missing on read.
///
/// # Variant Attributes (for enums)
///
/// - `#[rumps(rename = "x")]` - Rename this variant's tag.
/// - `#[rumps(rename_all = "case")]` - Apply naming convention to fields within this variant.
#[cfg_attr(feature = "derive", doc = include_str!("orm_derive.md"))]
pub mod orm {
    // From rumps-types: primitive conversion traits and error types
    // From rumps-storage: struct conversion traits and extension traits
    // Note: This also re-exports FromRumps/ToRumps derive macros when `derive` feature is enabled
    pub use rumps_storage::orm::{
        FromRumps, RumpsRead, RumpsWrite, Sealed, ToRumps,
    };
    // Re-export remaining derive macros (traits with same names already exported above)
    #[cfg(feature = "derive")]
    pub use rumps_storage::orm::{
        FromSubscript, FromValue, ToSubscript, ToValue,
    };
    pub use rumps_types::orm::{
        DecodeError, FromSubscript, FromValue, IntoKey, ToSubscript, ToValue,
    };
}
