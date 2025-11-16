//! Core types for RUMPS database system.
//!
//! This crate defines the fundamental types used across the RUMPS storage
//! and query layers, including globals, keys, values, and node structures.

#![warn(missing_docs)]

// Re-export commonly used types
pub use error::*;
pub use key::*;
// pub use node::*;  // TODO: Implement node types for B-tree storage
pub use transaction::*;
pub use value::*;

mod error;
mod key;
// mod node;  // TODO: Implement node types for B-tree storage
mod transaction;
mod value;
