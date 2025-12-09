//! LRU page cache with dirty tracking.

use std::collections::HashSet;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use lru::LruCache;
use tokio::sync::RwLock;

use super::PageId;
use crate::node::Node;

/// A cached page entry.
#[derive(Debug, Clone)]
struct CachedPage {
    /// The node stored in this page.
    node: Arc<Node>,
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

/// Statistics for the page cache.
#[derive(Debug, Clone, Default)]
pub struct PageCacheStats {
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

/// LRU cache for B-tree node pages with dirty tracking.
///
/// `PageCache` provides an in-memory cache of pages read from or to be written
/// to the data file. It tracks which pages have been modified (dirty) and need
/// to be flushed to disk.
///
/// # Thread Safety
///
/// `PageCache` uses internal `RwLock` for concurrent access. Multiple readers
/// can access the cache simultaneously, but writes are serialized.
///
/// # Eviction
///
/// When the cache reaches capacity, the least-recently-used page is evicted.
/// The [`put`] method returns an [`EvictedPage`] if eviction occurred; the
/// caller is responsible for flushing dirty evicted pages to disk.
///
/// [`put`]: Self::put
pub(crate) struct PageCache {
    /// LRU cache of pages.
    cache: RwLock<LruCache<PageId, CachedPage>>,
    /// Set of dirty page IDs for quick lookup.
    dirty_set: RwLock<HashSet<PageId>>,
    /// Cache statistics.
    stats: RwLock<PageCacheStats>,
    /// Maximum number of pages in cache.
    capacity: usize,
}

impl PageCache {
    /// Create a new page cache with the given capacity (number of pages).
    ///
    /// If `capacity` is `0`, defaults to `1`.
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        // SAFETY: `cap` is at least 1 due to `max(1)` above.
        #[allow(clippy::unwrap_used)]
        let cap_nz = NonZeroUsize::new(cap).unwrap();
        Self {
            cache: RwLock::new(LruCache::new(cap_nz)),
            dirty_set: RwLock::new(HashSet::new()),
            stats: RwLock::new(PageCacheStats::default()),
            capacity: cap,
        }
    }

    /// Get a page from the cache.
    ///
    /// Returns `None` if the page is not in cache (cache miss).
    /// Updates LRU order on hit.
    pub(crate) async fn get(&self, id: PageId) -> Option<Arc<Node>> {
        let mut cache = self.cache.write().await;
        let mut stats = self.stats.write().await;

        cache
            .get(&id)
            .map(|entry| {
                stats.hits += 1;
                Arc::clone(&entry.node)
            })
            .or_else(|| {
                stats.misses += 1;
                None
            })
    }

    /// Insert or update a page in the cache.
    ///
    /// If `dirty` is `true`, the page is marked as needing flush.
    /// If `dirty` is `false`, any previous dirty status is cleared.
    /// Returns the evicted page (with dirty status) if the cache was at capacity.
    #[must_use = "dirty evicted pages must be flushed to disk"]
    pub(crate) async fn put(
        &self,
        id: PageId,
        node: Node,
        dirty: bool,
    ) -> Option<EvictedPage> {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        // Update dirty status for this page
        if dirty {
            dirty_set.insert(id);
            stats.writes += 1;
        } else {
            dirty_set.remove(&id);
        }

        let entry = CachedPage {
            node: Arc::new(node),
        };

        // Check if we'll evict something (only if inserting new page at capacity)
        let evicted = (cache.len() >= self.capacity && !cache.contains(&id))
            .then(|| {
                cache.peek_lru().map(|(evict_id, evict_entry)| {
                    let evict_id = *evict_id;
                    let evict_node = Arc::clone(&evict_entry.node);
                    let evict_dirty = dirty_set.remove(&evict_id);
                    stats.evictions += 1;
                    EvictedPage {
                        id: evict_id,
                        node: evict_node,
                        dirty: evict_dirty,
                    }
                })
            })
            .flatten();

        cache.put(id, entry);

        evicted
    }

    /// Mark a page as dirty (needs flush).
    pub(crate) async fn mark_dirty(&self, id: PageId) -> MarkDirtyResult {
        let cache = self.cache.read().await;
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        if cache.contains(&id) {
            dirty_set.insert(id);
            stats.writes += 1;
            MarkDirtyResult::Marked
        } else {
            MarkDirtyResult::NotInCache
        }
    }

    /// Get all dirty pages.
    ///
    /// Returns a list of `(PageId, Node)` pairs for pages that need flushing.
    pub(crate) async fn dirty_pages(&self) -> Vec<(PageId, Arc<Node>)> {
        let cache = self.cache.read().await;
        let dirty_set = self.dirty_set.read().await;

        dirty_set
            .iter()
            .filter_map(|&id| {
                cache.peek(&id).map(|entry| (id, Arc::clone(&entry.node)))
            })
            .collect()
    }

    /// Mark pages as flushed (no longer dirty).
    ///
    /// Call this after successfully writing pages to disk.
    /// Only increments flush stats for pages that were actually dirty.
    pub(crate) async fn mark_flushed(&self, ids: &[PageId]) {
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        ids.iter().for_each(|&id| {
            if dirty_set.remove(&id) {
                stats.flushes += 1;
            }
        });
    }

    /// Clear the dirty flag for a specific page.
    pub(crate) async fn mark_clean(&self, id: PageId) {
        self.dirty_set.write().await.remove(&id);
    }

    /// Check if a page is dirty.
    pub(crate) async fn is_dirty(&self, id: PageId) -> bool {
        self.dirty_set.read().await.contains(&id)
    }

    /// Get the number of dirty pages.
    pub(crate) async fn dirty_count(&self) -> usize {
        self.dirty_set.read().await.len()
    }

    /// Remove a page from the cache.
    ///
    /// Returns the removed page if it was in cache.
    pub(crate) async fn remove(&self, id: PageId) -> Option<Arc<Node>> {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;

        dirty_set.remove(&id);
        cache.pop(&id).map(|entry| entry.node)
    }

    /// Clear all pages from the cache.
    ///
    /// Returns all dirty pages that were discarded, allowing the caller to
    /// flush them if needed.
    pub(crate) async fn clear(&self) -> Vec<(PageId, Arc<Node>)> {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;

        let dirty_pages = dirty_set
            .iter()
            .filter_map(|&id| {
                cache.peek(&id).map(|e| (id, Arc::clone(&e.node)))
            })
            .collect();

        cache.clear();
        dirty_set.clear();

        dirty_pages
    }

    /// Get the number of pages in cache.
    pub(crate) async fn len(&self) -> usize {
        self.cache.read().await.len()
    }

    /// Check if the cache is empty.
    pub(crate) async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }

    /// Get the cache capacity.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get cache statistics.
    pub(crate) async fn stats(&self) -> PageCacheStats {
        self.stats.read().await.clone()
    }

    /// Calculate the hit rate (`0.0` to `1.0`).
    pub(crate) async fn hit_rate(&self) -> f64 {
        let stats = self.stats.read().await;
        let total = stats.hits + stats.misses;
        if total == 0 {
            0.0
        } else {
            stats.hits as f64 / total as f64
        }
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

    #[tokio::test]
    async fn cache_basic_get_put() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        assert!(cache.get(id).await.is_none());

        cache.put(id, node.clone(), false).await;

        let retrieved = cache.get(id).await.expect("should be in cache");
        assert_eq!(*retrieved, node);
    }

    #[tokio::test]
    async fn cache_dirty_tracking() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        // Insert clean
        cache.put(id, node.clone(), false).await;
        assert!(!cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 0);

        // Mark dirty
        assert_eq!(cache.mark_dirty(id).await, MarkDirtyResult::Marked);
        assert!(cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 1);

        // Mark clean
        cache.mark_clean(id).await;
        assert!(!cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 0);
    }

    #[tokio::test]
    async fn cache_insert_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        let node = make_node(1);

        cache.put(id, node, true).await;

        assert!(cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 1);
    }

    #[tokio::test]
    async fn cache_dirty_pages() {
        let cache = PageCache::new(10);

        // Insert some clean and some dirty
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), true)
            .await;
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;
        cache
            .put(PageId::from_page_num(4).unwrap(), make_node(4), true)
            .await;

        let dirty = cache.dirty_pages().await;
        assert_eq!(dirty.len(), 2);

        let ids: HashSet<_> = dirty.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&PageId::from_page_num(2).unwrap()));
        assert!(ids.contains(&PageId::from_page_num(4).unwrap()));
    }

    #[tokio::test]
    async fn cache_mark_flushed() {
        let cache = PageCache::new(10);

        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), true)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), true)
            .await;
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), true)
            .await;

        assert_eq!(cache.dirty_count().await, 3);

        cache
            .mark_flushed(&[
                PageId::from_page_num(1).unwrap(),
                PageId::from_page_num(3).unwrap(),
            ])
            .await;

        assert_eq!(cache.dirty_count().await, 1);
        assert!(cache.is_dirty(PageId::from_page_num(2).unwrap()).await);
        assert!(!cache.is_dirty(PageId::from_page_num(1).unwrap()).await);
        assert!(!cache.is_dirty(PageId::from_page_num(3).unwrap()).await);
    }

    #[tokio::test]
    async fn cache_eviction() {
        let cache = PageCache::new(3);

        // Fill cache
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;

        assert_eq!(cache.len().await, 3);

        // This should evict page 1 (LRU)
        let evicted = cache
            .put(PageId::from_page_num(4).unwrap(), make_node(4), false)
            .await;

        assert!(evicted.is_some());
        let ev = evicted.unwrap();
        assert_eq!(ev.id, PageId::from_page_num(1).unwrap());
        assert!(!ev.dirty);

        assert!(cache.get(PageId::from_page_num(1).unwrap()).await.is_none());
        assert!(cache.get(PageId::from_page_num(4).unwrap()).await.is_some());
    }

    #[tokio::test]
    async fn cache_lru_order() {
        let cache = PageCache::new(3);

        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;

        // Access page 1 to make it recently used
        cache.get(PageId::from_page_num(1).unwrap()).await;

        // Now page 2 should be LRU
        let evicted = cache
            .put(PageId::from_page_num(4).unwrap(), make_node(4), false)
            .await;

        assert_eq!(evicted.unwrap().id, PageId::from_page_num(2).unwrap());
    }

    #[tokio::test]
    async fn cache_remove() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true).await;
        assert!(cache.get(id).await.is_some());
        assert!(cache.is_dirty(id).await);

        let removed = cache.remove(id).await;
        assert!(removed.is_some());
        assert!(cache.get(id).await.is_none());
        assert!(!cache.is_dirty(id).await);
    }

    #[tokio::test]
    async fn cache_clear() {
        let cache = PageCache::new(10);

        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), true)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        assert_eq!(cache.len().await, 2);
        assert_eq!(cache.dirty_count().await, 1);

        let discarded = cache.clear().await;

        assert!(cache.is_empty().await);
        assert_eq!(cache.dirty_count().await, 0);

        // Should return the dirty page that was discarded
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].0, PageId::from_page_num(1).unwrap());
    }

    #[tokio::test]
    async fn cache_stats() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Miss
        cache.get(id).await;

        // Insert (write)
        cache.put(id, make_node(1), true).await;

        // Hit
        cache.get(id).await;
        cache.get(id).await;

        let stats = cache.stats().await;
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.writes, 1);
    }

    #[tokio::test]
    async fn cache_hit_rate() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Empty cache, no operations
        assert_eq!(cache.hit_rate().await, 0.0);

        // 1 miss
        cache.get(id).await;
        assert_eq!(cache.hit_rate().await, 0.0);

        cache.put(id, make_node(1), false).await;

        // 1 hit
        cache.get(id).await;
        // 1 hit, 1 miss = 50%
        assert!((cache.hit_rate().await - 0.5).abs() < 0.01);

        // 2 more hits
        cache.get(id).await;
        cache.get(id).await;
        // 3 hits, 1 miss = 75%
        assert!((cache.hit_rate().await - 0.75).abs() < 0.01);
    }

    #[tokio::test]
    async fn cache_update_existing() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), false).await;

        // Update with new node
        let new_node = make_node(999);
        cache.put(id, new_node.clone(), true).await;

        let retrieved = cache.get(id).await.expect("should be in cache");
        assert_eq!(*retrieved, new_node);
        assert!(cache.is_dirty(id).await);
    }

    #[tokio::test]
    async fn cache_mark_dirty_nonexistent() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        assert_eq!(cache.mark_dirty(id).await, MarkDirtyResult::NotInCache);
    }

    #[tokio::test]
    async fn cache_eviction_dirty_page() {
        let cache = PageCache::new(2);

        // Insert a dirty page
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), true)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        // Evict page 1 (dirty)
        let evicted = cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;

        let ev = evicted.unwrap();
        assert_eq!(ev.id, PageId::from_page_num(1).unwrap());
        assert!(ev.dirty); // Should indicate the evicted page was dirty
    }

    #[tokio::test]
    async fn cache_put_clean_clears_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        // Insert as dirty
        cache.put(id, make_node(1), true).await;
        assert!(cache.is_dirty(id).await);

        // Update with dirty=false should clear the dirty flag
        cache.put(id, make_node(2), false).await;
        assert!(!cache.is_dirty(id).await);
    }

    #[tokio::test]
    async fn cache_capacity_zero_becomes_one() {
        let cache = PageCache::new(0);

        // Should be treated as capacity 1
        assert_eq!(cache.capacity(), 1);

        // Can insert one page
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        assert_eq!(cache.len().await, 1);

        // Second insert should evict the first
        let evicted = cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;
        assert!(evicted.is_some());
        assert_eq!(evicted.unwrap().id, PageId::from_page_num(1).unwrap());
        assert_eq!(cache.len().await, 1);
    }

    #[tokio::test]
    async fn cache_mark_flushed_only_counts_dirty() {
        let cache = PageCache::new(10);

        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), true)
            .await;

        // Flush both existing and non-existing pages
        cache
            .mark_flushed(&[
                PageId::from_page_num(1).unwrap(),
                PageId::from_page_num(999).unwrap(), // doesn't exist
            ])
            .await;

        let stats = cache.stats().await;
        // Should only count the one that was actually dirty
        assert_eq!(stats.flushes, 1);
    }

    #[tokio::test]
    async fn cache_get_miss_explicit() {
        let cache = PageCache::new(10);

        // Explicit test that get on non-existent page returns None
        let result = cache.get(PageId::from_page_num(999).unwrap()).await;
        assert!(result.is_none());

        // Stats should show a miss
        let stats = cache.stats().await;
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hits, 0);
    }

    #[tokio::test]
    async fn cache_put_update_at_capacity_no_eviction() {
        let cache = PageCache::new(2);

        let id1 = PageId::from_page_num(1).unwrap();
        let id2 = PageId::from_page_num(2).unwrap();

        // Fill cache
        cache.put(id1, make_node(1), false).await;
        cache.put(id2, make_node(2), false).await;

        assert_eq!(cache.len().await, 2);

        // Update existing page - should NOT evict
        let evicted = cache.put(id1, make_node(100), true).await;
        assert!(evicted.is_none());
        assert_eq!(cache.len().await, 2);

        // Verify update happened
        let node = cache.get(id1).await.unwrap();
        assert_eq!(node.values.len(), 1);
    }

    #[tokio::test]
    async fn cache_put_same_page_twice() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true).await;
        cache.put(id, make_node(2), false).await;

        // Should still only have 1 entry
        assert_eq!(cache.len().await, 1);

        // Second put with dirty=false should clear dirty
        assert!(!cache.is_dirty(id).await);
    }

    #[tokio::test]
    async fn cache_mark_dirty_already_dirty() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true).await;
        assert!(cache.is_dirty(id).await);

        // Mark dirty again - should be idempotent
        let result = cache.mark_dirty(id).await;
        assert_eq!(result, MarkDirtyResult::Marked);
        assert!(cache.is_dirty(id).await);

        // Dirty count should still be 1
        assert_eq!(cache.dirty_count().await, 1);
    }

    #[tokio::test]
    async fn cache_mark_flushed_empty_slice() {
        let cache = PageCache::new(10);
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), true)
            .await;

        // Flush with empty slice - should be no-op
        cache.mark_flushed(&[]).await;

        // Original page should still be dirty
        assert!(cache.is_dirty(PageId::from_page_num(1).unwrap()).await);
        assert_eq!(cache.stats().await.flushes, 0);
    }

    #[tokio::test]
    async fn cache_mark_flushed_duplicate_in_slice() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();
        cache.put(id, make_node(1), true).await;

        // Flush same page twice in one call
        cache.mark_flushed(&[id, id]).await;

        // Should only count as 1 flush (second one wasn't dirty anymore)
        let stats = cache.stats().await;
        assert_eq!(stats.flushes, 1);
    }

    #[tokio::test]
    async fn cache_remove_nonexistent() {
        let cache = PageCache::new(10);

        // Remove page that doesn't exist
        let result = cache.remove(PageId::from_page_num(999).unwrap()).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn cache_clear_empty() {
        let cache = PageCache::new(10);

        // Clear empty cache
        let discarded = cache.clear().await;
        assert!(discarded.is_empty());
    }

    #[tokio::test]
    async fn cache_clear_only_clean_pages() {
        let cache = PageCache::new(10);

        // Add only clean pages
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        assert_eq!(cache.len().await, 2);
        assert_eq!(cache.dirty_count().await, 0);

        // Clear should return empty (no dirty pages to report)
        let discarded = cache.clear().await;
        assert!(discarded.is_empty());
        assert!(cache.is_empty().await);
    }

    #[tokio::test]
    async fn cache_dirty_pages_empty_when_none() {
        let cache = PageCache::new(10);

        // Add only clean pages
        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        let dirty = cache.dirty_pages().await;
        assert!(dirty.is_empty());
    }

    #[tokio::test]
    async fn cache_is_dirty_after_eviction() {
        let cache = PageCache::new(2);

        let id1 = PageId::from_page_num(1).unwrap();
        cache.put(id1, make_node(1), true).await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        // Evict id1 (dirty)
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;

        // Evicted page should not be in dirty set
        assert!(!cache.is_dirty(id1).await);
    }

    #[tokio::test]
    async fn cache_stats_evictions_accurate() {
        let cache = PageCache::new(2);

        cache
            .put(PageId::from_page_num(1).unwrap(), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2).unwrap(), make_node(2), false)
            .await;

        // This should evict
        cache
            .put(PageId::from_page_num(3).unwrap(), make_node(3), false)
            .await;
        cache
            .put(PageId::from_page_num(4).unwrap(), make_node(4), false)
            .await;

        let stats = cache.stats().await;
        assert_eq!(stats.evictions, 2);
    }

    #[tokio::test]
    async fn cache_remove_dirty_page() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1).unwrap();

        cache.put(id, make_node(1), true).await;
        assert!(cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 1);

        // Remove should also clear from dirty set
        cache.remove(id).await;

        assert!(!cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 0);
    }
}
