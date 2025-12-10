//! ORM-like traits for converting Rust types to/from RUMPS storage.
//!
//! This module provides traits for automatic conversion between Rust types
//! and RUMPS's hierarchical key-value storage, similar to how serde works
//! for serialization but tailored to RUMPS's tree structure.
//!
//! # Trait Hierarchy
//!
//! - [`ToRumps`] / [`FromRumps`]: Convert structs to/from tree key-value pairs
//! - [`RumpsRead`]: Read operations called on entity types
//! - [`RumpsWrite`]: Write operations called on entity types
//!
//! # Example
//!
//! ```ignore
//! use rumps_storage::{Database, RumpsRead, RumpsWrite};
//!
//! #[derive(ToRumps, FromRumps)]
//! #[rumps(global = "user")]
//! struct User {
//!     #[rumps(key)]
//!     id: u64,
//!     name: String,
//!     email: String,
//! }
//!
//! let db = Database::in_memory()?;
//!
//! // Write requires transaction
//! db.transaction(|tx| async move {
//!     let user = User { id: 1, name: "Alice".into(), email: "a@b.c".into() };
//!     user.insert(&tx).await?;
//!     Ok(())
//! }).await?;
//!
//! // Read works on db directly
//! let user = User::one(&db, 1u64).await?;
//! let users = User::all(&db).await?;
//! ```

mod sealed;
mod traits;

// Re-export derive macros (they can share names with traits; different namespaces)
#[cfg(feature = "derive")]
pub use rumps_derive::{
    FromRumps, FromSubscript, FromValue, ToRumps, ToSubscript, ToValue,
};
pub use sealed::Sealed;
pub use traits::{
    FromRumps, RumpsRead, RumpsReader, RumpsWrite, RumpsWriter, ToRumps,
};
