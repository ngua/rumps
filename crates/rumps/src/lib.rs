//! RUMPS: A reimagining of the MUMPS database system in Rust.
//!
//! This crate provides a unified public API for the RUMPS storage system,
//! re-exporting types from `rumps-types` and `rumps-storage`.
//!
//! # Quick Start
//!
//! ```
//! # tokio_test::block_on(async {
//! use rumps::{Database, global, local, key, value};
//!
//! // Create an in-memory database
//! let db = Database::in_memory()?;
//!
//! // Locals can be set directly (no transaction needed)
//! db.set(&local!("CACHE"), &key!["user", 123], value!("data")).await?;
//!
//! // Globals require transactions
//! db.transaction(|txn| async move {
//!     txn.set(&global!("PATIENT"), &key![123, "NAME"], value!("Alice")).await?;
//!     txn.set(&global!("PATIENT"), &key![123, "AGE"], value!(30)).await?;
//!     Ok(())
//! }).await?;
//!
//! // Read values
//! let name = db.get(&global!("PATIENT"), &key![123, "NAME"]).await?;
//! assert_eq!(name, Some(value!("Alice")));
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
//! - [`value!`] - Create a value: `value!(42)` → `Value::Integer(42)`
//! - [`json`] - Create JSON values for subscripts or values (re-exported from `serde_json`)

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
    // Storage engine
    DirtyPageStats,
    // Error handling
    Error,
    IsolationLevel,
    Result,
    StorageConfig,
    StorageError,
    StorageMetadata,
    SyncMode,
    Transaction,
    TransactionBuilder,
    TransactionContext,
    WalWriterConfig,
};
// Re-export types from rumps-types
pub use rumps_types::{
    // Macros
    global,
    json,
    key,
    local,
    value,
    // Core types
    DataStatus,
    FromValue,
    Key,
    Name,
    Subscript,
    ToValue,
    Value,
};
