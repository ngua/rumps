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

/// ORM-like traits for converting Rust types to/from RUMPS storage.
///
/// This module re-exports traits from both `rumps-types::orm` and
/// `rumps-storage::orm` for convenient access, including derive macros.
///
/// # Example
///
/// ```ignore
/// use rumps::orm::{ToRumps, FromRumps, RumpsRead, RumpsWrite};
///
/// #[derive(ToRumps, FromRumps)]
/// #[rumps(global = "user")]
/// struct User {
///     #[rumps(key)]
///     id: u64,
///     name: String,
/// }
/// ```
pub mod orm {
    // From rumps-types: primitive conversion traits and error types
    // From rumps-storage: struct conversion traits, extension traits, and derive macros
    pub use rumps_storage::orm::{
        FromRumps, RumpsRead, RumpsWrite, Sealed, ToRumps,
    };
    pub use rumps_types::orm::{
        DecodeError, FromSubscript, FromValue, IntoKey, ToSubscript, ToValue,
    };
}
