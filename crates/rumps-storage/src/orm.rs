//! ORM-like traits for converting Rust types to/from RUMPS storage.
//!
//! This module provides traits for automatic conversion between Rust types
//! and RUMPS's hierarchical key-value storage, similar to how serde works
//! for serialization but tailored to RUMPS's tree structure.
//!
//! # Trait Hierarchy
//!
//! - [`ToRumps`] / [`FromRumps`]: Convert structs to/from tree key-value pairs
//! - [`RumpsRead`]: Read operations (available on `Database` and `Transaction`)
//! - [`RumpsWrite`]: Write operations (only on `Transaction`)
//!
//! # Example
//!
//! ```ignore
//! use rumps_storage::{Database, ToRumps, FromRumps, RumpsRead, RumpsWrite};
//!
//! struct User {
//!     id: u64,
//!     name: String,
//!     email: String,
//! }
//!
//! impl ToRumps for User { /* ... */ }
//! impl FromRumps for User { /* ... */ }
//!
//! let db = Database::in_memory()?;
//!
//! // Write requires transaction
//! db.transaction(|txn| async {
//!     txn.insert(&User { id: 1, name: "Alice".into(), email: "a@b.c".into() }).await?;
//!     Ok(())
//! }).await?;
//!
//! // Read works on db directly
//! let user: Option<User> = db.one(1).await?;
//! let users: Vec<User> = db.all().await?;
//! ```

mod sealed;
mod traits;

// Re-export derive macros (they can share names with traits - different namespaces)
#[cfg(feature = "derive")]
pub use rumps_derive::{
    FromRumps, FromSubscript, FromValue, ToRumps, ToSubscript, ToValue,
};
pub use sealed::Sealed;
pub use traits::{FromRumps, RumpsRead, RumpsWrite, ToRumps};
