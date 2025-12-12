//! Core types for RUMPS database system.
//!
//! This crate defines the fundamental types used across the RUMPS storage
//! and query layers, including globals, keys, values, and node structures.
//!
//! # Platform Requirements
//!
//! RUMPS requires a 64-bit platform. The storage layer uses `u64` page
//! identifiers throughout, and 32-bit platforms would require pervasive
//! bounds checking and truncation handling.

#![warn(missing_docs)]

#[cfg(not(target_pointer_width = "64"))]
compile_error!("RUMPS requires a 64-bit platform (usize must be 64 bits)");

// Re-export commonly used types
pub use error::{Error, Result, StorageError};
pub use key::*;
// Re-export serde_json::json! macro for convenience with JSON subscripts
pub use serde_json::json;
// Re-export smol_str for constructing `Name`s from runtime strings
pub use smol_str::SmolStr;
pub use value::*;

mod error;
mod key;
pub mod orm;
mod value;
