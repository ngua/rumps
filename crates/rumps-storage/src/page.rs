//! Page-based storage for RUMPS.
//!
//! This module provides:
//! - [`PAGE_SIZE`]: Compile-time constant for B-tree node page size
//! - [`PageId`]: Identifier for a page (byte offset into the data file)
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

use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::ops::Deref;
use std::sync::Arc;

use lru::LruCache;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::node::Node;

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

/// Identifier for a page in the data file.
///
/// `PageId` represents the byte offset into the data file where a page begins.
/// Each page is `PAGE_SIZE` bytes. Page 0 is reserved for metadata/header.
///
/// # Relationship to `NodeId`
///
/// - `PageId` is specific to disk storage (byte offset in data file)
/// - `NodeId` is a logical identifier used by the B-tree
/// - For persistent globals: `NodeId` maps to `PageId`
/// - For locals: `NodeId` maps to in-memory index
#[repr(transparent)]
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize
)]
pub(crate) struct PageId(u64);

impl PageId {
    /// Reserved page ID for the file header/metadata.
    pub const HEADER: Self = Self(0);

    /// Create a `PageId` from a page number (not byte offset).
    ///
    /// Page `n` starts at byte offset `n * PAGE_SIZE`.
    pub fn from_page_num(n: u64) -> Self {
        Self(n * PAGE_SIZE as u64)
    }

    /// Get the page number (0-indexed).
    pub fn page_num(self) -> u64 {
        self.0 / PAGE_SIZE as u64
    }

    /// Get the byte offset in the data file.
    pub fn offset(self) -> u64 {
        self.0
    }

    /// Check if this is the header page.
    pub fn is_header(self) -> bool {
        self.0 == 0
    }
}

impl Deref for PageId {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u64> for PageId {
    /// Create a `PageId` from a byte offset.
    fn from(offset: u64) -> Self {
        Self(offset)
    }
}

impl From<PageId> for u64 {
    fn from(id: PageId) -> Self {
        id.0
    }
}

/// A cached page entry with its data and dirty flag.
#[derive(Debug, Clone)]
struct CachedPage {
    /// The node stored in this page.
    node: Arc<Node>,
    /// Whether this page has been modified since last flush.
    dirty: bool,
}

/// Statistics for the page cache.
#[derive(Debug, Clone, Default)]
pub(crate) struct PageCacheStats {
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
/// If the evicted page is dirty, it must be flushed before eviction (the caller
/// is responsible for handling this via the `evict_dirty` callback).
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
    pub fn new(capacity: usize) -> Self {
        // SAFETY: 1 is non-zero
        const ONE: NonZeroUsize = match NonZeroUsize::new(1) {
            Some(n) => n,
            None => unreachable!(),
        };
        let cap = NonZeroUsize::new(capacity).unwrap_or(ONE);
        Self {
            cache: RwLock::new(LruCache::new(cap)),
            dirty_set: RwLock::new(HashSet::new()),
            stats: RwLock::new(PageCacheStats::default()),
            capacity,
        }
    }

    /// Get a page from the cache.
    ///
    /// Returns `None` if the page is not in cache (cache miss).
    /// Updates LRU order on hit.
    pub async fn get(&self, id: PageId) -> Option<Arc<Node>> {
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
    /// If `dirty` is true, the page is marked as needing flush.
    /// Returns the evicted page if the cache was at capacity.
    pub async fn put(
        &self,
        id: PageId,
        node: Node,
        dirty: bool,
    ) -> Option<(PageId, Arc<Node>)> {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        if dirty {
            dirty_set.insert(id);
            stats.writes += 1;
        }

        let entry = CachedPage {
            node: Arc::new(node),
            dirty,
        };

        // Check if we'll evict something
        let evicted = (cache.len() >= self.capacity && !cache.contains(&id))
            .then(|| {
                cache.peek_lru().map(|(evict_id, evict_entry)| {
                    let evict_id = *evict_id;
                    let evict_node = Arc::clone(&evict_entry.node);
                    stats.evictions += 1;
                    dirty_set.remove(&evict_id);
                    (evict_id, evict_node)
                })
            })
            .flatten();

        cache.put(id, entry);

        evicted
    }

    /// Mark a page as dirty (needs flush).
    ///
    /// Returns `false` if the page is not in cache.
    pub async fn mark_dirty(&self, id: PageId) -> bool {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        cache
            .get_mut(&id)
            .map(|entry| {
                entry.dirty = true;
                dirty_set.insert(id);
                stats.writes += 1;
            })
            .is_some()
    }

    /// Get all dirty pages.
    ///
    /// Returns a list of `(PageId, Node)` pairs for pages that need flushing.
    pub async fn dirty_pages(&self) -> Vec<(PageId, Arc<Node>)> {
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
    pub async fn mark_flushed(&self, ids: &[PageId]) {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;
        let mut stats = self.stats.write().await;

        ids.iter().for_each(|&id| {
            dirty_set.remove(&id);
            stats.flushes += 1;
            if let Some(entry) = cache.get_mut(&id) {
                entry.dirty = false;
            }
        });
    }

    /// Clear the dirty flag for a specific page.
    pub async fn mark_clean(&self, id: PageId) {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;

        dirty_set.remove(&id);
        if let Some(entry) = cache.get_mut(&id) {
            entry.dirty = false;
        }
    }

    /// Check if a page is dirty.
    pub async fn is_dirty(&self, id: PageId) -> bool {
        self.dirty_set.read().await.contains(&id)
    }

    /// Get the number of dirty pages.
    pub async fn dirty_count(&self) -> usize {
        self.dirty_set.read().await.len()
    }

    /// Remove a page from the cache.
    ///
    /// Returns the removed page if it was in cache.
    pub async fn remove(&self, id: PageId) -> Option<Arc<Node>> {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;

        dirty_set.remove(&id);
        cache.pop(&id).map(|entry| entry.node)
    }

    /// Clear all pages from the cache.
    ///
    /// **Warning**: This discards dirty pages without flushing!
    pub async fn clear(&self) {
        let mut cache = self.cache.write().await;
        let mut dirty_set = self.dirty_set.write().await;

        cache.clear();
        dirty_set.clear();
    }

    /// Get the number of pages in cache.
    pub async fn len(&self) -> usize {
        self.cache.read().await.len()
    }

    /// Check if the cache is empty.
    pub async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }

    /// Get the cache capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get cache statistics.
    pub async fn stats(&self) -> PageCacheStats {
        self.stats.read().await.clone()
    }

    /// Reset cache statistics.
    pub async fn reset_stats(&self) {
        *self.stats.write().await = PageCacheStats::default();
    }

    /// Calculate the hit rate (0.0 to 1.0).
    pub async fn hit_rate(&self) -> f64 {
        let stats = self.stats.read().await;
        let total = stats.hits + stats.misses;
        if total == 0 {
            0.0
        } else {
            stats.hits as f64 / total as f64
        }
    }
}

impl std::fmt::Debug for PageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageCache")
            .field("capacity", &self.capacity)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use rumps_types::Key;

    use super::*;
    use crate::node::NodeData;

    fn make_node(id: u64) -> Node {
        let mut node = Node::new_leaf();
        node.keys.push(Key::from(vec![(id as i64).into()]));
        node.values
            .push(Arc::new(NodeData::new(Some((id as i64).into()), false)));
        node
    }

    // PageId tests

    #[test]
    fn page_id_from_page_num() {
        let id = PageId::from_page_num(0);
        assert_eq!(id.offset(), 0);
        assert_eq!(id.page_num(), 0);

        let id = PageId::from_page_num(1);
        assert_eq!(id.offset(), PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 1);

        let id = PageId::from_page_num(10);
        assert_eq!(id.offset(), 10 * PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 10);
    }

    #[test]
    fn page_id_header() {
        assert!(PageId::HEADER.is_header());
        assert_eq!(PageId::HEADER.offset(), 0);
        assert!(!PageId::from_page_num(1).is_header());
    }

    #[test]
    fn page_id_conversions() {
        let offset = 8192u64;
        let id: PageId = offset.into();
        assert_eq!(u64::from(id), offset);
    }

    #[test]
    fn page_size_is_power_of_two() {
        assert!(PAGE_SIZE.is_power_of_two());
    }

    #[test]
    fn page_size_reasonable() {
        assert!(PAGE_SIZE >= 512, "page size too small");
        assert!(PAGE_SIZE <= 65536, "page size too large");
    }

    // PageCache tests

    #[tokio::test]
    async fn cache_basic_get_put() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1);
        let node = make_node(1);

        assert!(cache.get(id).await.is_none());

        cache.put(id, node.clone(), false).await;

        let retrieved = cache.get(id).await.expect("should be in cache");
        assert_eq!(*retrieved, node);
    }

    #[tokio::test]
    async fn cache_dirty_tracking() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1);
        let node = make_node(1);

        // Insert clean
        cache.put(id, node.clone(), false).await;
        assert!(!cache.is_dirty(id).await);
        assert_eq!(cache.dirty_count().await, 0);

        // Mark dirty
        assert!(cache.mark_dirty(id).await);
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
        let id = PageId::from_page_num(1);
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
            .put(PageId::from_page_num(1), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2), make_node(2), true)
            .await;
        cache
            .put(PageId::from_page_num(3), make_node(3), false)
            .await;
        cache
            .put(PageId::from_page_num(4), make_node(4), true)
            .await;

        let dirty = cache.dirty_pages().await;
        assert_eq!(dirty.len(), 2);

        let ids: HashSet<_> = dirty.iter().map(|(id, _)| *id).collect();
        assert!(ids.contains(&PageId::from_page_num(2)));
        assert!(ids.contains(&PageId::from_page_num(4)));
    }

    #[tokio::test]
    async fn cache_mark_flushed() {
        let cache = PageCache::new(10);

        cache
            .put(PageId::from_page_num(1), make_node(1), true)
            .await;
        cache
            .put(PageId::from_page_num(2), make_node(2), true)
            .await;
        cache
            .put(PageId::from_page_num(3), make_node(3), true)
            .await;

        assert_eq!(cache.dirty_count().await, 3);

        cache
            .mark_flushed(&[PageId::from_page_num(1), PageId::from_page_num(3)])
            .await;

        assert_eq!(cache.dirty_count().await, 1);
        assert!(cache.is_dirty(PageId::from_page_num(2)).await);
        assert!(!cache.is_dirty(PageId::from_page_num(1)).await);
        assert!(!cache.is_dirty(PageId::from_page_num(3)).await);
    }

    #[tokio::test]
    async fn cache_eviction() {
        let cache = PageCache::new(3);

        // Fill cache
        cache
            .put(PageId::from_page_num(1), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2), make_node(2), false)
            .await;
        cache
            .put(PageId::from_page_num(3), make_node(3), false)
            .await;

        assert_eq!(cache.len().await, 3);

        // This should evict page 1 (LRU)
        let evicted = cache
            .put(PageId::from_page_num(4), make_node(4), false)
            .await;

        assert!(evicted.is_some());
        let (evict_id, _) = evicted.unwrap();
        assert_eq!(evict_id, PageId::from_page_num(1));

        assert!(cache.get(PageId::from_page_num(1)).await.is_none());
        assert!(cache.get(PageId::from_page_num(4)).await.is_some());
    }

    #[tokio::test]
    async fn cache_lru_order() {
        let cache = PageCache::new(3);

        cache
            .put(PageId::from_page_num(1), make_node(1), false)
            .await;
        cache
            .put(PageId::from_page_num(2), make_node(2), false)
            .await;
        cache
            .put(PageId::from_page_num(3), make_node(3), false)
            .await;

        // Access page 1 to make it recently used
        cache.get(PageId::from_page_num(1)).await;

        // Now page 2 should be LRU
        let evicted = cache
            .put(PageId::from_page_num(4), make_node(4), false)
            .await;

        let (evict_id, _) = evicted.unwrap();
        assert_eq!(evict_id, PageId::from_page_num(2));
    }

    #[tokio::test]
    async fn cache_remove() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1);

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
            .put(PageId::from_page_num(1), make_node(1), true)
            .await;
        cache
            .put(PageId::from_page_num(2), make_node(2), false)
            .await;

        assert_eq!(cache.len().await, 2);
        assert_eq!(cache.dirty_count().await, 1);

        cache.clear().await;

        assert!(cache.is_empty().await);
        assert_eq!(cache.dirty_count().await, 0);
    }

    #[tokio::test]
    async fn cache_stats() {
        let cache = PageCache::new(10);
        let id = PageId::from_page_num(1);

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
        let id = PageId::from_page_num(1);

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
        let id = PageId::from_page_num(1);

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
        let id = PageId::from_page_num(1);

        assert!(!cache.mark_dirty(id).await);
    }
}
