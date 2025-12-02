//! Error types for RUMPS.

use thiserror::Error;

/// Errors that can occur in RUMPS operations.
#[derive(Debug, Error)]
pub enum Error {
    /// Placeholder error variant.
    #[error("not yet implemented")]
    NotImplemented,

    /// Attempted to free a page beyond the allocator's capacity.
    #[error("page {0} is out of bounds")]
    PageOutOfBounds(u64),

    /// Attempted to free a page that is not allocated (double-free).
    #[error("page {0} is not allocated")]
    PageNotAllocated(u64),

    /// Page allocation failed (limit exceeded or no free pages).
    #[error("page allocation failed: limit of {0} pages exceeded")]
    PageLimitExceeded(u64),

    /// Cannot free page 0 (reserved for header).
    #[error("cannot free page 0 (reserved header page)")]
    CannotFreeHeaderPage,

    /// Cannot free a reserved page (superblock or bitmap page).
    #[error("cannot free reserved page {0}")]
    CannotFreeReservedPage(u64),

    /// Invalid bitmap data (e.g., empty bitmap during recovery).
    #[error("invalid bitmap: must have at least one word")]
    InvalidBitmap,

    /// Page number too large (would overflow byte offset calculation).
    #[error("page number {0} overflows byte offset calculation")]
    PageNumberOverflow(u64),
}

/// Result type alias for RUMPS operations.
pub type Result<T> = std::result::Result<T, Error>;
