//! Core types for RUMPS database system.
//!
//! This crate defines the fundamental types used across the RUMPS storage
//! and query layers, including globals, keys, values, and node structures.

#![warn(missing_docs)]

// Re-export commonly used types
pub use error::*;
pub use key::*;
pub use value::*;

// Re-export serde_json::json! macro for convenience with JSON subscripts
pub use serde_json::json;

mod error;
mod key;
mod value;
