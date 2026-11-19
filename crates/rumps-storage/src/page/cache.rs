//! Dirty page tracker for the storage engine.
//!
//! Tracks which pages have been modified and need flushing to disk.
//! Node data lives exclusively in `NodeCache` (`DashMap`); this module
//! only records the set of dirty `PageId`s.

use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use super::PageId;

/// Statistics for dirty page tracking.
#[derive(Debug, Clone, Default)]
pub struct DirtyPageStats {
    /// Number of pages marked dirty.
    pub writes: u64,
    /// Number of pages flushed to disk.
    pub flushes: u64,
}

/// Atomic counters for dirty-tracking statistics.
struct AtomicStats {
    writes: AtomicU64,
    flushes: AtomicU64,
}

impl AtomicStats {
    fn new() -> Self {
        Self {
            writes: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
        }
    }

    fn snapshot(&self) -> DirtyPageStats {
        DirtyPageStats {
            writes: self.writes.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
        }
    }
}

/// Tracks which pages are dirty (need flushing to disk).
///
/// This replaced the former LRU page cache; node data now lives
/// exclusively in `NodeCache` (`DashMap<NodeId, Arc<Node>>`).
pub(crate) struct DirtyPages {
    dirty: RwLock<HashSet<PageId>>,
    stats: AtomicStats,
}

impl DirtyPages {
    pub(crate) fn new() -> Self {
        Self {
            dirty: RwLock::new(HashSet::new()),
            stats: AtomicStats::new(),
        }
    }

    /// Mark a page as dirty (needs flush).
    pub(crate) fn mark_dirty(&self, id: PageId) {
        self.dirty.write().insert(id);
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
    }

    /// Get all dirty page IDs.
    pub(crate) fn dirty_page_ids(&self) -> Vec<PageId> {
        self.dirty.read().iter().copied().collect()
    }

    /// Mark pages as flushed (no longer dirty).
    pub(crate) fn mark_flushed(&self, ids: &[PageId]) {
        let mut dirty = self.dirty.write();
        ids.iter().for_each(|id| {
            if dirty.remove(id) {
                self.stats.flushes.fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    /// Check if a page is dirty.
    pub(crate) fn is_dirty(&self, id: PageId) -> bool {
        self.dirty.read().contains(&id)
    }

    /// Get the number of dirty pages.
    pub(crate) fn dirty_count(&self) -> usize {
        self.dirty.read().len()
    }

    /// Remove a page from the dirty set.
    pub(crate) fn remove(&self, id: PageId) {
        self.dirty.write().remove(&id);
    }

    /// Clear all dirty pages. Returns the set of page IDs that were dirty.
    pub(crate) fn clear(&self) -> Vec<PageId> {
        self.dirty.write().drain().collect()
    }

    /// Check if there are no dirty pages.
    pub(crate) fn is_empty(&self) -> bool {
        self.dirty.read().is_empty()
    }

    /// Get dirty-tracking statistics.
    pub(crate) fn stats(&self) -> DirtyPageStats {
        self.stats.snapshot()
    }
}

impl fmt::Debug for DirtyPages {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirtyPages")
            .field("dirty_count", &self.dirty.read().len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::page::PageId;

    #[test]
    fn mark_dirty_and_check() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();

        assert!(!dp.is_dirty(id));
        dp.mark_dirty(id);
        assert!(dp.is_dirty(id));
        assert_eq!(dp.dirty_count(), 1);
    }

    #[test]
    fn mark_dirty_idempotent() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();

        dp.mark_dirty(id);
        dp.mark_dirty(id);
        assert_eq!(dp.dirty_count(), 1);
    }

    #[test]
    fn dirty_page_ids() {
        let dp = DirtyPages::new();
        let id1 = PageId::from_page_num(1).unwrap();
        let id2 = PageId::from_page_num(2).unwrap();
        let id3 = PageId::from_page_num(3).unwrap();

        dp.mark_dirty(id1);
        dp.mark_dirty(id2);

        let ids: std::collections::HashSet<_> =
            dp.dirty_page_ids().into_iter().collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
        assert!(!ids.contains(&id3));
    }

    #[test]
    fn mark_flushed() {
        let dp = DirtyPages::new();
        let id1 = PageId::from_page_num(1).unwrap();
        let id2 = PageId::from_page_num(2).unwrap();
        let id3 = PageId::from_page_num(3).unwrap();

        dp.mark_dirty(id1);
        dp.mark_dirty(id2);
        dp.mark_dirty(id3);

        dp.mark_flushed(&[id1, id3]);

        assert!(!dp.is_dirty(id1));
        assert!(dp.is_dirty(id2));
        assert!(!dp.is_dirty(id3));
        assert_eq!(dp.dirty_count(), 1);
    }

    #[test]
    fn mark_flushed_nonexistent() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(999).unwrap();

        dp.mark_flushed(&[id]);
        assert_eq!(dp.stats().flushes, 0);
    }

    #[test]
    fn mark_flushed_empty_slice() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();
        dp.mark_dirty(id);

        dp.mark_flushed(&[]);
        assert!(dp.is_dirty(id));
        assert_eq!(dp.stats().flushes, 0);
    }

    #[test]
    fn mark_flushed_duplicate_in_slice() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();
        dp.mark_dirty(id);

        dp.mark_flushed(&[id, id]);
        assert_eq!(dp.stats().flushes, 1);
    }

    #[test]
    fn remove() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();

        dp.mark_dirty(id);
        assert!(dp.is_dirty(id));

        dp.remove(id);
        assert!(!dp.is_dirty(id));
        assert_eq!(dp.dirty_count(), 0);
    }

    #[test]
    fn remove_nonexistent() {
        let dp = DirtyPages::new();
        dp.remove(PageId::from_page_num(999).unwrap());
        assert!(dp.is_empty());
    }

    #[test]
    fn clear() {
        let dp = DirtyPages::new();
        dp.mark_dirty(PageId::from_page_num(1).unwrap());
        dp.mark_dirty(PageId::from_page_num(2).unwrap());

        let cleared = dp.clear();
        assert_eq!(cleared.len(), 2);
        assert!(dp.is_empty());
    }

    #[test]
    fn clear_empty() {
        let dp = DirtyPages::new();
        let cleared = dp.clear();
        assert!(cleared.is_empty());
    }

    #[test]
    fn stats() {
        let dp = DirtyPages::new();
        let id = PageId::from_page_num(1).unwrap();

        dp.mark_dirty(id);
        dp.mark_dirty(PageId::from_page_num(2).unwrap());

        let s = dp.stats();
        assert_eq!(s.writes, 2);
        assert_eq!(s.flushes, 0);
    }
}
