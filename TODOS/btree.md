# Phase 2.1: B-Tree Structure - Initial Setup (Enhanced)

## Summary of Enhancements

This enhanced plan incorporates the following improvements over the original design:

1. **Error Handling**: Added `StorageError` enum from the start with `Result<T>` returns
2. **Resource Management**: Added memory limits and statistics tracking via `BTreeStats`
3. **NodeId Allocation**: Abstracted behind `NodeAllocator` trait for future extensibility
4. **Thread Safety**: Explicit `Arc<BTree>` pattern documented with concurrent access tests
5. **B-tree Clarification**: Clearly documented as B-tree (not B+-tree) variant
6. **Transaction Readiness**: Optional transaction context parameters considered
7. **Performance Characteristics**: Added memory usage and growth pattern documentation
8. **Comprehensive Testing**: Added concurrent reader/writer tests from day one

## Overview
Implement the foundational `BTree` struct for the in-memory B-tree storage system. This is Phase 2.1's first checkbox: defining the structure and constructor only (operations like `find_node`, `split_node`, `merge_nodes` will come later).

**Key Decision**: We're starting with **async APIs from day one** to avoid massive refactoring when we add disk persistence in Phase 4. This means using `tokio::sync::RwLock` and `async fn` even for pure in-memory operations.

## Core Data Structure Design

### B-tree Variant Clarification

**Important**: This implementation is a **B-tree** (not strictly a B+-tree), where:
- Both internal nodes and leaves can store values
- Keys are complete paths (e.g., `[123, "NAME"]`), not single subscripts
- This aligns with the `Node` struct which has `values: Vec<NodeData>` at all levels
- The distinction from a trie: We store complete paths as single entries for efficient disk I/O

This design allows for optimal disk access patterns where a single page read can resolve multi-level paths.

### Collection Choice Rationale

The `BTree` struct uses two different collection types for different purposes:

#### 1. `roots: BTreeMap<Name, NodeId>` - Variable Name Registry

**Why BTreeMap?**
- **Ordering semantics**: RUMPS requires ordered iteration over variable names
  - In MUMPS, `$ORDER` can iterate over global/local names themselves (not just their subscripts)
  - Example: `$ORDER(^"PATIENT")` might return `^"RECORDS"` as the next global name
- **Consistency**: Both `Name::Global` and `Name::Local` should support the same ordered traversal semantics
- **Name implements Ord**: The `Name` enum has well-defined ordering (Global < Local, then lexicographic)
- **Small size**: We won't have millions of variables, so O(log n) vs O(1) lookup is negligible
- **Thematic consistency**: Using a B-tree map for the top-level registry aligns with the B-tree storage model

#### 2. `nodes: RwLock<HashMap<NodeId, Node>>` - Node Storage Pool

**Why HashMap?**
- **NodeId is arbitrary**: NodeIds are internal references (u64), similar to memory addresses or disk page offsets
  - They have no semantic meaning beyond identifying a node
  - The ordering of NodeIds does not reflect any logical ordering in the database
- **Logical ordering lives in the tree**: The B-tree's ordering is maintained by:
  - Parent-child relationships (via `children: Vec<NodeId>`)
  - Sorted keys within each node (`keys: Vec<Key>`)
  - The tree structure itself, not the physical organization of nodes
- **Access pattern**: We only perform direct lookups by NodeId, never arbitrary iteration
- **Performance**: O(1) lookup is beneficial for node resolution

**Why RwLock?**
- **Async-aware**: `tokio::sync::RwLock` allows concurrent readers, exclusive writers, and works with async/await
- **Future-proof**: When we add disk I/O in Phase 4, we'll need async locking anyway
- **Interior mutability**: Allows cache updates from async methods without `&mut self`
- **No breaking changes**: Starting async now means no API refactoring in Phase 4

**Key Insight**: Ordering in a B-tree is maintained by the tree structure (parent-child links + sorted keys within nodes), not by how nodes are physically organized in memory or on disk.

### On-Disk vs In-Memory Semantics (Phase 4 Preview)

When loading globals from disk (Phase 4):
1. Look up root `NodeId` in `roots: BTreeMap<Name, NodeId>`
2. Check cache: `nodes.read().await.get(&node_id)`
3. If miss: load from disk (async I/O), insert into cache
4. Lazy-load child nodes on demand as tree is traversed
5. Both persistent (Global) and ephemeral (Local) nodes coexist in the same `nodes` cache

The API remains the same—just add an optional storage engine field in Phase 4.

## Files to Create

### 1. `crates/rumps-storage/src/error.rs` (New File - Add First)

Define error types for the storage layer:

```rust
use thiserror::Error;
use rumps_types::NodeId;

/// Errors that can occur during B-tree operations.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// Node not found in storage
    #[error("Node {0:?} not found")]
    NodeNotFound(NodeId),

    /// Key not found in tree
    #[error("Key not found: {0}")]
    KeyNotFound(String),

    /// Node overflow (too many keys)
    #[error("Node overflow: {current} keys, max {max}")]
    NodeOverflow { current: usize, max: usize },

    /// Memory limit exceeded
    #[error("Memory limit exceeded: {used} bytes, limit {limit}")]
    MemoryLimitExceeded { used: usize, limit: usize },

    /// I/O error (for future disk operations)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Transaction error (for future)
    #[error("Transaction error: {0}")]
    Transaction(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;
```

### 2. `crates/rumps-storage/src/btree.rs`

Define the `BTree` struct with error handling and resource management:

```rust
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;
use rumps_types::{Name, Node, NodeId};
use crate::error::{StorageError, Result};
use crate::allocator::NodeAllocator;
use crate::stats::BTreeStats;

/// Statistics tracking for B-tree operations.
#[derive(Debug, Clone, Default)]
pub struct BTreeStats {
    /// Current height of the tree
    pub height: usize,
    /// Total number of nodes
    pub node_count: usize,
    /// Total number of keys across all nodes
    pub key_count: usize,
    /// Average fill factor (keys per node / max keys per node)
    pub avg_fill_factor: f64,
    /// Memory usage in bytes (estimated)
    pub memory_bytes: usize,
    /// Number of splits performed
    pub splits: u64,
    /// Number of merges performed
    pub merges: u64,
}

/// Trait for node ID allocation strategies.
pub trait NodeAllocator: Send + Sync {
    /// Allocate a new node ID
    async fn allocate(&self) -> Result<NodeId>;

    /// Deallocate a node ID for reuse
    async fn deallocate(&self, id: NodeId) -> Result<()>;

    /// Get the next available ID without allocating
    async fn peek_next(&self) -> NodeId;
}

/// Simple incrementing allocator for in-memory use.
pub struct IncrementingAllocator {
    next_id: RwLock<u64>,
}

impl IncrementingAllocator {
    pub fn new() -> Self {
        Self { next_id: RwLock::new(0) }
    }
}

impl NodeAllocator for IncrementingAllocator {
    async fn allocate(&self) -> Result<NodeId> {
        let mut next = self.next_id.write().await;
        let id = NodeId(*next);
        *next += 1;
        Ok(id)
    }

    async fn deallocate(&self, _id: NodeId) -> Result<()> {
        // No-op for simple allocator
        Ok(())
    }

    async fn peek_next(&self) -> NodeId {
        NodeId(*self.next_id.read().await)
    }
}

/// In-memory B-tree storage for RUMPS database.
///
/// This structure manages the hierarchical storage of both persistent globals
/// (`^NAME`) and ephemeral locals (`NAME`). All operations are async to support
/// future disk persistence without API changes.
///
/// # B-tree Properties
///
/// For a B-tree of minimum degree `t` (where `min_degree = t`):
/// - Each node (except root) contains `t-1` to `2t-1` keys
/// - Each internal node (except root) has `t` to `2t` children
/// - The root has 1 to `2t-1` keys (can be smaller)
/// - All leaves are at the same depth
/// - Keys within a node are sorted in ascending order
///
/// # Design Decisions
///
/// ## BTreeMap for roots
/// Variable names are stored in a `BTreeMap` to support ordered iteration,
/// enabling MUMPS `$ORDER` semantics over variable names themselves.
///
/// ## RwLock<HashMap> for nodes
/// Nodes are stored in an async-aware `RwLock<HashMap>` because:
/// - NodeIds are arbitrary internal references (like page IDs)
/// - Logical ordering is maintained by the tree structure, not NodeId values
/// - RwLock enables concurrent reads with exclusive writes
/// - Async from day one prevents breaking API changes when adding disk I/O
///
/// In Phase 2-3, this is pure in-memory storage. In Phase 4, it becomes
/// a page cache with lazy loading from disk.
///
/// # Thread Safety
///
/// The BTree is designed to be shared across threads using `Arc<BTree>`.
/// All operations use interior mutability via `RwLock`, allowing multiple
/// concurrent readers with exclusive writers.
///
/// # Examples
///
/// ```
/// use rumps_storage::BTree;
/// use std::sync::Arc;
///
/// # tokio_test::block_on(async {
/// // Create a B-tree with minimum degree 3
/// // (nodes will have 2-5 keys)
/// let btree = Arc::new(BTree::new(3)?);
/// assert_eq!(btree.min_degree(), 3);
/// assert_eq!(btree.node_count().await, 0);
///
/// // Use default configuration (min_degree = 3)
/// let btree = Arc::new(BTree::default());
/// assert_eq!(btree.min_degree(), 3);
/// # });
/// ```
pub struct BTree {
    /// Maps variable names to root nodes (maintains sorted order).
    ///
    /// This BTreeMap enables ordered iteration over variable names,
    /// supporting MUMPS `$ORDER` semantics. Both Global and Local
    /// variables are stored in the same map with consistent ordering.
    roots: RwLock<BTreeMap<Name, NodeId>>,

    /// Async-aware node storage pool.
    ///
    /// In Phase 2-3, this holds all nodes in memory. In Phase 4, it becomes
    /// a page cache with LRU eviction, where nodes are lazy-loaded from disk.
    ///
    /// Uses `tokio::sync::RwLock` for async-compatible concurrent access:
    /// - Multiple readers can access simultaneously
    /// - Writers get exclusive access
    /// - Works seamlessly with async/await
    ///
    /// NodeIds have no semantic ordering—they're internal references.
    /// The tree's logical ordering is maintained by parent-child links
    /// and sorted keys within each node.
    nodes: RwLock<HashMap<NodeId, Node>>,

    /// Node ID allocator strategy.
    allocator: Arc<dyn NodeAllocator>,

    /// Minimum degree `t` of the B-tree.
    ///
    /// Nodes (except root) contain `t-1` to `2t-1` keys.
    /// Internal nodes (except root) have `t` to `2t` children.
    min_degree: usize,

    /// Optional memory limit in bytes.
    ///
    /// When set, operations will fail if memory usage exceeds this limit.
    /// This prevents unbounded growth in memory-constrained environments.
    max_memory_bytes: Option<usize>,

    /// Statistics tracking for monitoring and debugging.
    stats: RwLock<BTreeStats>,
}
```

**Methods to implement:**

```rust
impl BTree {
    /// Creates a new empty B-tree with the specified minimum degree.
    ///
    /// # Errors
    ///
    /// Returns an error if `min_degree < 2` (invalid B-tree configuration).
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    /// use std::sync::Arc;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = Arc::new(BTree::new(3)?);
    /// assert_eq!(btree.min_degree(), 3);
    /// assert_eq!(btree.node_count().await, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub fn new(min_degree: usize) -> Result<Self> {
        if min_degree < 2 {
            return Err(StorageError::InvalidConfiguration(
                "min_degree must be >= 2".to_string()
            ));
        }

        Ok(Self {
            roots: RwLock::new(BTreeMap::new()),
            nodes: RwLock::new(HashMap::new()),
            allocator: Arc::new(IncrementingAllocator::new()),
            min_degree,
            max_memory_bytes: None,
            stats: RwLock::new(BTreeStats::default()),
        })
    }

    /// Creates a new B-tree with custom configuration.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    /// use std::sync::Arc;
    ///
    /// # tokio_test::block_on(async {
    /// // Create with 10MB memory limit
    /// let btree = Arc::new(BTree::with_config(3, Some(10_000_000))?);
    /// assert_eq!(btree.min_degree(), 3);
    /// assert!(btree.has_memory_limit());
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub fn with_config(min_degree: usize, max_memory_bytes: Option<usize>) -> Result<Self> {
        let mut btree = Self::new(min_degree)?;
        btree.max_memory_bytes = max_memory_bytes;
        Ok(btree)
    }

    /// Returns the minimum degree of this B-tree.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// let btree = BTree::new(4);
    /// assert_eq!(btree.min_degree(), 4);
    /// ```
    pub fn min_degree(&self) -> usize {
        self.min_degree
    }

    /// Returns the total number of nodes in this B-tree.
    ///
    /// This is an async method because it requires acquiring a read lock
    /// on the nodes collection.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3);
    /// assert_eq!(btree.node_count().await, 0);
    /// # });
    /// ```
    pub async fn node_count(&self) -> usize {
        self.nodes.read().await.len()
    }

    /// Returns whether a memory limit is configured.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// assert!(!btree.has_memory_limit());
    ///
    /// let btree = BTree::with_config(3, Some(1000000))?;
    /// assert!(btree.has_memory_limit());
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub fn has_memory_limit(&self) -> bool {
        self.max_memory_bytes.is_some()
    }

    /// Returns the current statistics for this B-tree.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::new(3)?;
    /// let stats = btree.stats().await;
    /// assert_eq!(stats.node_count, 0);
    /// assert_eq!(stats.key_count, 0);
    /// # Ok::<(), rumps_storage::StorageError>(())
    /// # });
    /// ```
    pub async fn stats(&self) -> BTreeStats {
        self.stats.read().await.clone()
    }

    /// Checks if the current memory usage is within limits.
    async fn check_memory_limit(&self) -> Result<()> {
        if let Some(limit) = self.max_memory_bytes {
            let stats = self.stats.read().await;
            if stats.memory_bytes > limit {
                return Err(StorageError::MemoryLimitExceeded {
                    used: stats.memory_bytes,
                    limit,
                });
            }
        }
        Ok(())
    }
}

impl Default for BTree {
    /// Creates a B-tree with default minimum degree of 3.
    ///
    /// This provides a good balance between tree height and node utilization:
    /// - Nodes contain 2-5 keys
    /// - Internal nodes have 3-6 children
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_storage::BTree;
    ///
    /// # tokio_test::block_on(async {
    /// let btree = BTree::default();
    /// assert_eq!(btree.min_degree(), 3);
    /// assert_eq!(btree.node_count().await, 0);
    /// # });
    /// ```
    fn default() -> Self {
        Self::new(3).expect("Default configuration is valid")
    }
}
```

**Unit tests to include:**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_btree_new() {
        let btree = BTree::new(3);
        assert_eq!(btree.min_degree(), 3);
    }

    #[test]
    fn test_btree_new_min_degree_2() {
        let btree = BTree::new(2);
        assert_eq!(btree.min_degree(), 2);
    }

    #[test]
    #[should_panic(expected = "min_degree must be >= 2")]
    fn test_btree_new_invalid_min_degree_zero() {
        BTree::new(0);
    }

    #[test]
    #[should_panic(expected = "min_degree must be >= 2")]
    fn test_btree_new_invalid_min_degree_one() {
        BTree::new(1);
    }

    #[test]
    fn test_btree_default() {
        let btree = BTree::default();
        assert_eq!(btree.min_degree(), 3);
    }

    #[tokio::test]
    async fn test_btree_initial_state() {
        let btree = BTree::new(4);
        assert!(btree.roots.is_empty());
        assert_eq!(btree.node_count().await, 0);
        assert_eq!(*btree.next_node_id.read().await, 0);
    }

    #[tokio::test]
    async fn test_btree_node_count() {
        let btree = BTree::new(3).unwrap();
        assert_eq!(btree.node_count().await, 0);

        // Will add nodes in future phases
    }

    #[tokio::test]
    async fn test_btree_with_memory_limit() {
        let btree = BTree::with_config(3, Some(1_000_000)).unwrap();
        assert!(btree.has_memory_limit());
        assert_eq!(btree.min_degree(), 3);
    }

    #[tokio::test]
    async fn test_btree_stats() {
        let btree = BTree::new(3).unwrap();
        let stats = btree.stats().await;
        assert_eq!(stats.node_count, 0);
        assert_eq!(stats.key_count, 0);
        assert_eq!(stats.height, 0);
        assert_eq!(stats.splits, 0);
        assert_eq!(stats.merges, 0);
    }

    #[tokio::test]
    async fn test_concurrent_readers() {
        use std::sync::Arc;
        use tokio::task;

        let btree = Arc::new(BTree::new(3).unwrap());
        let mut handles = vec![];

        // Spawn 10 concurrent reader tasks
        for _ in 0..10 {
            let btree_clone = Arc::clone(&btree);
            let handle = task::spawn(async move {
                for _ in 0..100 {
                    let _count = btree_clone.node_count().await;
                    let _stats = btree_clone.stats().await;
                }
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        for handle in handles {
            handle.await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_writer_blocks_readers() {
        use std::sync::Arc;
        use tokio::time::{sleep, Duration};

        let btree = Arc::new(BTree::new(3).unwrap());

        // Acquire write lock and hold it
        let write_guard = btree.nodes.write().await;

        // Try to read concurrently (should block)
        let btree_clone = Arc::clone(&btree);
        let read_task = tokio::spawn(async move {
            let start = tokio::time::Instant::now();
            let _count = btree_clone.node_count().await;
            start.elapsed()
        });

        // Hold the write lock for a moment
        sleep(Duration::from_millis(10)).await;

        // Release write lock
        drop(write_guard);

        // Read should now complete
        let elapsed = read_task.await.unwrap();
        assert!(elapsed >= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn test_stress_large_tree() {
        // This test is a placeholder for Phase 2.2+ when SET operations are implemented
        let btree = BTree::new(100).unwrap(); // Large min_degree
        assert_eq!(btree.min_degree(), 100);

        // Future: Insert millions of keys and verify tree properties
    }
}
```

### 3. `crates/rumps-storage/src/lib.rs`

Update to:

```rust
//! Persistent storage layer for RUMPS database.
//!
//! This crate implements the B-tree-backed persistent storage system
//! for MUMPS-style globals with disk persistence.
//!
//! # Async-First Design
//!
//! All APIs are async from the start to support future disk I/O without
//! breaking changes. Even pure in-memory operations use async primitives
//! (e.g., `tokio::sync::RwLock`) for consistency.
//!
//! # Thread Safety
//!
//! All structures are designed to be used with `Arc` for thread-safe sharing.
//! Operations use interior mutability via `RwLock` for concurrent access.

#![warn(missing_docs)]
#![deny(use_self)]

mod btree;
mod error;

pub use btree::BTree;
pub use error::{StorageError, Result};
```

### 4. Update `crates/rumps-storage/Cargo.toml`

Add tokio dependency:

```toml
[dependencies]
rumps-types = { path = "../rumps-types" }
bincode = "1.3"
serde = { workspace = true }
thiserror = { workspace = true }
tokio = { workspace = true }

[dev-dependencies]
tokio-test = "0.4"
```

## Implementation Checklist

- [ ] Create `crates/rumps-storage/src/error.rs` (NEW)
  - [ ] Define `StorageError` enum with all error variants
  - [ ] Implement `thiserror::Error` trait
  - [ ] Define `Result<T>` type alias
  - [ ] Include error variants for future phases (Transaction, IO, etc.)
- [ ] Create `crates/rumps-storage/src/btree.rs`
  - [ ] Define `BTreeStats` struct for tracking metrics
  - [ ] Define `NodeAllocator` trait for abstracted ID allocation
  - [ ] Implement `IncrementingAllocator` for in-memory use
  - [ ] Define `BTree` struct with comprehensive rustdoc
    - [ ] Include resource management fields (`max_memory_bytes`, `stats`)
    - [ ] Use `Arc<dyn NodeAllocator>` for flexible allocation
    - [ ] Make `roots` field use `RwLock` for consistency
  - [ ] Document BTreeMap choice for `roots`
  - [ ] Document RwLock<HashMap> choice for `nodes`
  - [ ] Explain async-first design decision
  - [ ] Document thread safety model (Arc sharing)
  - [ ] Clarify B-tree variant (not B+-tree)
  - [ ] Implement `BTree::new(min_degree)` returning `Result<Self>`
  - [ ] Implement `BTree::with_config()` for custom configuration
  - [ ] Implement `Default` trait (min_degree = 3)
  - [ ] Implement accessor methods:
    - [ ] `min_degree()`
    - [ ] `node_count()` (async)
    - [ ] `has_memory_limit()`
    - [ ] `stats()` (async)
    - [ ] `check_memory_limit()` (async, private)
- [ ] Write comprehensive unit tests
  - [ ] Valid constructor cases
  - [ ] Invalid min_degree (error handling)
  - [ ] Default implementation
  - [ ] Initial state verification (async test)
  - [ ] Node count verification (async test)
  - [ ] Memory limit configuration test
  - [ ] Stats initialization test
  - [ ] Concurrent readers test
  - [ ] Writer blocks readers test
  - [ ] Stress test placeholder (for future phases)
- [ ] Update `crates/rumps-storage/src/lib.rs`
  - [ ] Add module declarations (`btree`, `error`)
  - [ ] Re-export `BTree`, `StorageError`, `Result`
  - [ ] Document async-first design
  - [ ] Document thread safety model
  - [ ] Add `#![deny(use_self)]` lint
- [ ] Update `crates/rumps-storage/Cargo.toml`
  - [ ] Add `tokio` workspace dependency
  - [ ] Add `tokio-test` dev dependency
  - [ ] Verify `thiserror` is included
- [ ] Run `cargo test --package rumps-storage`
- [ ] Run `cargo clippy --package rumps-storage`
- [ ] Run `cargo doc --package rumps-storage --no-deps --open`

## Key Constraints

1. **Async-First**: All public APIs use `async fn` even for in-memory operations
   - Use `tokio::sync::RwLock` instead of `std::sync::RwLock` or `RefCell`
   - Tests use `#[tokio::test]` for async tests
   - Examples use `tokio_test::block_on` for doctests

2. **Functional Style**: Follow project coding standards
   - Use iterator methods over explicit loops
   - Prefer exhaustive pattern matching
   - Avoid early returns when possible

3. **Minimum Degree Validation**: Must be >= 2 for valid B-tree properties

4. **Clippy Lints**: Respect `use_self = "deny"` (use `Self` where appropriate)

5. **Documentation**: All public items must have rustdoc comments with examples

## Memory and Performance Characteristics

### Memory Usage
- **Node Size**: Each node contains `2t-1` keys maximum
  - For `t=3`: 5 keys + 6 child pointers max per node
  - For `t=100`: 199 keys + 200 child pointers max per node
- **Key Storage**: Each key is a `Vec<Subscript>` with variable size
- **Value Storage**: Compact binary encoding (1-10 bytes for simple values)
- **Overhead**: HashMap entry (~48 bytes), RwLock wrapper (~24 bytes)
- **Estimated Memory**: Can be tracked via `BTreeStats::memory_bytes`

### Performance Expectations
- **Lookup**: O(log n) tree traversal + O(1) HashMap access per node
- **Insert/Delete**: O(log n) with potential node splits/merges
- **Concurrent Reads**: Lock-free with RwLock (multiple readers)
- **Write Contention**: Exclusive lock required, serialized access
- **Cache Behavior**: HashMap provides good cache locality for hot nodes

### Growth Patterns
- Tree grows upward (new root) when root splits
- Nodes split when full (`2t-1` keys)
- Nodes merge when underfull (`< t-1` keys)
- Memory usage grows linearly with key count
- Height grows logarithmically with key count

## Notes for Future Phases

### Phase 2.2-2.6: Tree Operations
Will add async tree operations (SET, GET, KILL, DATA, ORDER) to the `BTree` struct.

Consider adding optional transaction context parameter from the start:
```rust
pub async fn set(&self,
    name: &Name,
    key: &Key,
    value: Value,
    txn: Option<&TransactionContext>
) -> Result<()> { ... }
```
This would make Phase 5 integration smoother without breaking changes.

### Phase 4: Disk Persistence
Will add disk storage with minimal changes:
- Add `storage: Option<AsyncStorageEngine>` field
- `nodes` semantics shift from "complete storage" to "page cache"
- Add `with_storage(min_degree, path)` constructor
- Internal methods check cache first, then load from disk if needed
- **API remains unchanged** thanks to async-first design
- NodeAllocator trait enables free-list implementation for reusing deleted pages

### Phase 5: Transactions
Will add ACID transactions with WAL (Write-Ahead Logging):
- Transaction context passed through operations
- Snapshot isolation for reads
- Write-ahead logging for durability
- The `Arc<BTree>` pattern enables shared ownership across transactions
