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

## Completed Implementation (Phase 2.1)

The following have been implemented in `crates/rumps-storage/`:

### 1. `src/error.rs` ✅
Defines `StorageError` enum with all error variants and `Result<T>` type alias. Includes error types for configuration, node operations, memory limits, I/O, serialization, and future transaction support.

### 2. `src/btree.rs` ✅
Complete B-tree structure implementation including:
- `BTreeStats` struct for metrics tracking
- `NodeAllocator` trait with `#[async_trait]` for flexible ID allocation
- `IncrementingAllocator` for in-memory use
- `BTree` struct with all fields (roots, nodes, allocator, configuration, stats)
- Constructor methods: `new()`, `with_config()`, `Default` impl
- Accessor methods: `min_degree()`, `node_count()`, `has_memory_limit()`, `stats()`, `check_memory_limit()`
- Comprehensive rustdoc with examples
- 12 unit tests including concurrent access tests

### 3. `src/lib.rs` ✅
Module declarations and public exports with async-first design documentation.

### 4. `Cargo.toml` ✅
All dependencies configured: `async-trait`, `tokio`, `bincode`, `serde`, `thiserror`, plus dev dependencies `futures` and `tokio-test`.

## Implementation Checklist

- [x] Create `crates/rumps-storage/src/error.rs` (NEW)
  - [x] Define `StorageError` enum with all error variants
  - [x] Implement `thiserror::Error` trait
  - [x] Define `Result<T>` type alias
  - [x] Include error variants for future phases (Transaction, IO, etc.)
- [x] Create `crates/rumps-storage/src/btree.rs`
  - [x] Define `BTreeStats` struct for tracking metrics
  - [x] Define `NodeAllocator` trait for abstracted ID allocation (with `#[async_trait]`)
  - [x] Implement `IncrementingAllocator` for in-memory use
  - [x] Define `BTree` struct with comprehensive rustdoc
    - [x] Include resource management fields (`max_memory_bytes`, `stats`)
    - [x] Use `Arc<dyn NodeAllocator>` for flexible allocation
    - [x] Make `roots` field use `RwLock` for consistency
  - [x] Document BTreeMap choice for `roots`
  - [x] Document RwLock<HashMap> choice for `nodes`
  - [x] Explain async-first design decision
  - [x] Document thread safety model (Arc sharing)
  - [x] Clarify B-tree variant (not B+-tree)
  - [x] Implement `BTree::new(min_degree)` returning `Result<Self>`
  - [x] Implement `BTree::with_config()` for custom configuration
  - [x] Implement `Default` trait (min_degree = 3)
  - [x] Implement accessor methods:
    - [x] `min_degree()`
    - [x] `node_count()` (async)
    - [x] `has_memory_limit()`
    - [x] `stats()` (async)
    - [x] `check_memory_limit()` (async, private)
- [x] Write comprehensive unit tests
  - [x] Valid constructor cases
  - [x] Invalid min_degree (error handling)
  - [x] Default implementation
  - [x] Initial state verification (async test)
  - [x] Node count verification (async test)
  - [x] Memory limit configuration test
  - [x] Stats initialization test
  - [x] Concurrent readers test
  - [x] Writer blocks readers test
  - [x] Stress test placeholder (for future phases)
- [x] Update `crates/rumps-storage/src/lib.rs`
  - [x] Add module declarations (`btree`, `error`)
  - [x] Re-export `BTree`, `StorageError`, `Result`
  - [x] Document async-first design
  - [x] Document thread safety model
  - [x] Add `#![deny(clippy::use_self)]` lint
- [x] Update `crates/rumps-storage/Cargo.toml`
  - [x] Add `tokio` workspace dependency
  - [x] Add `async-trait` dependency
  - [x] Add `tokio-test` dev dependency
  - [x] Add `futures` dev dependency
  - [x] Verify `thiserror` is included
- [x] Run `cargo test --package rumps-storage` (12 tests passing)
- [x] Run `cargo clippy --package rumps-storage` (clean except expected dead code warnings)
- [x] Run `cargo doc --package rumps-storage --no-deps --open` (documentation complete)

## Key Constraints

1. **Async-First**: All public APIs use `async fn` even for in-memory operations
   - Use `tokio::sync::RwLock` instead of `std::sync::RwLock` or `RefCell`
   - Tests use `#[tokio::test]` for async tests
   - Examples use `tokio_test::block_on` for doctests

2. **Functional Style**: Follow project coding standards
   - Use iterator methods over explicit loops
   - Prefer exhaustive pattern matching
   - Avoid early returns

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

Each operation has two methods, with the simple version delegating to the context version:
- **Context method**: Full implementation with optional transaction context (e.g., `get_with_context()`, `set_with_context()`)
- **Simple method**: Convenience wrapper that calls context method with `None` (e.g., `get()`, `set()`)

This design avoids code duplication while keeping the common case simple:
```rust
// Full implementation with optional transaction context
pub async fn get_with_context(&self, name: &Name, key: &Key, context: Option<&TransactionContext>) -> Result<Option<Value>> {
    // Full implementation here
    // If context is Some, use transaction snapshot isolation
}

pub async fn set_with_context(&self, name: &Name, key: &Key, value: Value, context: Option<&TransactionContext>) -> Result<()> {
    // Full implementation here
    // If context is Some, track writes in transaction
}

// Simple convenience wrappers (delegate to context versions with None)
pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
    self.get_with_context(name, key, None).await
}

pub async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()> {
    self.set_with_context(name, key, value, None).await
}
```

The public API in Phase 5 (`Database` and `Transaction` structs) will hide this distinction from users.

#### Value Cloning and Lock Semantics

**Why `get()` returns owned `Value` instead of `&Value`:**

Read operations like `get()` return owned values (`Option<Value>`) rather than references because:

1. **Lock lifetime constraints**: With async operations and `RwLock`, we cannot return references that borrow from the lock guard:
   ```rust
   // This doesn't work - guard drops at end of function:
   async fn get(&self, ...) -> Result<Option<&Value>> {
       let guard = self.nodes.read().await;  // Lock acquired
       let value = &guard[...];  // Borrow from guard
       Ok(Some(value))  // ERROR: guard dropped, reference invalid
   }
   ```

2. **Lock contention**: Returning references would require keeping locks held while the caller processes the value, significantly reducing concurrency. Cloning allows locks to be released immediately after reading.

3. **Standard Rust patterns**: This follows the same design as `std::collections::HashMap::get()` which returns `Option<&V>` for synchronous access, but async concurrent structures typically clone values to avoid lock lifetime issues. Similar patterns exist in other async data structures like `dashmap::DashMap::get()` which also clones values.

**Implementation approach:**
```rust
async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
    let nodes = self.nodes.read().await;  // Acquire read lock
    let node = nodes.get(&node_id)?;
    let value = node.values[index].value.clone(); // Clone value
    // Lock guard drops here - other readers/writers can proceed
    Ok(value)
}
```

**Performance considerations:**
- MUMPS values are typically small (integers, short strings, booleans)
- Cloning cost is acceptable for the concurrency benefits
- If profiling shows cloning overhead is significant, future optimizations include:
  - Using `Arc<Value>` internally for copy-on-write semantics
  - Reference-counted sharing of large strings/JSON values
  - Specialized handling for large values (e.g., threshold-based `Arc` wrapping)
  - Zero-copy deserialization for disk-backed values

### Phase 4: Disk Persistence with AsyncStorageEngine

#### AsyncStorageEngine Design

The `AsyncStorageEngine` will be the abstraction layer between the B-tree and disk storage:

```rust
/// Trait for async disk storage operations
#[async_trait]
pub trait AsyncStorageEngine: Send + Sync {
    /// Read a node from disk by its ID
    async fn read_node(&self, id: NodeId) -> Result<Node>;

    /// Write a node to disk
    async fn write_node(&self, id: NodeId, node: &Node) -> Result<()>;

    /// Allocate a new page on disk
    async fn allocate_page(&self) -> Result<NodeId>;

    /// Deallocate a page for reuse
    async fn deallocate_page(&self, id: NodeId) -> Result<()>;

    /// Flush all pending writes to disk
    async fn flush(&self) -> Result<()>;

    /// Get metadata about storage
    async fn metadata(&self) -> StorageMetadata;
}

/// Metadata about the storage engine
#[derive(Debug, Clone)]
pub struct StorageMetadata {
    pub page_size: usize,
    pub total_pages: usize,
    pub free_pages: usize,
    pub dirty_pages: usize,
}

/// Concrete implementation using files
pub struct FileStorageEngine {
    /// Data file handle
    data_file: Arc<RwLock<tokio::fs::File>>,

    /// WAL for durability
    wal: Arc<WalWriter>,

    /// Page cache with LRU eviction
    cache: Arc<PageCache>,

    /// Free page management
    page_allocator: Arc<PageAllocator>,

    /// Configuration
    config: StorageConfig,
}

/// Storage configuration
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub page_size: usize,        // Default: wal::format::PAGE_SIZE
    pub cache_size: usize,       // Max pages in cache
    pub sync_mode: SyncMode,     // When to fsync
    pub compression: bool,       // Enable compression
}

/// When to sync data to disk
#[derive(Debug, Clone)]
pub enum SyncMode {
    /// Sync on every write (slow but safest)
    Immediate,
    /// Sync on transaction commit
    OnCommit,
    /// Sync periodically
    Periodic(Duration),
}
```

#### Updated BTree Structure

```rust
pub struct BTree {
    /// In-memory node cache (Phase 2-3: all nodes, Phase 4: LRU cache)
    nodes: RwLock<HashMap<NodeId, Node>>,

    /// Optional storage engine for persistence
    storage: Option<Arc<dyn AsyncStorageEngine>>,

    /// Node allocator (switches based on storage)
    allocator: Arc<dyn NodeAllocator>,

    /// Other fields remain the same...
    min_degree: usize,
    max_memory_bytes: Option<usize>,
    stats: RwLock<BTreeStats>,
}

impl BTree {
    /// Create disk-backed B-tree
    pub async fn with_storage(
        min_degree: usize,
        storage: Arc<dyn AsyncStorageEngine>
    ) -> Result<Self> {
        // Use DiskNodeAllocator that delegates to storage engine
        let allocator = Arc::new(DiskNodeAllocator::new(storage.clone()));

        Ok(Self {
            nodes: RwLock::new(HashMap::new()),
            storage: Some(storage),
            allocator,
            min_degree,
            max_memory_bytes: None,
            stats: RwLock::new(BTreeStats::default()),
        })
    }

    /// Load a node (from cache or disk)
    async fn load_node(&self, id: NodeId) -> Result<Node> {
        // Check cache first
        let nodes = self.nodes.read().await;

        if let Some(node) = nodes.get(&id) {
            Ok(node.clone())
        } else {
          drop(nodes);

          // Load from disk if storage is configured
          if let Some(storage) = &self.storage {
              let node = storage.read_node(id).await?;
  
              // Add to cache
              let mut nodes = self.nodes.write().await;
              nodes.insert(id, node.clone());
  
              // TODO: Implement LRU eviction if cache is full
  
              Ok(node)
          } else {
              Err(StorageError::NodeNotFound(id))
          }
        }

    }

    /// Save a node (to cache and optionally disk)
    async fn save_node(&self, id: NodeId, node: Node) -> Result<()> {
        // Update cache
        {
            let mut nodes = self.nodes.write().await;
            nodes.insert(id, node.clone());
        }

        // Write to disk if storage is configured
        if let Some(storage) = &self.storage {
            storage.write_node(id, &node).await?;
        }

        Ok(())
    }
}
```

#### Node Allocator for Disk

```rust
/// Allocator that uses the storage engine for page management
pub struct DiskNodeAllocator {
    storage: Arc<dyn AsyncStorageEngine>,
}

impl DiskNodeAllocator {
    pub fn new(storage: Arc<dyn AsyncStorageEngine>) -> Self {
        Self { storage }
    }
}

#[async_trait]
impl NodeAllocator for DiskNodeAllocator {
    async fn allocate(&self) -> Result<NodeId> {
        self.storage.allocate_page().await
    }

    async fn deallocate(&self, id: NodeId) -> Result<()> {
        self.storage.deallocate_page(id).await
    }

    async fn peek_next(&self) -> NodeId {
        // This might not be available for disk allocator
        NodeId(0) // Placeholder
    }
}
```

#### Integration Points

Will add disk storage with minimal changes:
- Add `storage: Option<Arc<dyn AsyncStorageEngine>>` field
- `nodes` semantics shift from "complete storage" to "page cache"
- Add `with_storage(min_degree, storage)` constructor
- Internal methods (`load_node`, `save_node`) check cache first, then load from disk if needed
- **API remains unchanged** thanks to async-first design
- NodeAllocator trait enables free-list implementation for reusing deleted pages
- All existing operations (SET, GET, KILL, etc.) work transparently with disk storage

### Phase 5: Transactions
Will add ACID transactions with WAL (Write-Ahead Logging):
- Transaction context passed through operations
- Snapshot isolation for reads
- Write-ahead logging for durability
- The `Arc<BTree>` pattern enables shared ownership across transactions
