//! Configuration types for the storage engine.

use std::path::PathBuf;

use crate::wal::WalWriterConfig;

/// Configuration for [`super::FileStorageEngine`].
#[derive(Debug, Clone)]
pub(crate) struct StorageConfig {
    /// Maximum number of pages to cache in memory.
    ///
    /// Larger values improve read performance at the cost of memory.
    /// Default: `1024` pages (~4 MiB at 4KB page size).
    pub(crate) cache_size: usize,

    /// Maximum number of pages in the database (optional limit).
    ///
    /// `None` means unlimited growth. Useful for testing or
    /// resource-constrained environments.
    pub(crate) max_pages: Option<u64>,

    /// WAL configuration (sync mode, max file size, etc.).
    pub(crate) wal_config: WalWriterConfig,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cache_size: 1024,
            max_pages: None,
            wal_config: WalWriterConfig::default(),
        }
    }
}

/// Metadata about the storage engine state.
#[derive(Debug, Clone)]
pub struct StorageMetadata {
    /// Total number of allocated pages (including header).
    pub allocated_pages: u64,

    /// Number of free pages available for allocation.
    pub free_pages: u64,

    /// Number of pages currently in cache.
    pub cached_pages: usize,

    /// Number of dirty pages pending flush.
    pub dirty_pages: usize,

    /// Cache hit rate (`0.0` to `1.0`).
    pub cache_hit_rate: f64,

    /// Path to the data directory.
    pub data_dir: PathBuf,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn storage_config_default() {
        let cfg = StorageConfig::default();
        assert_eq!(cfg.cache_size, 1024);
        assert!(cfg.max_pages.is_none());
    }

    #[test]
    fn storage_metadata_fields() {
        let meta = StorageMetadata {
            allocated_pages: 100,
            free_pages: 50,
            cached_pages: 25,
            dirty_pages: 5,
            cache_hit_rate: 0.75,
            data_dir: PathBuf::from("/tmp/test"),
        };

        assert_eq!(meta.allocated_pages, 100);
        assert_eq!(meta.free_pages, 50);
        assert_eq!(meta.cached_pages, 25);
        assert_eq!(meta.dirty_pages, 5);
        assert!((meta.cache_hit_rate - 0.75).abs() < 0.001);
        assert_eq!(meta.data_dir, PathBuf::from("/tmp/test"));
    }
}
