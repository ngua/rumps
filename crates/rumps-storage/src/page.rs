//! Page-based storage for RUMPS.
//!
//! This module provides:
//! - [`PAGE_SIZE`]: Compile-time constant for B-tree node page size
//! - [`PageId`]: Identifier for a page (byte offset into the data file)
//! - [`PageAllocator`]: Bitmap-based page allocator
//! - [`PageCache`]: LRU cache of pages with dirty tracking and async flush
//!
//! # Page Size Configuration
//!
//! The page size is a compile-time constant that determines the maximum
//! size of serialized B-tree nodes. This affects disk I/O alignment,
//! node splitting thresholds, and cache efficiency.
//!
//! To change the page size, set `RUMPS_PAGE_SIZE` env var at compile time:
//! ```sh
//! RUMPS_PAGE_SIZE=8192 cargo build
//! ```

mod allocator;
mod cache;
mod id;

pub(crate) use allocator::PageAllocator;
pub(crate) use cache::PageCache;
pub use cache::PageCacheStats;
pub(crate) use id::PageId;

/// Page size in bytes for B-tree node storage.
///
/// Common values:
/// - `4096` (4KB) - typical OS page size, good default
/// - `8192` (8KB) - PostgreSQL's default
/// - `16384` (16KB) - MySQL/InnoDB's default
///
/// Set via `RUMPS_PAGE_SIZE` env var at compile time. Defaults to `4096`.
/// Existing databases created with a different page size are incompatible.
pub(crate) const PAGE_SIZE: usize = {
    // SAFETY: build.rs guarantees this is set and valid
    match usize::from_str_radix(env!("RUMPS_PAGE_SIZE"), 10) {
        Ok(n) => n,
        Err(_) => 4096,
    }
};
