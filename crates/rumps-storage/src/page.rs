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
use rumps_types::{Error, Result};
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

/// Result of attempting to mark a page as allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkAllocatedResult {
    /// The page was free and is now marked as allocated.
    NewlyAllocated,
    /// The page was already allocated; no change was made.
    AlreadyAllocated,
}

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
    pub(crate) const HEADER: Self = Self(0);

    /// Create a `PageId` from a page number (not byte offset).
    ///
    /// Page `n` starts at byte offset `n * PAGE_SIZE`.
    /// Returns an error if `n * PAGE_SIZE` would overflow.
    pub(crate) fn from_page_num(n: u64) -> Result<Self> {
        n.checked_mul(PAGE_SIZE as u64)
            .map(Self)
            .ok_or(Error::PageNumberOverflow(n))
    }

    /// Get the page number (0-indexed).
    pub(crate) fn page_num(self) -> u64 {
        self.0 / PAGE_SIZE as u64
    }

    /// Get the byte offset in the data file.
    pub(crate) fn offset(self) -> u64 {
        self.0
    }

    /// Check if this is the header page.
    pub(crate) fn is_header(self) -> bool {
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

// ----------------------------------------------------------------------------
// Bitmap
// ----------------------------------------------------------------------------

/// Result of setting a bit in the bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetBitResult {
    /// Bit was `0`, now `1`.
    WasUnset,
    /// Bit was already `1`.
    WasSet,
}

/// Result of clearing a bit in the bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClearBitResult {
    /// Bit was `1`, now `0`.
    WasSet,
    /// Bit was already `0`.
    WasUnset,
    /// Index out of bounds.
    OutOfBounds,
}

/// Compact bitmap for tracking allocated/free status of pages.
///
/// Each bit represents one page: `1` = allocated, `0` = free.
/// Internally stored as `Vec<u64>` where each word tracks 64 pages.
#[derive(Debug, Clone)]
struct Bitmap {
    words: Vec<u64>,
}

impl Bitmap {
    /// Bits per word.
    const BITS_PER_WORD: usize = 64;

    /// Create a new bitmap with capacity for at least `bits` bits.
    ///
    /// Rounds up to the nearest word boundary. Minimum capacity is 64 bits.
    fn new(bits: usize) -> Self {
        let bits = bits.max(Self::BITS_PER_WORD);
        let words = bits.div_ceil(Self::BITS_PER_WORD);
        Self {
            words: vec![0u64; words],
        }
    }

    /// Restore a bitmap from serialized bytes.
    ///
    /// Returns `None` if bytes is empty or not a multiple of 8.
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        (!bytes.is_empty()).then(|| Self {
            words: bytes
                .chunks_exact(8)
                .map(|chunk| {
                    // SAFETY: `chunks_exact(8)` guarantees exactly 8 bytes per chunk.
                    #[allow(clippy::unwrap_used)]
                    u64::from_le_bytes(chunk.try_into().unwrap())
                })
                .collect(),
        })
    }

    /// Serialize the bitmap to bytes.
    fn to_bytes(&self) -> Vec<u8> {
        self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    /// Check if the bit at `idx` is set.
    ///
    /// Returns `None` if `idx` is out of bounds.
    fn get(&self, idx: usize) -> Option<bool> {
        let (word_idx, bit_idx) =
            (idx / Self::BITS_PER_WORD, idx % Self::BITS_PER_WORD);
        self.words.get(word_idx).map(|&w| (w & (1 << bit_idx)) != 0)
    }

    /// Set the bit at `idx`.
    ///
    /// Returns whether the bit was already set, or `None` if out of bounds.
    fn set(&mut self, idx: usize) -> Option<SetBitResult> {
        let (word_idx, bit_idx) =
            (idx / Self::BITS_PER_WORD, idx % Self::BITS_PER_WORD);
        self.words.get_mut(word_idx).map(|word| {
            let mask = 1 << bit_idx;
            let was_set = (*word & mask) != 0;
            *word |= mask;
            if was_set {
                SetBitResult::WasSet
            } else {
                SetBitResult::WasUnset
            }
        })
    }

    /// Set the bit at `idx`, after ensuring capacity.
    ///
    /// Extends the bitmap if necessary.
    fn set_with_extend(&mut self, idx: usize) -> SetBitResult {
        self.ensure_capacity(idx);
        // SAFETY: ensure_capacity guarantees idx is in bounds
        #[allow(clippy::unwrap_used)]
        self.set(idx).unwrap()
    }

    /// Clear the bit at `idx`.
    ///
    /// Returns whether the bit was set, or `OutOfBounds` if `idx` is invalid.
    fn clear(&mut self, idx: usize) -> ClearBitResult {
        let (word_idx, bit_idx) =
            (idx / Self::BITS_PER_WORD, idx % Self::BITS_PER_WORD);
        self.words.get_mut(word_idx).map_or(
            ClearBitResult::OutOfBounds,
            |word| {
                let mask = 1 << bit_idx;
                let was_set = (*word & mask) != 0;
                *word &= !mask;
                if was_set {
                    ClearBitResult::WasSet
                } else {
                    ClearBitResult::WasUnset
                }
            },
        )
    }

    /// Find the first free (unset) bit starting from `hint`, wrapping around.
    ///
    /// Returns `Some((idx, word_idx))` with the bit index and word index,
    /// or `None` if all bits are set.
    fn find_free_from(&self, hint: usize) -> Option<(usize, usize)> {
        let len = self.words.len();
        let hint = hint.min(len.saturating_sub(1));

        (hint..len).chain(0..hint).find_map(|word_idx| {
            self.words
                .get(word_idx)
                .filter(|&&w| w != u64::MAX)
                .map(|&w| {
                    let bit_idx = w.trailing_ones() as usize;
                    (word_idx * Self::BITS_PER_WORD + bit_idx, word_idx)
                })
        })
    }

    /// Ensure the bitmap can hold bit `idx`, extending if necessary.
    ///
    /// Returns the word index for `idx`.
    fn ensure_capacity(&mut self, idx: usize) -> usize {
        let word_idx = idx / Self::BITS_PER_WORD;
        if word_idx >= self.words.len() {
            self.words.resize(word_idx + 1, 0);
        }
        word_idx
    }

    /// Extend the bitmap by one word and set bit 0 of that word.
    ///
    /// Returns the bit index of the newly set bit.
    fn extend_and_set_first(&mut self) -> usize {
        let word_idx = self.words.len();
        self.words.push(1);
        word_idx * Self::BITS_PER_WORD
    }

    /// Total number of bits the bitmap can track.
    fn capacity(&self) -> usize {
        self.words.len() * Self::BITS_PER_WORD
    }

    /// Count the number of set bits.
    fn count_ones(&self) -> u64 {
        self.words.iter().map(|w| w.count_ones() as u64).sum()
    }

    /// Number of words in the bitmap.
    fn word_count(&self) -> usize {
        self.words.len()
    }
}

// ----------------------------------------------------------------------------
// PageAllocator
// ----------------------------------------------------------------------------

/// Mutable state for the page allocator, protected by `RwLock`.
struct PageAllocatorState {
    /// Bitmap tracking allocated pages.
    bitmap: Bitmap,

    /// Number of currently allocated pages (cached for O(1) lookup).
    allocated: u64,

    /// Hint for where to start searching for free pages (word index).
    search_hint: usize,
}

/// Bitmap-based page allocator for tracking free and allocated pages.
///
/// Uses a compact bitmap representation where each bit indicates whether
/// a page is allocated (`1`) or free (`0`). Page 0 is always reserved
/// for the file header.
///
/// # Thread Safety
///
/// All mutable state is protected by a single `RwLock`, ensuring atomic
/// operations and preventing race conditions.
///
/// # Persistence
///
/// The bitmap can be serialized via [`to_bytes`] and restored via [`from_bytes`]
/// for crash recovery. The bitmap should be persisted as part of the file header
/// or in a dedicated metadata region.
///
/// [`to_bytes`]: Self::to_bytes
/// [`from_bytes`]: Self::from_bytes
pub(crate) struct PageAllocator {
    /// All mutable state, protected by a single lock.
    state: RwLock<PageAllocatorState>,

    /// Maximum number of pages that can be allocated. `None` means unlimited.
    max_pages: Option<u64>,
}

impl PageAllocator {
    /// Maximum trackable pages (defense against memory exhaustion from crafted `PageId`).
    ///
    /// At 4KB pages, `2^52` pages = ~16 petabytes, far beyond any realistic use.
    const MAX_TRACKABLE_PAGES: u64 = 1 << 52;

    /// Create a new allocator with the given initial capacity (in pages).
    ///
    /// Page 0 is automatically marked as allocated (reserved for header).
    /// If `initial_pages` is 0, defaults to 64 pages.
    pub(crate) fn new(initial_pages: u64) -> Self {
        Self::with_limit(initial_pages, None)
    }

    /// Create a new allocator with a maximum page limit.
    ///
    /// Page 0 is automatically marked as allocated (reserved for header).
    /// If `initial_pages` is 0, defaults to 64 pages.
    pub(crate) fn with_limit(
        initial_pages: u64,
        max_pages: Option<u64>,
    ) -> Self {
        let mut bitmap = Bitmap::new(initial_pages as usize);
        // Reserve page 0 for header (Bitmap::new guarantees capacity >= 64).
        bitmap.set(0);

        Self {
            state: RwLock::new(PageAllocatorState {
                bitmap,
                allocated: 1, // page 0 is allocated
                search_hint: 0,
            }),
            max_pages,
        }
    }

    /// Restore an allocator from a serialized bitmap.
    ///
    /// Used during crash recovery to restore the allocation state.
    /// Returns an error if the bitmap is empty (must have at least one word
    /// for the reserved header page).
    pub(crate) fn from_bytes(
        bytes: &[u8],
        max_pages: Option<u64>,
    ) -> Result<Self> {
        Bitmap::from_bytes(bytes).map_or(Err(Error::InvalidBitmap), |bitmap| {
            let allocated = bitmap.count_ones();
            Ok(Self {
                state: RwLock::new(PageAllocatorState {
                    bitmap,
                    allocated,
                    search_hint: 0,
                }),
                max_pages,
            })
        })
    }

    /// Serialize the bitmap to bytes for persistence.
    pub(crate) async fn to_bytes(&self) -> Vec<u8> {
        self.state.read().await.bitmap.to_bytes()
    }

    /// Allocate a free page and return its ID.
    ///
    /// Returns an error if the page limit has been reached.
    pub(crate) async fn allocate(&self) -> Result<PageId> {
        let mut state = self.state.write().await;

        // Check limit (inside lock to prevent races)
        if let Some(max) = self.max_pages {
            if state.allocated >= max {
                Err(Error::PageLimitExceeded(max))
            } else {
                Self::allocate_inner(&mut state)
            }
        } else {
            Self::allocate_inner(&mut state)
        }
    }

    /// Inner allocation logic (extracted to avoid duplication in the limit check branches).
    fn allocate_inner(state: &mut PageAllocatorState) -> Result<PageId> {
        let page_num = state
            .bitmap
            .find_free_from(state.search_hint)
            .map(|(bit_idx, word_idx)| {
                state.bitmap.set(bit_idx);
                state.search_hint = word_idx;
                bit_idx
            })
            .unwrap_or_else(|| {
                // No free pages - extend bitmap
                let bit_idx = state.bitmap.extend_and_set_first();
                state.search_hint = state.bitmap.word_count() - 1;
                bit_idx
            });

        state.allocated += 1;
        PageId::from_page_num(page_num as u64)
    }

    /// Free a previously allocated page.
    ///
    /// Returns an error if:
    /// - The page is page 0 (reserved header page)
    /// - The page is beyond the bitmap bounds
    /// - The page is not currently allocated
    pub(crate) async fn free(&self, id: PageId) -> Result<()> {
        let page_num = id.page_num();

        // Prevent freeing the reserved header page
        if page_num == 0 {
            Err(Error::CannotFreeHeaderPage)
        } else {
            let idx = page_num as usize;
            let mut state = self.state.write().await;

            match state.bitmap.clear(idx) {
                ClearBitResult::OutOfBounds => {
                    Err(Error::PageOutOfBounds(page_num))
                }
                ClearBitResult::WasUnset => {
                    Err(Error::PageNotAllocated(page_num))
                }
                ClearBitResult::WasSet => {
                    state.allocated = state.allocated.saturating_sub(1);
                    // Update hint if this page is before current hint
                    let word_idx = idx / Bitmap::BITS_PER_WORD;
                    if word_idx < state.search_hint {
                        state.search_hint = word_idx;
                    }
                    Ok(())
                }
            }
        }
    }

    /// Check if a page is currently allocated.
    ///
    /// Returns `false` for pages beyond the current bitmap bounds.
    pub(crate) async fn is_allocated(&self, id: PageId) -> bool {
        let idx = id.page_num() as usize;
        self.state.read().await.bitmap.get(idx).unwrap_or(false)
    }

    /// Mark a specific page as allocated.
    ///
    /// Used during recovery to rebuild allocation state from WAL.
    ///
    /// Returns an error for pages beyond [`Self::MAX_TRACKABLE_PAGES`]
    /// to prevent memory exhaustion from malformed/malicious `PageId` values.
    ///
    /// **Note**: This method ignores `max_pages` limit, as recovery must
    /// restore the exact state from the WAL regardless of current limits.
    pub(crate) async fn mark_allocated(
        &self,
        id: PageId,
    ) -> Result<MarkAllocatedResult> {
        let page_num = id.page_num();

        // Reject absurdly large page numbers to prevent memory exhaustion.
        if page_num >= Self::MAX_TRACKABLE_PAGES {
            Err(Error::PageOutOfBounds(page_num))
        } else {
            let idx = page_num as usize;
            let mut state = self.state.write().await;

            Ok(match state.bitmap.set_with_extend(idx) {
                SetBitResult::WasUnset => {
                    state.allocated += 1;
                    MarkAllocatedResult::NewlyAllocated
                }
                SetBitResult::WasSet => MarkAllocatedResult::AlreadyAllocated,
            })
        }
    }

    /// Get the number of allocated pages.
    pub(crate) async fn allocated_count(&self) -> u64 {
        self.state.read().await.allocated
    }

    /// Get the total capacity (number of pages the bitmap can track).
    pub(crate) async fn capacity(&self) -> u64 {
        self.state.read().await.bitmap.capacity() as u64
    }

    /// Get the number of free pages.
    pub(crate) async fn free_count(&self) -> u64 {
        let state = self.state.read().await;
        (state.bitmap.capacity() as u64).saturating_sub(state.allocated)
    }
}

impl std::fmt::Debug for PageAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageAllocator")
            .field("max_pages", &self.max_pages)
            .finish_non_exhaustive()
    }
}

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
pub(crate) struct PageCacheStats {
    /// Number of cache hits.
    pub(crate) hits: u64,
    /// Number of cache misses.
    pub(crate) misses: u64,
    /// Number of pages written (marked dirty).
    pub(crate) writes: u64,
    /// Number of pages flushed to disk.
    pub(crate) flushes: u64,
    /// Number of pages evicted from cache.
    pub(crate) evictions: u64,
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

    /// Reset cache statistics.
    pub(crate) async fn reset_stats(&self) {
        *self.stats.write().await = PageCacheStats::default();
    }

    /// Calculate the hit rate (0.0 to 1.0).
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
    use rumps_types::{Error, Key};

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
        let id = PageId::from_page_num(0).unwrap();
        assert_eq!(id.offset(), 0);
        assert_eq!(id.page_num(), 0);

        let id = PageId::from_page_num(1).unwrap();
        assert_eq!(id.offset(), PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 1);

        let id = PageId::from_page_num(10).unwrap();
        assert_eq!(id.offset(), 10 * PAGE_SIZE as u64);
        assert_eq!(id.page_num(), 10);
    }

    #[test]
    fn page_id_header() {
        assert!(PageId::HEADER.is_header());
        assert_eq!(PageId::HEADER.offset(), 0);
        assert!(!PageId::from_page_num(1).unwrap().is_header());
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

    // PageAllocator tests

    #[tokio::test]
    async fn allocator_new_reserves_page_zero() {
        let alloc = PageAllocator::new(64);

        assert!(alloc.is_allocated(PageId::HEADER).await);
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_allocate_sequential() {
        let alloc = PageAllocator::new(64);

        // Page 0 is reserved, so first allocation should be page 1
        let p1 = alloc.allocate().await.unwrap();
        assert_eq!(p1.page_num(), 1);

        let p2 = alloc.allocate().await.unwrap();
        assert_eq!(p2.page_num(), 2);

        let p3 = alloc.allocate().await.unwrap();
        assert_eq!(p3.page_num(), 3);

        assert_eq!(alloc.allocated_count().await, 4); // 0, 1, 2, 3
    }

    #[tokio::test]
    async fn allocator_free_and_reuse() {
        let alloc = PageAllocator::new(64);

        let p1 = alloc.allocate().await.unwrap();
        let p2 = alloc.allocate().await.unwrap();
        let p3 = alloc.allocate().await.unwrap();

        assert_eq!(alloc.allocated_count().await, 4);

        // Free page 2
        alloc.free(p2).await.unwrap();
        assert_eq!(alloc.allocated_count().await, 3);
        assert!(!alloc.is_allocated(p2).await);

        // Next allocation should reuse page 2
        let p4 = alloc.allocate().await.unwrap();
        assert_eq!(p4.page_num(), p2.page_num());
        assert_eq!(alloc.allocated_count().await, 4);

        // Verify p1 and p3 still allocated
        assert!(alloc.is_allocated(p1).await);
        assert!(alloc.is_allocated(p3).await);
    }

    #[tokio::test]
    async fn allocator_extends_bitmap() {
        let alloc = PageAllocator::new(64);

        assert_eq!(alloc.capacity().await, 64);

        // Allocate all 64 pages (0 is reserved, so 63 more)
        let pages: Vec<_> =
            futures::future::join_all((0..63).map(|_| alloc.allocate()))
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

        assert_eq!(alloc.allocated_count().await, 64);
        assert_eq!(alloc.free_count().await, 0);

        // Next allocation should extend bitmap
        let p = alloc.allocate().await.unwrap();
        assert_eq!(p.page_num(), 64);
        assert_eq!(alloc.capacity().await, 128);

        // Verify all previous pages still allocated
        assert!(alloc.is_allocated(*pages.first().unwrap()).await);
        assert!(alloc.is_allocated(*pages.last().unwrap()).await);
    }

    #[tokio::test]
    async fn allocator_is_allocated() {
        let alloc = PageAllocator::new(64);

        assert!(alloc.is_allocated(PageId::HEADER).await);
        assert!(!alloc.is_allocated(PageId::from_page_num(1).unwrap()).await);

        let p = alloc.allocate().await.unwrap();
        assert!(alloc.is_allocated(p).await);

        alloc.free(p).await.unwrap();
        assert!(!alloc.is_allocated(p).await);
    }

    #[tokio::test]
    async fn allocator_mark_allocated() {
        let alloc = PageAllocator::new(64);

        let id = PageId::from_page_num(42).unwrap();

        // Mark as allocated
        assert_eq!(
            alloc.mark_allocated(id).await.unwrap(),
            MarkAllocatedResult::NewlyAllocated
        );
        assert!(alloc.is_allocated(id).await);

        // Mark again - should indicate already allocated
        assert_eq!(
            alloc.mark_allocated(id).await.unwrap(),
            MarkAllocatedResult::AlreadyAllocated
        );
    }

    #[tokio::test]
    async fn allocator_mark_allocated_extends() {
        let alloc = PageAllocator::new(64);

        // Mark a page beyond current capacity
        let id = PageId::from_page_num(100).unwrap();
        assert_eq!(
            alloc.mark_allocated(id).await.unwrap(),
            MarkAllocatedResult::NewlyAllocated
        );
        assert!(alloc.is_allocated(id).await);
        assert!(alloc.capacity().await >= 101);
    }

    #[test]
    fn page_id_from_page_num_overflow() {
        // Page number that would overflow when multiplied by PAGE_SIZE
        let huge = u64::MAX / PAGE_SIZE as u64 + 1;
        let err = PageId::from_page_num(huge).unwrap_err();
        assert!(matches!(err, Error::PageNumberOverflow(_)));
    }

    #[tokio::test]
    async fn allocator_serialization_roundtrip() {
        let alloc = PageAllocator::new(128);

        // Allocate some pages
        let p1 = alloc.allocate().await.unwrap();
        let p2 = alloc.allocate().await.unwrap();
        let _p3 = alloc.allocate().await.unwrap();
        alloc.free(p2).await.unwrap();

        let bytes = alloc.to_bytes().await;
        let restored = PageAllocator::from_bytes(&bytes, None).unwrap();

        assert_eq!(
            restored.allocated_count().await,
            alloc.allocated_count().await
        );
        assert!(restored.is_allocated(PageId::HEADER).await);
        assert!(restored.is_allocated(p1).await);
        assert!(!restored.is_allocated(p2).await);
    }

    #[test]
    fn bitmap_find_free() {
        // All zeros - first free is bit 0
        let bm = Bitmap::new(64);
        assert_eq!(bm.find_free_from(0), Some((0, 0)));

        // Bit 0 set - first free is bit 1
        let mut bm = Bitmap::new(64);
        bm.set(0);
        assert_eq!(bm.find_free_from(0), Some((1, 0)));

        // First 3 bits set - first free is bit 3
        let mut bm = Bitmap::new(64);
        bm.set(0);
        bm.set(1);
        bm.set(2);
        assert_eq!(bm.find_free_from(0), Some((3, 0)));

        // Bits 0,1,3 set (gap at 2) - first free is bit 2
        let mut bm = Bitmap::new(64);
        bm.set(0);
        bm.set(1);
        bm.set(3);
        assert_eq!(bm.find_free_from(0), Some((2, 0)));
    }

    #[test]
    fn bitmap_all_set_returns_none() {
        let mut bm = Bitmap::new(64);
        (0..64).for_each(|i| {
            bm.set(i);
        });
        assert_eq!(bm.find_free_from(0), None);
    }

    #[tokio::test]
    async fn allocator_free_count() {
        let alloc = PageAllocator::new(64);

        // Initially 63 free (page 0 reserved)
        assert_eq!(alloc.free_count().await, 63);

        alloc.allocate().await.unwrap();
        assert_eq!(alloc.free_count().await, 62);

        alloc.allocate().await.unwrap();
        alloc.allocate().await.unwrap();
        assert_eq!(alloc.free_count().await, 60);
    }

    #[tokio::test]
    async fn allocator_search_hint_optimization() {
        let alloc = PageAllocator::new(128);

        // Allocate pages 1-10
        let pages: Vec<_> =
            futures::future::join_all((0..10).map(|_| alloc.allocate()))
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

        // Free page 5
        alloc.free(*pages.get(4).unwrap()).await.unwrap();

        // Next allocation should find page 5 quickly due to hint
        let p = alloc.allocate().await.unwrap();
        assert_eq!(p.page_num(), 5);
    }

    #[tokio::test]
    async fn allocator_free_not_allocated() {
        let alloc = PageAllocator::new(64);

        // Try to free a page that was never allocated
        let id = PageId::from_page_num(10).unwrap();
        let err = alloc.free(id).await.unwrap_err();

        assert!(matches!(err, Error::PageNotAllocated(10)));
    }

    #[tokio::test]
    async fn allocator_free_double_free() {
        let alloc = PageAllocator::new(64);

        let p = alloc.allocate().await.unwrap();
        alloc.free(p).await.unwrap();

        // Try to free again
        let err = alloc.free(p).await.unwrap_err();

        assert!(matches!(err, Error::PageNotAllocated(_)));
    }

    #[tokio::test]
    async fn allocator_free_out_of_bounds() {
        let alloc = PageAllocator::new(64);

        // Try to free a page beyond the bitmap capacity
        let id = PageId::from_page_num(1000).unwrap();
        let err = alloc.free(id).await.unwrap_err();

        assert!(matches!(err, Error::PageOutOfBounds(1000)));
    }

    #[tokio::test]
    async fn allocator_limit_exceeded() {
        // Limit of 5 pages total (including reserved page 0)
        let alloc = PageAllocator::with_limit(64, Some(5));

        // Can allocate 4 more pages (page 0 is already allocated)
        alloc.allocate().await.unwrap(); // page 1
        alloc.allocate().await.unwrap(); // page 2
        alloc.allocate().await.unwrap(); // page 3
        alloc.allocate().await.unwrap(); // page 4

        assert_eq!(alloc.allocated_count().await, 5);

        // Next allocation should fail
        let err = alloc.allocate().await.unwrap_err();
        assert!(matches!(err, Error::PageLimitExceeded(5)));
    }

    #[tokio::test]
    async fn allocator_limit_with_free_reuse() {
        let alloc = PageAllocator::with_limit(64, Some(5));

        let _p1 = alloc.allocate().await.unwrap();
        let p2 = alloc.allocate().await.unwrap();
        alloc.allocate().await.unwrap();
        alloc.allocate().await.unwrap();

        // At limit
        assert!(alloc.allocate().await.is_err());

        // Free a page
        alloc.free(p2).await.unwrap();

        // Now we can allocate again
        let p = alloc.allocate().await.unwrap();
        assert_eq!(p.page_num(), p2.page_num());

        // But still at limit after that
        assert!(matches!(
            alloc.allocate().await.unwrap_err(),
            Error::PageLimitExceeded(5)
        ));
    }

    #[tokio::test]
    async fn allocator_from_bytes_empty_bitmap() {
        let empty: &[u8] = &[];
        let err = PageAllocator::from_bytes(empty, None).unwrap_err();
        assert!(matches!(err, Error::InvalidBitmap));
    }

    #[tokio::test]
    async fn allocator_cannot_free_page_zero() {
        let alloc = PageAllocator::new(64);
        let page_zero = PageId::from_page_num(0).unwrap();
        let err = alloc.free(page_zero).await.unwrap_err();
        assert!(matches!(err, Error::CannotFreeHeaderPage));
    }

    #[tokio::test]
    async fn allocator_limit_one_only_header() {
        // Limit of 1 means only page 0 (header) can exist
        let alloc = PageAllocator::with_limit(64, Some(1));

        // Already at limit (page 0 is reserved)
        assert_eq!(alloc.allocated_count().await, 1);

        // Cannot allocate any more
        let err = alloc.allocate().await.unwrap_err();
        assert!(matches!(err, Error::PageLimitExceeded(1)));
    }

    #[tokio::test]
    async fn allocator_reuses_from_beginning_after_free_all() {
        let alloc = PageAllocator::new(64);

        // Allocate several pages
        let p1 = alloc.allocate().await.unwrap();
        let p2 = alloc.allocate().await.unwrap();
        let p3 = alloc.allocate().await.unwrap();

        assert_eq!(p1.page_num(), 1);
        assert_eq!(p2.page_num(), 2);
        assert_eq!(p3.page_num(), 3);

        // Free all of them
        alloc.free(p1).await.unwrap();
        alloc.free(p2).await.unwrap();
        alloc.free(p3).await.unwrap();

        // Next allocation should start from the beginning (page 1)
        let p = alloc.allocate().await.unwrap();
        assert_eq!(p.page_num(), 1);
    }

    #[tokio::test]
    async fn allocator_from_bytes_partial_word_ignored() {
        // 8 bytes = 1 complete word, 7 more bytes = incomplete (should be ignored)
        let mut bytes = vec![0u8; 15];
        bytes[0] = 1; // Mark page 0 as allocated

        let alloc = PageAllocator::from_bytes(&bytes, None).unwrap();

        // Should only have 64 pages (1 word), partial bytes ignored
        assert_eq!(alloc.capacity().await, 64);
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_from_bytes_exact_multiple() {
        // 16 bytes = exactly 2 words
        let mut bytes = vec![0u8; 16];
        bytes[0] = 0b11; // Pages 0 and 1 allocated

        let alloc = PageAllocator::from_bytes(&bytes, None).unwrap();

        assert_eq!(alloc.capacity().await, 128);
        assert_eq!(alloc.allocated_count().await, 2);
        assert!(alloc.is_allocated(PageId::from_page_num(0).unwrap()).await);
        assert!(alloc.is_allocated(PageId::from_page_num(1).unwrap()).await);
        assert!(!alloc.is_allocated(PageId::from_page_num(2).unwrap()).await);
    }

    #[tokio::test]
    async fn allocator_mark_allocated_page_zero() {
        let alloc = PageAllocator::new(64);

        // Page 0 is already allocated
        assert_eq!(
            alloc.mark_allocated(PageId::HEADER).await.unwrap(),
            MarkAllocatedResult::AlreadyAllocated
        );

        // Count should not change
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_is_allocated_beyond_bounds() {
        let alloc = PageAllocator::new(64);

        // Page way beyond capacity should return false, not panic
        assert!(
            !alloc
                .is_allocated(PageId::from_page_num(10000).unwrap())
                .await
        );
    }

    #[tokio::test]
    async fn allocator_counts_after_free_all() {
        let alloc = PageAllocator::new(64);

        // Allocate all 63 available pages (64 - 1 for header)
        let pages: Vec<_> =
            futures::future::join_all((0..63).map(|_| alloc.allocate()))
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

        assert_eq!(alloc.allocated_count().await, 64);
        assert_eq!(alloc.free_count().await, 0);

        // Free all
        futures::future::join_all(pages.iter().map(|&p| alloc.free(p))).await;

        assert_eq!(alloc.allocated_count().await, 1); // Only page 0
        assert_eq!(alloc.free_count().await, 63);
    }

    #[tokio::test]
    async fn allocator_fill_word_then_extend() {
        let alloc = PageAllocator::new(64);

        // Allocate all 63 pages in first word
        let _pages: Vec<_> =
            futures::future::join_all((0..63).map(|_| alloc.allocate()))
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

        // First word should be full (all 64 bits set)
        assert_eq!(alloc.allocated_count().await, 64);
        assert_eq!(alloc.capacity().await, 64);

        // Next allocation should extend to second word
        let p = alloc.allocate().await.unwrap();
        assert_eq!(p.page_num(), 64);
        assert_eq!(alloc.capacity().await, 128);
    }
}
