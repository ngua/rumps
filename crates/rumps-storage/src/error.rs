//! Error types for the storage layer.

// Re-export public error types from rumps-types
pub use rumps_types::{Error, StorageError};

/// Internal result type using `StorageError` directly.
pub(crate) type Result<T> = std::result::Result<T, StorageError>;
