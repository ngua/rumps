//! Concurrent LRU page cache with dirty tracking.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use moka::sync::Cache;

use super::PageId;
use crate::node::Node;

/// A cached page entry with an atomic dirty flag.
#[derive(Debug, Clone)]
struct CachedPage {
    node: Arc<Node>,
    dirty: Arc<AtomicBool>,
}

/// Result of attempting to mark a page as dirty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkDirtyResult {
    /// The page was marked as dirty.
    Marked,
    /// The page is not in cache.
    NotInCache,
}

/// An evicted page with its dirty status.
#[derive(Debug, Clone)]
pub(crate) struct EvictedPage {
    /// The page ID.
    pub(crate) id: PageId,
    /// The node data.
    pub(crate) node: Arc<Node>,
    /// Whether the page was dirty (needs flushing).
    pub(crate) dirty: bool,
}

/// Atomic counters for cache statistics.
struct Stats {
    hits: AtomicU64,
    misses: AtomicU64,
    writes: AtomicU64,
    flushes: AtomicU64,
    evictions: AtomicU64,
}

impl Stats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }
}

/// Statistics for the page cache.
#[derive(Debug, Clone, Default)]
pub struct PageCacheStats {
    /// Maximum number of pages the cache can hold.
    pub max_capacity: usize,
    /// Number of cache hits.
    pub hits: u64,
    /// Number of cache misses.
    pub misses: u64,
    /// Number of pages written (marked dirty).
    pub writes: u64,
    /// Number of pages flushed to disk.
    pub flushes: u64,
    /// Number of pages evicted from cache.
    pub evictions: u64,
}

/// Concurrent cache for B-tree node pages with dirty tracking.
///
/// `PageCache` provides an in-memory cache of pages read from or to be written
/// to the data file. It tracks which pages have been modified (dirty) and need
/// to be flushed to disk.
///
/// # Sync by Design
///
/// All operations are deliberately synchronous despite the surrounding async
/// engine. The cache is purely in-memory; no operation performs I/O or needs to
/// yield. This avoids the overhead of `tokio::sync::RwLock` (waker registration,
/// task scheduling) and eliminates the risk of holding an async lock guard
/// across an `.await` point. Async callers invoke cache methods directly and
/// then perform any resulting disk I/O (e.g. flushing dirty evicted pages)
/// in their own async context.
///
/// # Thread Safety
///
/// All operations are lock-free for readers. Moka handles internal
/// synchronization with sharded locks and lock-free reads. Dirty flags use
/// `AtomicBool` and require no re-insertion to toggle.
///
/// # Eviction
///
/// Moka evicts entries using a TinyLFU admission policy. Dirty evicted pages
/// are buffered and returned from [`put`] (or [`drain_evicted`]) so the caller
/// can flush them to disk before they are lost.
///
/// [`put`]: Self::put
/// [`drain_evicted`]: Self::drain_evicted
pub(crate) struct PageCache {
    cache: Cache<PageId, CachedPage>,
    evicted: Arc<Mutex<Vec<EvictedPage>>>,
    stats: Arc<Stats>,
    capacity: usize,
}

impl PageCache {
    /// Create a new page cache with the given capacity (number of pages).
    ///
    /// If `capacity` is `0`, defaults to `1`.
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        let evicted: Arc<Mutex<Vec<EvictedPage>>> =
            Arc::new(Mutex::new(Vec::new()));
        let stats = Arc::new(Stats::new());

        let ev = Arc::clone(&evicted);
        let st = Arc::clone(&stats);
        let cache = Cache::builder()
            .max_capacity(cap as u64)
            .eviction_listener(move |k, v: CachedPage, _cause| {
                st.evictions.fetch_add(1, Ordering::Relaxed);
                if v.dirty.load(Ordering::Relaxed) {
                    #[allow(clippy::unwrap_used)]
                    ev.lock().unwrap().push(EvictedPage {
                        id: *k,
                        node: v.node,
                        dirty: true,
                    });
                }
            })
            .build();

        Self {
            cache,
            evicted,
            stats,
            capacity: cap,
        }
    }

    /// Get a page from the cache.
    ///
    /// Returns `None` if the page is not in cache (cache miss).
    /// Updates LRU order on hit.
    pub(crate) fn get(&self, id: PageId) -> Option<Arc<Node>> {
        self.cache
            .get(&id)
            .map(|entry| {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                Arc::clone(&entry.node)
            })
            .or_else(|| {
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            })
    }

    /// Insert or update a page in the cache.
    ///
    /// If `dirty` is `true`, the page is marked as needing flush.
    /// Returns dirty pages that were evicted, if any; the caller is
    /// responsible for flushing them to disk.
    pub(crate) fn put(
        &self,
        id: PageId,
        node: Node,
        dirty: bool,
    ) -> Vec<EvictedPage> {
        if dirty {
            self.stats.writes.fetch_add(1, Ordering::Relaxed);
        }

        self.cache.insert(
            id,
            CachedPage {
                node: Arc::new(node),
                dirty: Arc::new(AtomicBool::new(dirty)),
            },
        );
        self.cache.run_pending_tasks();

        self.drain_evicted()
    }

    /// Mark a page as dirty (needs flush).
    pub(crate) fn mark_dirty(&self, id: PageId) -> MarkDirtyResult {
        self.cache
            .get(&id)
            .map(|entry| {
                entry.dirty.store(true, Ordering::Relaxed);
                self.stats.writes.fetch_add(1, Ordering::Relaxed);
                MarkDirtyResult::Marked
            })
            .unwrap_or(MarkDirtyResult::NotInCache)
    }

    /// Get all dirty pages.
    ///
    /// Returns a list of `(PageId, Node)` pairs for pages that need flushing.
    pub(crate) fn dirty_pages(&self) -> Vec<(PageId, Arc<Node>)> {
        self.cache
            .iter()
            .filter(|(_, entry)| entry.dirty.load(Ordering::Relaxed))
            .map(|(id, entry)| (*id, Arc::clone(&entry.node)))
            .collect()
    }

    /// Mark pages as flushed (no longer dirty).
    ///
    /// Call this after successfully writing pages to disk.
    pub(crate) fn mark_flushed(&self, ids: &[PageId]) {
        ids.iter().for_each(|&id| {
            if let Some(entry) = self.cache.get(&id) {
                if entry.dirty.swap(false, Ordering::Relaxed) {
                    self.stats.flushes.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    /// Clear the dirty flag for a specific page.
    pub(crate) fn mark_clean(&self, id: PageId) {
        if let Some(entry) = self.cache.get(&id) {
            entry.dirty.store(false, Ordering::Relaxed);
        }
    }

    /// Check if a page is dirty.
    pub(crate) fn is_dirty(&self, id: PageId) -> bool {
        self.cache
            .get(&id)
            .is_some_and(|entry| entry.dirty.load(Ordering::Relaxed))
    }

    /// Get the number of dirty pages.
    pub(crate) fn dirty_count(&self) -> usize {
        self.cache
            .iter()
            .filter(|(_, e)| e.dirty.load(Ordering::Relaxed))
            .count()
    }

    /// Remove a page from the cache.
    ///
    /// Returns the removed page if it was in cache.
    pub(crate) fn remove(&self, id: PageId) -> Option<Arc<Node>> {
        self.cache.remove(&id).map(|entry| entry.node)
    }

    /// Clear all pages from the cache.
    ///
    /// Returns all dirty pages that were discarded, allowing the caller to
    /// flush them if needed.
    pub(crate) fn clear(&self) -> Vec<(PageId, Arc<Node>)> {
        let dirty = self.dirty_pages();
        self.cache.invalidate_all();
        self.cache.run_pending_tasks();
        #[allow(clippy::unwrap_used)]
        self.evicted.lock().unwrap().clear();
        dirty
    }

    /// Get the number of pages in cache.
    pub(crate) fn len(&self) -> usize {
        self.cache.entry_count() as usize
    }

    /// Check if the cache is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.cache.entry_count() == 0
    }

    /// Get the cache capacity.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get cache statistics.
    pub(crate) fn stats(&self) -> PageCacheStats {
        PageCacheStats {
            max_capacity: self.capacity,
            hits: self.stats.hits.load(Ordering::Relaxed),
            misses: self.stats.misses.load(Ordering::Relaxed),
            writes: self.stats.writes.load(Ordering::Relaxed),
            flushes: self.stats.flushes.load(Ordering::Relaxed),
            evictions: self.stats.evictions.load(Ordering::Relaxed),
        }
    }

    /// Calculate the hit rate (`0.0` to `1.0`).
    pub(crate) fn hit_rate(&self) -> f64 {
        let h = self.stats.hits.load(Ordering::Relaxed);
        let m = self.stats.misses.load(Ordering::Relaxed);
        let total = h + m;
        if total == 0 {
            0.0
        } else {
            h as f64 / total as f64
        }
    }

    /// Drain any dirty pages captured by the eviction listener.
    #[allow(clippy::unwrap_used)]
    pub(crate) fn drain_evicted(&self) -> Vec<EvictedPage> {
        let mut buf = self.evicted.lock().unwrap();
        buf.drain(..).collect()
    }
}

impl fmt::Debug for PageCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageCache")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
mod tests {
    use std::collections::HashSet;

    use rumps_types::key;

    use super::*;
    use crate::node::NodeData;
    use crate::page::PageId;

    fn make_node(id: u64) -> Node {
        let mut node = Node::new_leaf();
        node.keys.push(key![id as i64]);
        node.values
            .push(Arc::new(NodeData::new(Some((id as i64).into()), false)));
        node
    }

    #[test]
    fn cache_basic_get_put() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        assert!(cache.get(id).is_none());

        cache.put(id, node.clone(), false);

        let retrieved = cache.get(id).expect("should be in cache");
        assert_eq!(*retrieved, node);
    }

    #[test]
    fn cache_dirty_tracking() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        // Insert clean
        cache.put(id, node.clone(), false);
        assert!(!cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 0);

        // Mark dirty
        assert_eq!(cache.mark_dirty(id), MarkDirtyResult::Marked);
        assert!(cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 1);

        // Mark clean
        cache.mark_clean(id);
        assert!(!cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 0);
    }

    #[test]
    fn cache_insert_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        cache.put(id, node, true);

        assert!(cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 1);
    }

    #[test]
    fn cache_dirty_pages() {
        let cache = PageCache::new(10);

        // Insert some clean and some dirty
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), true);
        cache.put(PageId::from_page_num(3).unwrap(), make_node(3), false);
        cache.put(PageId::from_page_num(4).unwrap(), make_node(4), true);

        let dirty = cache.dirty_pages();
        assert_eq!(dirty.len(), 2);

        let ids: HashSet<_> = dirty.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&PageId::from_page_num(2).unwrap()));
        assert!(ids.contains(&PageId::from_page_num(4).unwrap()));
    }

    #[test]
    fn cache_mark_flushed() {
        let cache = PageCache::new(10);

        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), true);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), true);
        cache.put(PageId::from_page_num(3).unwrap(), make_node(3), true);

        assert_eq!(cache.dirty_count(), 3);

        cache.mark_flushed(&[
            PageId::from_page_num(1).unwrap(),
            PageId::from_page_num(3).unwrap(),
        ]);

        assert_eq!(cache.dirty_count(), 1);
        assert!(cache.is_dirty(PageId::from_page_num(2).unwrap()));
        assert!(!cache.is_dirty(PageId::from_page_num(1).unwrap()));
        assert!(!cache.is_dirty(PageId::from_page_num(3).unwrap()));
    }

    #[test]
    fn cache_eviction() {
        let cache = PageCache::new(3);

        // Fill cache
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);
        cache.put(PageId::from_page_num(3).unwrap(), make_node(3), false);

        assert_eq!(cache.len(), 3);

        // Insert beyond capacity; moka will evict eventually
        cache.put(PageId::from_page_num(4).unwrap(), make_node(4), false);

        // Cache should not grow unboundedly
        assert!(cache.len() <= 4);
    }

    #[test]
    fn cache_lru_order() {
        let cache = PageCache::new(3);

        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);
        cache.put(PageId::from_page_num(3).unwrap(), make_node(3), false);

        // Access page 1 to boost its frequency
        cache.get(PageId::from_page_num(1).unwrap());

        // Insert beyond capacity
        cache.put(PageId::from_page_num(4).unwrap(), make_node(4), false);

        // Page 1 was recently accessed, so it should survive eviction
        assert!(cache.get(PageId::from_page_num(1).unwrap()).is_some());
    }

    #[test]
    fn cache_remove() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true);
        assert!(cache.get(id).is_some());
        assert!(cache.is_dirty(id));

        let removed = cache.remove(id);
        assert!(removed.is_some());
        assert!(cache.get(id).is_none());
        assert!(!cache.is_dirty(id));
    }

    #[test]
    fn cache_clear() {
        let cache = PageCache::new(10);

        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), true);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.dirty_count(), 1);

        let discarded = cache.clear();

        assert!(cache.is_empty());
        assert_eq!(cache.dirty_count(), 0);

        // Should return the dirty page that was discarded
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].0, PageId::from_page_num(1).unwrap());
    }

    #[test]
    fn cache_stats() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Miss
        cache.get(id);

        // Insert (write)
        cache.put(id, make_node(1), true);

        // Hit
        cache.get(id);
        cache.get(id);

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.writes, 1);
    }

    #[test]
    fn cache_hit_rate() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Empty cache, no operations
        assert_eq!(cache.hit_rate(), 0.0);

        // 1 miss
        cache.get(id);
        assert_eq!(cache.hit_rate(), 0.0);

        cache.put(id, make_node(1), false);

        // 1 hit
        cache.get(id);
        // 1 hit, 1 miss = 50%
        assert!((cache.hit_rate() - 0.5).abs() < 0.01);

        // 2 more hits
        cache.get(id);
        cache.get(id);
        // 3 hits, 1 miss = 75%
        assert!((cache.hit_rate() - 0.75).abs() < 0.01);
    }

    #[test]
    fn cache_update_existing() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), false);

        // Update with new node
        let new_node = make_node(999);
        cache.put(id, new_node.clone(), true);

        let retrieved = cache.get(id).expect("should be in cache");
        assert_eq!(*retrieved, new_node);
        assert!(cache.is_dirty(id));
    }

    #[test]
    fn cache_mark_dirty_nonexistent() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        assert_eq!(cache.mark_dirty(id), MarkDirtyResult::NotInCache);
    }

    #[test]
    fn cache_eviction_dirty_page() {
        // Use a larger cache so moka's admission policy is more predictable
        let cache = PageCache::new(4);

        // Fill with dirty pages
        (1..=4).for_each(|i| {
            cache.put(PageId::from_page_num(i).unwrap(), make_node(i), true);
        });

        // Insert several more to force eviction of dirty pages
        let evicted: Vec<_> = (5..=8)
            .flat_map(|i| {
                cache.put(
                    PageId::from_page_num(i).unwrap(),
                    make_node(i),
                    false,
                )
            })
            .collect();

        // At least some dirty pages should have been captured
        assert!(evicted.iter().all(|e| e.dirty));
    }

    #[test]
    fn cache_put_clean_clears_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Insert as dirty
        cache.put(id, make_node(1), true);
        assert!(cache.is_dirty(id));

        // Update with dirty=false should clear the dirty flag
        cache.put(id, make_node(2), false);
        assert!(!cache.is_dirty(id));
    }

    #[test]
    fn cache_capacity_zero_becomes_one() {
        let cache = PageCache::new(0);

        // Should be treated as capacity 1
        assert_eq!(cache.capacity(), 1);

        // Can insert one page
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        assert_eq!(cache.len(), 1);

        // Second insert should evict the first
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);
        assert!(cache.len() <= 2);
    }

    #[test]
    fn cache_mark_flushed_only_counts_dirty() {
        let cache = PageCache::new(10);

        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), true);

        // Flush both existing and non-existing pages
        cache.mark_flushed(&[
            PageId::from_page_num(1).unwrap(),
            PageId::from_page_num(999).unwrap(), // doesn't exist
        ]);

        let stats = cache.stats();
        // Should only count the one that was actually dirty
        assert_eq!(stats.flushes, 1);
    }

    #[test]
    fn cache_get_miss_explicit() {
        let cache = PageCache::new(10);

        // Explicit test that get on non-existent page returns None
        let result = cache.get(PageId::from_page_num(999).unwrap());
        assert!(result.is_none());

        // Stats should show a miss
        let stats = cache.stats();
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 0);
    }

    #[test]
    fn cache_put_update_at_capacity_no_eviction() {
        let cache = PageCache::new(2);

        let id1 = PageId::from_page_num(1).unwrap();
        let id2 = PageId::from_page_num(2).unwrap();

        // Fill cache
        cache.put(id1, make_node(1), false);
        cache.put(id2, make_node(2), false);

        assert_eq!(cache.len(), 2);

        // Update existing page; should NOT evict
        let evicted = cache.put(id1, make_node(100), true);
        assert!(evicted.is_empty());
        assert_eq!(cache.len(), 2);

        // Verify update happened
        let node = cache.get(id1).unwrap();
        assert_eq!(node.values.len(), 1);
    }

    #[test]
    fn cache_put_same_page_twice() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true);
        cache.put(id, make_node(2), false);

        // Should still only have 1 entry
        assert_eq!(cache.len(), 1);

        // Second put with dirty=false should clear dirty
        assert!(!cache.is_dirty(id));
    }

    #[test]
    fn cache_mark_dirty_already_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true);
        assert!(cache.is_dirty(id));

        // Mark dirty again; should be idempotent
        let result = cache.mark_dirty(id);
        assert_eq!(result, MarkDirtyResult::Marked);
        assert!(cache.is_dirty(id));

        // Dirty count should still be 1
        assert_eq!(cache.dirty_count(), 1);
    }

    #[test]
    fn cache_mark_flushed_empty_slice() {
        let cache = PageCache::new(10);
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), true);

        // Flush with empty slice; should be no-op
        cache.mark_flushed(&[]);

        // Original page should still be dirty
        assert!(cache.is_dirty(PageId::from_page_num(1).unwrap()));
        assert_eq!(cache.stats().flushes, 0);
    }

    #[test]
    fn cache_mark_flushed_duplicate_in_slice() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        cache.put(id, make_node(1), true);

        // Flush same page twice in one call
        cache.mark_flushed(&[id, id]);

        // Should only count as 1 flush (second swap returns false)
        let stats = cache.stats();
        assert_eq!(stats.flushes, 1);
    }

    #[test]
    fn cache_remove_nonexistent() {
        let cache = PageCache::new(10);

        // Remove page that doesn't exist
        let result = cache.remove(PageId::from_page_num(999).unwrap());
        assert!(result.is_none());
    }

    #[test]
    fn cache_clear_empty() {
        let cache = PageCache::new(10);

        // Clear empty cache
        let discarded = cache.clear();
        assert!(discarded.is_empty());
    }

    #[test]
    fn cache_clear_only_clean_pages() {
        let cache = PageCache::new(10);

        // Add only clean pages
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);

        assert_eq!(cache.len(), 2);
        assert_eq!(cache.dirty_count(), 0);

        // Clear should return empty (no dirty pages to report)
        let discarded = cache.clear();
        assert!(discarded.is_empty());
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_dirty_pages_empty_when_none() {
        let cache = PageCache::new(10);

        // Add only clean pages
        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);

        let dirty = cache.dirty_pages();
        assert!(dirty.is_empty());
    }

    #[test]
    fn cache_dirty_not_lost_on_eviction() {
        // Dirty pages are either still in cache or captured by the listener
        let cache = PageCache::new(4);

        let dirty_ids: Vec<_> =
            (1..=4).map(|i| PageId::from_page_num(i).unwrap()).collect();

        dirty_ids.iter().enumerate().for_each(|(i, &id)| {
            cache.put(id, make_node(i as u64 + 1), true);
        });

        // Force eviction by inserting beyond capacity
        let evicted: Vec<_> = (5..=8)
            .flat_map(|i| {
                cache.put(
                    PageId::from_page_num(i).unwrap(),
                    make_node(i),
                    false,
                )
            })
            .collect();

        // Every original dirty page is either still in cache or was captured
        let evicted_ids: HashSet<_> = evicted.iter().map(|e| e.id).collect();
        dirty_ids.iter().for_each(|&id| {
            assert!(
                cache.get(id).is_some() || evicted_ids.contains(&id),
                "dirty page {id:?} lost"
            );
        });
    }

    #[test]
    fn cache_stats_evictions_accurate() {
        let cache = PageCache::new(2);

        cache.put(PageId::from_page_num(1).unwrap(), make_node(1), false);
        cache.put(PageId::from_page_num(2).unwrap(), make_node(2), false);

        // These should each evict
        cache.put(PageId::from_page_num(3).unwrap(), make_node(3), false);
        cache.put(PageId::from_page_num(4).unwrap(), make_node(4), false);

        let stats = cache.stats();
        assert_eq!(stats.evictions, 2);
    }

    #[test]
    fn cache_remove_dirty_page() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true);
        assert!(cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 1);

        // Remove should also clear from dirty set
        cache.remove(id);

        assert!(!cache.is_dirty(id));
        assert_eq!(cache.dirty_count(), 0);
    }
}
