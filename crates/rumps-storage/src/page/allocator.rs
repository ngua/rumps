//! Bitmap-based page allocator.

use std::collections::HashSet;
use std::fmt;

use tokio::sync::RwLock;

use super::PageId;
use crate::error::{Result, StorageError};

/// Result of attempting to mark a page as allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MarkAllocatedResult {
    /// The page was free and is now marked as allocated.
    NewlyAllocated,
    /// The page was already allocated; no change was made.
    AlreadyAllocated,
}

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
/// Internally stored as `BitVec<u64, Lsb0>` where each word tracks 64 pages.
///
/// Wraps [`bitvec::vec::BitVec`] with a compatible serialization format
/// (little-endian `u64` words) and additional methods for page allocation.
#[derive(Debug, Clone)]
#[repr(transparent)]
struct Bitmap(bitvec::vec::BitVec<u64, bitvec::order::Lsb0>);

impl Bitmap {
    /// Bits per word.
    const BITS_PER_WORD: usize = 64;

    /// Create a new bitmap with capacity for at least `bits` bits.
    ///
    /// Rounds up to the nearest word boundary. Minimum capacity is 64 bits.
    fn new(bits: usize) -> Self {
        let bits = bits.max(Self::BITS_PER_WORD);
        let rounded = bits.div_ceil(Self::BITS_PER_WORD) * Self::BITS_PER_WORD;
        Self(bitvec::bitvec![u64, bitvec::order::Lsb0; 0; rounded])
    }

    /// Restore a bitmap from serialized bytes.
    ///
    /// Returns `None` if bytes is empty. Partial words (trailing bytes
    /// not aligned to 8) are ignored.
    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        (!bytes.is_empty()).then(|| {
            let words: Vec<u64> = bytes
                .chunks_exact(8)
                .map(|chunk| {
                    // SAFETY: `chunks_exact(8)` guarantees exactly 8 bytes.
                    #[allow(clippy::unwrap_used)]
                    u64::from_le_bytes(chunk.try_into().unwrap())
                })
                .collect();
            Self(bitvec::vec::BitVec::from_vec(words))
        })
    }

    /// Serialize the bitmap to bytes (little-endian `u64` words).
    fn to_bytes(&self) -> Vec<u8> {
        self.0
            .as_raw_slice()
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect()
    }

    /// Check if the bit at `idx` is set.
    ///
    /// Returns `None` if `idx` is out of bounds.
    fn get(&self, idx: usize) -> Option<bool> {
        self.0.get(idx).map(|b| *b)
    }

    /// Set the bit at `idx`.
    ///
    /// Returns whether the bit was already set, or `None` if out of bounds.
    fn set(&mut self, idx: usize) -> Option<SetBitResult> {
        self.0.get_mut(idx).map(|mut bit| {
            let was_set = *bit;
            *bit = true;
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
        self.0
            .get_mut(idx)
            .map_or(ClearBitResult::OutOfBounds, |mut bit| {
                let was_set = *bit;
                *bit = false;
                if was_set {
                    ClearBitResult::WasSet
                } else {
                    ClearBitResult::WasUnset
                }
            })
    }

    /// Find the first free (unset) bit starting from `hint`, wrapping around.
    ///
    /// Returns `Some((idx, word_idx))` with the bit index and word index,
    /// or `None` if all bits are set.
    fn find_free_from(&self, hint: usize) -> Option<(usize, usize)> {
        let words = self.0.as_raw_slice();
        let len = words.len();
        let hint = hint.min(len.saturating_sub(1));

        (hint..len).chain(0..hint).find_map(|word_idx| {
            words.get(word_idx).filter(|&&w| w != u64::MAX).map(|&w| {
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
        let needed = (word_idx + 1) * Self::BITS_PER_WORD;
        if needed > self.0.len() {
            self.0.resize(needed, false);
        }
        word_idx
    }

    /// Extend the bitmap by one word and set bit 0 of that word.
    ///
    /// Returns the bit index of the newly set bit.
    fn extend_and_set_first(&mut self) -> usize {
        let bit_idx = self.0.len();
        self.0.resize(bit_idx + Self::BITS_PER_WORD, false);
        self.0.set(bit_idx, true);
        bit_idx
    }

    /// Total number of bits the bitmap can track.
    fn capacity(&self) -> usize {
        self.0.len()
    }

    /// Count the number of set bits.
    fn count_ones(&self) -> u64 {
        self.0.count_ones() as u64
    }

    /// Number of words in the bitmap.
    fn word_count(&self) -> usize {
        self.0.as_raw_slice().len()
    }
}

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
/// for the superblock, and additional pages (bitmap pages) may be reserved.
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

    /// Reserved pages that cannot be freed (superblock + bitmap pages).
    reserved: RwLock<HashSet<u64>>,
}

impl PageAllocator {
    /// Maximum trackable pages (defense against memory exhaustion from crafted `PageId`).
    ///
    /// At 4KB pages, `2^52` pages = ~16 petabytes, far beyond any realistic use.
    const MAX_TRACKABLE_PAGES: u64 = 1 << 52;

    /// Create a new allocator for a fresh database.
    ///
    /// Reserves pages 0-3 (superblock, bitmap, metadata, registry) as allocated
    /// and non-freeable. If `initial_bits` is 0, defaults to 64 bits.
    pub(crate) fn new(initial_bits: u64, max_pages: Option<u64>) -> Self {
        Self::with_reserved(initial_bits, max_pages, &[0, 1, 2, 3])
    }

    /// Create allocator with custom reserved pages.
    ///
    /// All pages in `reserved` are marked as allocated and cannot be freed.
    fn with_reserved(
        initial_bits: u64,
        max_pages: Option<u64>,
        reserved: &[u64],
    ) -> Self {
        let mut bitmap = Bitmap::new(initial_bits as usize);
        let reserved_set: HashSet<u64> = reserved.iter().copied().collect();

        // Mark all reserved pages as allocated
        reserved.iter().for_each(|&page_num| {
            bitmap.set_with_extend(page_num as usize);
        });

        Self {
            state: RwLock::new(PageAllocatorState {
                bitmap,
                allocated: reserved_set.len() as u64,
                search_hint: 0,
            }),
            max_pages,
            reserved: RwLock::new(reserved_set),
        }
    }

    /// Restore an allocator from a serialized bitmap.
    ///
    /// Used during crash recovery to restore the allocation state.
    /// Returns an error if the bitmap is empty (must have at least one word
    /// for the reserved header page).
    ///
    /// The `reserved` slice should contain page numbers that cannot be freed
    /// (superblock at 0 + any bitmap pages).
    pub(crate) fn from_bytes(
        bytes: &[u8],
        max_pages: Option<u64>,
        reserved: &[u64],
    ) -> Result<Self> {
        Bitmap::from_bytes(bytes).map_or(
            Err(StorageError::InvalidOperation("invalid bitmap".into())),
            |bitmap| {
                let allocated = bitmap.count_ones();
                Ok(Self {
                    state: RwLock::new(PageAllocatorState {
                        bitmap,
                        allocated,
                        search_hint: 0,
                    }),
                    max_pages,
                    reserved: RwLock::new(reserved.iter().copied().collect()),
                })
            },
        )
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
                Err(StorageError::MemoryLimitExceeded {
                    used: state.allocated as usize,
                    limit: max as usize,
                })
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
    /// - The page is reserved (superblock or bitmap page)
    /// - The page is beyond the bitmap bounds
    /// - The page is not currently allocated
    pub(crate) async fn free(&self, id: PageId) -> Result<()> {
        let page_num = id.page_num();

        // Prevent freeing reserved pages (superblock + bitmap pages)
        let reserved = self.reserved.read().await;
        if reserved.contains(&page_num) {
            // Use specific error for page 0, generic for others
            Err(StorageError::InvalidOperation(
                if page_num == 0 {
                    "cannot free header page".into()
                } else {
                    format!("cannot free reserved page {page_num}")
                },
            ))
        } else {
            drop(reserved);
            let idx = page_num as usize;
            let mut state = self.state.write().await;

            match state.bitmap.clear(idx) {
                ClearBitResult::OutOfBounds => {
                    Err(StorageError::InvalidOperation(format!(
                        "page {page_num} is out of bounds"
                    )))
                }
                ClearBitResult::WasUnset => {
                    Err(StorageError::InvalidOperation(format!(
                        "page {page_num} is not allocated"
                    )))
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
            Err(StorageError::InvalidOperation(format!(
                "page {page_num} is out of bounds"
            )))
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

    /// Add a page to the reserved set.
    ///
    /// Reserved pages cannot be freed. This is used when allocating new
    /// bitmap pages that must remain protected.
    pub(crate) async fn add_reserved(&self, page_num: u64) {
        self.reserved.write().await.insert(page_num);
    }

    /// Get the set of reserved page numbers.
    pub(crate) async fn reserved_pages(&self) -> Vec<u64> {
        let reserved = self.reserved.read().await;
        let mut pages: Vec<_> = reserved.iter().copied().collect();
        pages.sort_unstable();
        pages
    }

    /// Check if a page is reserved (cannot be freed).
    pub(crate) async fn is_reserved(&self, page_num: u64) -> bool {
        self.reserved.read().await.contains(&page_num)
    }

    /// Extend the bitmap capacity to hold at least `min_pages` pages.
    ///
    /// If current capacity already exceeds `min_pages`, this is a no-op.
    /// Capacity is rounded up to the nearest 64-page boundary.
    pub(crate) async fn extend_capacity(&self, min_pages: u64) {
        let mut state = self.state.write().await;
        state.bitmap.ensure_capacity(min_pages as usize);
    }
}

impl fmt::Debug for PageAllocator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PageAllocator")
            .field("max_pages", &self.max_pages)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, unused_must_use)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allocator_new_reserves_page_zero() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        assert!(alloc.is_allocated(PageId::HEADER).await);
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_allocate_sequential() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        assert!(alloc.is_allocated(PageId::HEADER).await);
        assert!(!alloc.is_allocated(PageId::from_page_num(1).unwrap()).await);

        let p = alloc.allocate().await.unwrap();
        assert!(alloc.is_allocated(p).await);

        alloc.free(p).await.unwrap();
        assert!(!alloc.is_allocated(p).await);
    }

    #[tokio::test]
    async fn allocator_mark_allocated() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Mark a page beyond current capacity
        let id = PageId::from_page_num(100).unwrap();
        assert_eq!(
            alloc.mark_allocated(id).await.unwrap(),
            MarkAllocatedResult::NewlyAllocated
        );
        assert!(alloc.is_allocated(id).await);
        assert!(alloc.capacity().await >= 101);
    }

    #[tokio::test]
    async fn allocator_serialization_roundtrip() {
        let alloc = PageAllocator::with_reserved(128, None, &[0]);

        // Allocate some pages
        let p1 = alloc.allocate().await.unwrap();
        let p2 = alloc.allocate().await.unwrap();
        let _p3 = alloc.allocate().await.unwrap();
        alloc.free(p2).await.unwrap();

        let bytes = alloc.to_bytes().await;
        let restored = PageAllocator::from_bytes(&bytes, None, &[0]).unwrap();

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(128, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Try to free a page that was never allocated
        let id = PageId::from_page_num(10).unwrap();
        let err = alloc.free(id).await.unwrap_err();

        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_free_double_free() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        let p = alloc.allocate().await.unwrap();
        alloc.free(p).await.unwrap();

        // Try to free again
        let err = alloc.free(p).await.unwrap_err();

        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_free_out_of_bounds() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Try to free a page beyond the bitmap capacity
        let id = PageId::from_page_num(1000).unwrap();
        let err = alloc.free(id).await.unwrap_err();

        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_limit_exceeded() {
        // Limit of 5 pages total (including reserved page 0)
        let alloc = PageAllocator::with_reserved(64, Some(5), &[0]);

        // Can allocate 4 more pages (page 0 is already allocated)
        alloc.allocate().await.unwrap(); // page 1
        alloc.allocate().await.unwrap(); // page 2
        alloc.allocate().await.unwrap(); // page 3
        alloc.allocate().await.unwrap(); // page 4

        assert_eq!(alloc.allocated_count().await, 5);

        // Next allocation should fail
        let err = alloc.allocate().await.unwrap_err();
        assert!(matches!(err, StorageError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn allocator_limit_with_free_reuse() {
        let alloc = PageAllocator::with_reserved(64, Some(5), &[0]);

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
            StorageError::MemoryLimitExceeded { .. }
        ));
    }

    #[tokio::test]
    async fn allocator_from_bytes_empty_bitmap() {
        let empty: &[u8] = &[];
        let err = PageAllocator::from_bytes(empty, None, &[0]).unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_cannot_free_page_zero() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);
        let page_zero = PageId::from_page_num(0).unwrap();
        let err = alloc.free(page_zero).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_limit_one_only_header() {
        // Limit of 1 means only page 0 (header) can exist
        let alloc = PageAllocator::with_reserved(64, Some(1), &[0]);

        // Already at limit (page 0 is reserved)
        assert_eq!(alloc.allocated_count().await, 1);

        // Cannot allocate any more
        let err = alloc.allocate().await.unwrap_err();
        assert!(matches!(err, StorageError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn allocator_reuses_from_beginning_after_free_all() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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

        let alloc = PageAllocator::from_bytes(&bytes, None, &[0]).unwrap();

        // Should only have 64 pages (1 word), partial bytes ignored
        assert_eq!(alloc.capacity().await, 64);
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_from_bytes_exact_multiple() {
        // 16 bytes = exactly 2 words
        let mut bytes = vec![0u8; 16];
        bytes[0] = 0b11; // Pages 0 and 1 allocated

        let alloc = PageAllocator::from_bytes(&bytes, None, &[0]).unwrap();

        assert_eq!(alloc.capacity().await, 128);
        assert_eq!(alloc.allocated_count().await, 2);
        assert!(alloc.is_allocated(PageId::from_page_num(0).unwrap()).await);
        assert!(alloc.is_allocated(PageId::from_page_num(1).unwrap()).await);
        assert!(!alloc.is_allocated(PageId::from_page_num(2).unwrap()).await);
    }

    #[tokio::test]
    async fn allocator_mark_allocated_page_zero() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Page 0 is already allocated
        assert_eq!(
            alloc.mark_allocated(PageId::HEADER).await.unwrap(),
            MarkAllocatedResult::AlreadyAllocated
        );

        // Count should not change
        assert_eq!(alloc.allocated_count().await, 1);
    }

    #[tokio::test]
    async fn allocator_with_reserved_pages() {
        // Create allocator with pages 0, 5, 10 reserved
        let alloc = PageAllocator::with_reserved(64, None, &[0, 5, 10]);

        // All reserved pages should be allocated
        assert!(alloc.is_allocated(PageId::from_page_num(0).unwrap()).await);
        assert!(alloc.is_allocated(PageId::from_page_num(5).unwrap()).await);
        assert!(alloc.is_allocated(PageId::from_page_num(10).unwrap()).await);
        assert_eq!(alloc.allocated_count().await, 3);

        // First allocation should skip reserved pages
        let p1 = alloc.allocate().await.unwrap();
        assert_eq!(p1.page_num(), 1);

        let p2 = alloc.allocate().await.unwrap();
        assert_eq!(p2.page_num(), 2);
    }

    #[tokio::test]
    async fn allocator_cannot_free_reserved_page() {
        let alloc = PageAllocator::with_reserved(64, None, &[0, 5, 10]);

        // Cannot free any reserved page
        let err = alloc
            .free(PageId::from_page_num(5).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));

        let err = alloc
            .free(PageId::from_page_num(10).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));

        // Page 0 returns specific error
        let err = alloc
            .free(PageId::from_page_num(0).unwrap())
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_add_reserved() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Allocate a page
        let p = alloc.allocate().await.unwrap();
        assert!(alloc.is_allocated(p).await);

        // Add it to reserved set
        alloc.add_reserved(p.page_num()).await;

        // Now it cannot be freed
        let err = alloc.free(p).await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidOperation(_)));
    }

    #[tokio::test]
    async fn allocator_reserved_pages() {
        let alloc = PageAllocator::with_reserved(64, None, &[0, 10, 5]);

        let reserved = alloc.reserved_pages().await;
        assert_eq!(reserved, vec![0, 5, 10]); // sorted
    }

    #[tokio::test]
    async fn allocator_is_reserved() {
        let alloc = PageAllocator::with_reserved(64, None, &[0, 5]);

        assert!(alloc.is_reserved(0).await);
        assert!(alloc.is_reserved(5).await);
        assert!(!alloc.is_reserved(1).await);
        assert!(!alloc.is_reserved(10).await);
    }

    #[tokio::test]
    async fn allocator_extend_capacity() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        assert_eq!(alloc.capacity().await, 64);

        // Extend to 200 pages (rounds up to 256)
        alloc.extend_capacity(200).await;
        assert!(alloc.capacity().await >= 200);

        // Extending to smaller does nothing
        alloc.extend_capacity(50).await;
        assert!(alloc.capacity().await >= 200);
    }

    #[tokio::test]
    async fn allocator_is_allocated_beyond_bounds() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

        // Page way beyond capacity should return false, not panic
        assert!(
            !alloc
                .is_allocated(PageId::from_page_num(10000).unwrap())
                .await
        );
    }

    #[tokio::test]
    async fn allocator_counts_after_free_all() {
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
        let alloc = PageAllocator::with_reserved(64, None, &[0]);

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
