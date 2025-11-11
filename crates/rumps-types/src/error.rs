//! Error types for RUMPS.

use thiserror::Error;

/// Errors that can occur in RUMPS operations.
#[derive(Debug, Error)]
pub enum Error {
    /// Placeholder error variant.
    #[error("not yet implemented")]
    NotImplemented,
}

/// Result type alias for RUMPS operations.
pub type Result<T> = std::result::Result<T, Error>;
