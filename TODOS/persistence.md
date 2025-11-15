# RUMPS Persistent Storage Implementation Plan

This document tracks the implementation of Goal 1: Create a MUMPS-style binary tree storage system with persistent B-tree-backed globals.

## Core Principles

- **Unified Data Model**: In-memory and on-disk structures must be equivalent and synchronized
- **MUMPS Semantics**: Both globals (`^NAME`) and locals (`NAME`) are sparse multi-dimensional arrays with lexicographically ordered string keys
- **Two Namespaces**: Globals are persistent (written to disk), locals are ephemeral (memory-only)
- **Type Sharing**: Common types in `rumps-types` for use across storage and query layers
- **Idiomatic Rust**: Follow project lints and formatting rules

---

## Phase 1: Project Structure & Type System

### 1.1 Crate Setup
- [x] Create `crates/rumps-types/` directory structure
- [x] Create `crates/rumps-types/Cargo.toml` with workspace dependencies
- [x] Create `crates/rumps-types/src/lib.rs`
- [x] Create `crates/rumps-storage/` directory structure
- [x] Create `crates/rumps-storage/Cargo.toml` with dependencies (rumps-types, bincode, etc.)
- [x] Create `crates/rumps-storage/src/lib.rs`
- [x] Update workspace `Cargo.toml` to include new crates
- [x] Verify `cargo check` passes for new crates

### 1.2 Core Type Definitions (rumps-types)
- [ ] Define `Name` enum with `Global(String)` and `Local(String)` variants (e.g., `^PATIENT` vs `PATIENT`)
- [ ] Implement `Serialize`/`Deserialize` for `Name` using serde
- [ ] Add `Display` for `Name` (format with/without caret)
- [ ] Define `Subscript` type (string subscript in a key path)
- [ ] Define `Key` type (sequence of subscripts representing path: e.g., `["123", "NAME"]`)
- [ ] Define `Value` enum with variants: `String`, `Integer(i64)`, `Double(f64)`, `Boolean(bool)`
- [ ] Implement `Serialize`/`Deserialize` for `Value` using serde
- [ ] Add `Ord` and lexicographic ordering for `Key` and `Subscript`
- [ ] Add comprehensive unit tests for key ordering (verify lex order)
- [ ] Add unit tests for `Name` enum (both Global and Local variants)

### 1.3 Node Structure (rumps-types)
- [ ] Define `NodeData` struct containing:
  - Optional value: `Option<Value>`
  - Flag indicating whether descendants exist: `bool`
- [ ] Define `Node` struct representing a B-tree node:
  - Keys: `Vec<Subscript>` (sorted)
  - Children: `Vec<NodeId>` or `Vec<Box<Node>>` (decide approach)
  - Values: `Vec<NodeData>` (one per key, plus one extra for rightmost child)
  - Is leaf: `bool`
- [ ] Add `NodeId` type (page offset or handle for disk references)
- [ ] Ensure `Node` and `NodeData` derive `Serialize`/`Deserialize`
- [ ] Add size calculation methods for nodes (needed for B-tree splitting)

---

## Phase 2: In-Memory B-Tree Implementation

**Note**: The B-tree supports both `Name::Global` and `Name::Local` variables with the same operations. Only `Name::Global` entries will be persisted to disk in Phase 4.

### 2.1 B-Tree Structure (rumps-storage)
- [ ] Define `BTree` struct with:
  - Map from `Name` to root node reference (supports both Global and Local)
  - Order/branching factor (min/max keys per node)
  - Metadata (height, node count, etc.)
- [ ] Implement `BTree::new()` constructor
- [ ] Implement `BTree::find_node()` - navigate tree to find node containing key
- [ ] Implement `BTree::split_node()` - split full nodes during insertion
- [ ] Implement `BTree::merge_nodes()` - merge underfull nodes during deletion

### 2.2 MUMPS Operations - SET
- [ ] Implement `BTree::set(name: &Name, key: &Key, value: Value)`:
  - Navigate to appropriate leaf node
  - Insert/update key-value pair
  - Update parent `has_descendants` flags up the path
  - Handle node splits and tree growth
- [ ] Add tests for SET on empty tree (both Global and Local)
- [ ] Add tests for SET with existing keys (updates)
- [ ] Add tests for SET triggering node splits
- [ ] Add tests for SET on multi-level subscripts (e.g., `["A", "B", "C"]`)
- [ ] Add tests verifying Global and Local namespaces are separate

### 2.3 MUMPS Operations - GET
- [ ] Implement `BTree::get(name: &Name, key: &Key) -> Option<&Value>`:
  - Navigate tree following key path
  - Return value if exists
- [ ] Add tests for GET on non-existent keys
- [ ] Add tests for GET on existing keys
- [ ] Add tests for GET on partial paths (should return None if no value at that node)

### 2.4 MUMPS Operations - KILL
- [ ] Implement `BTree::kill(name: &Name, key: &Key)`:
  - Navigate to node
  - Delete entire subtree rooted at key
  - Update parent `has_descendants` flags
  - Handle node merging and tree shrinking
- [ ] Add tests for KILL leaf nodes (both Global and Local)
- [ ] Add tests for KILL intermediate nodes (removes subtree)
- [ ] Add tests for KILL root
- [ ] Verify tree structure remains valid after KILL

### 2.5 MUMPS Operations - DATA
- [ ] Implement `BTree::data(name: &Name, key: &Key) -> DataResult`:
  - Return enum: `NoData`, `HasValue`, `HasDescendants`, `Both`
- [ ] Add tests for all four DATA states
- [ ] Verify correct behavior for partial paths

### 2.6 MUMPS Operations - ORDER (Iterator)
- [ ] Implement `BTree::order(name: &Name, key: &Key) -> Option<Key>`:
  - Find next key in lexicographic order
  - Handle navigating between leaf nodes
- [ ] Implement `BTreeIterator` for sequential traversal
- [ ] Add tests for ORDER on empty tree
- [ ] Add tests for ORDER returning next sibling
- [ ] Add tests for ORDER wrapping to next parent's child
- [ ] Add tests for exhaustive iteration over entire tree

---

## Phase 3: Serialization Layer

### 3.1 Bincode Setup
- [ ] Add `bincode` dependency to `rumps-storage/Cargo.toml`
- [ ] Create `crates/rumps-storage/src/serialize.rs` module
- [ ] Define serialization configuration (endianness, int encoding, etc.)
- [ ] Implement `serialize_node(node: &Node) -> Result<Vec<u8>>`
- [ ] Implement `deserialize_node(bytes: &[u8]) -> Result<Node>`

### 3.2 Node Serialization Format
- [ ] Design fixed-size header for nodes:
  - Node type (leaf/internal)
  - Number of keys
  - Checksum (optional, for integrity)
- [ ] Serialize keys as length-prefixed strings
- [ ] Serialize values using bincode for `Value` enum
- [ ] Serialize child pointers as `NodeId` (page offsets)
- [ ] Add padding to ensure nodes fit in fixed page size

### 3.3 Serialization Tests
- [ ] Test round-trip serialization for leaf nodes
- [ ] Test round-trip serialization for internal nodes
- [ ] Test serialization of nodes with all Value types
- [ ] Test serialization size limits (ensure nodes fit in page)
- [ ] Test handling of oversized keys/values (error or truncate?)

---

## Phase 4: Disk Persistence

**Note**: Only `Name::Global` entries are persisted to disk. `Name::Local` entries remain in memory only and are not serialized.

**Design Note**: While the implementation in this phase will be synchronous, design data structures with async/concurrency in mind (e.g., avoid patterns that would be difficult to wrap with locks later). Phase 5 will add async operations with concurrent reads and exclusive writes.

### 4.1 Page-Based Storage
- [ ] Define `PAGE_SIZE` constant (e.g., 4096 bytes)
- [ ] Create `crates/rumps-storage/src/page.rs` module
- [ ] Define `PageId` type (u64 offset into file)
- [ ] Implement `PageCache` struct:
  - LRU cache of pages in memory
  - Dirty page tracking
  - Flush mechanism
- [ ] Implement `PageAllocator`:
  - Track free pages (bitmap or free list)
  - Allocate new pages on demand
  - Reclaim pages on node deletion

### 4.2 Storage Engine
- [ ] Create `crates/rumps-storage/src/engine.rs` module
- [ ] Define `StorageEngine` struct:
  - File handle for data file
  - Page cache
  - Page allocator
  - Root page ID for each global (only `Name::Global` variants)
- [ ] Implement `StorageEngine::open(path: &Path) -> Result<Self>`
- [ ] Implement `StorageEngine::create(path: &Path) -> Result<Self>`
- [ ] Implement `StorageEngine::write_page(page_id: PageId, data: &[u8])`
- [ ] Implement `StorageEngine::read_page(page_id: PageId) -> Result<Vec<u8>>`

### 4.3 Global Management
- [ ] Define `GlobalRegistry` struct:
  - Map from global name strings to root `PageId` (only persists `Name::Global`)
  - Store in header page (page 0)
- [ ] Implement `GlobalRegistry::register_global(name: String, root: PageId)`
- [ ] Implement `GlobalRegistry::get_root(name: &str) -> Option<PageId>`
- [ ] Serialize/deserialize global registry to/from page 0
- [ ] Add tests for multi-global persistence
- [ ] Add tests verifying Local variables are NOT persisted

### 4.4 Persistence Integration
- [ ] Integrate `BTree` with `StorageEngine`:
  - Load nodes from disk on access (only for `Name::Global`)
  - Write modified nodes back to disk (only for `Name::Global`)
  - Keep `Name::Local` entirely in memory
  - Lazy loading of child nodes
- [ ] Implement `PersistedBTree` wrapper:
  - Holds reference to `StorageEngine`
  - Implements same MUMPS operations as `BTree`
  - Manages node loading/storing transparently
  - Filters out `Name::Local` from persistence operations
- [ ] Add `flush()` method to persist all dirty pages (only `Name::Global`)
- [ ] Add `close()` method to clean up resources

### 4.5 Crash Recovery
- [ ] Implement write-ahead logging (WAL) or journal:
  - Log changes before applying to tree
  - Replay log on recovery
- [ ] Add `StorageEngine::recover()` method
- [ ] Add tests for crash simulation (interrupted writes)
- [ ] Add tests for recovery correctness

---

## Phase 5: Public API

**Concurrency Model**: The public API will be async and support concurrent reads with exclusive writes using `tokio::sync::RwLock`.

### 5.1 Async Database Handle
- [ ] Add `tokio` dependency to `rumps-storage/Cargo.toml`
- [ ] Create `crates/rumps-storage/src/database.rs` module
- [ ] Define `Database` struct as main entry point:
  - Wraps `StorageEngine` with `Arc<RwLock<_>>` for concurrent access
  - Manages multiple globals and locals
  - Separate storage for `Name::Global` (persistent) and `Name::Local` (ephemeral)
  - Uses `RwLock` to allow concurrent reads, exclusive writes
- [ ] Implement `Database::open(path: &Path) -> Result<Self>` (sync, returns async-compatible handle)
- [ ] Implement `Database::create(path: &Path) -> Result<Self>` (sync, returns async-compatible handle)
- [ ] Implement async high-level MUMPS operations:
  - `async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()>` (write lock)
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>` (read lock)
  - `async fn kill(&self, name: &Name, key: &Key) -> Result<()>` (write lock)
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>` (read lock)
  - `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>` (read lock)
  - `async fn query(&self, name: &Name, key: &Key) -> Result<Option<Key>>` (read lock)

### 5.2 Variable Handle (Optional)
- [ ] Consider adding `Variable` struct for ergonomic API:
  - Reference to `Database` (via `Arc`)
  - `Name` (Global or Local)
  - Provides scoped operations without passing name repeatedly
- [ ] If implemented, add async convenience methods:
  - `async fn exists(&self, key: &Key) -> Result<bool>`
  - `async fn iter(&self) -> Result<VariableIterator>` (async stream for traversal)

### 5.3 API Documentation
- [ ] Add rustdoc comments to all public types
- [ ] Add usage examples in doc comments (with async/await)
- [ ] Create `examples/basic_usage.rs` demonstrating:
  - Opening database
  - Using async operations with `tokio::main`
  - Setting values on globals (`^PATIENT(123)="John"`)
  - Setting values on locals (`TEMP(1)="value"`)
  - Getting values from both namespaces
  - Iterating over keys
  - Killing subtrees
  - Demonstrating that locals don't persist across database reopens
- [ ] Create `examples/concurrent_access.rs` demonstrating:
  - Multiple concurrent readers accessing same data
  - Concurrent reads while writes are happening
  - Using `tokio::spawn` for parallel operations

---

## Phase 6: Testing & Validation

### 6.1 Unit Tests
- [ ] Verify all Phase 2 tests pass (in-memory B-tree)
- [ ] Verify all Phase 3 tests pass (serialization)
- [ ] Add unit tests for page cache LRU eviction
- [ ] Add unit tests for page allocator

### 6.2 Integration Tests
- [ ] Create `crates/rumps-storage/tests/integration_tests.rs`
- [ ] Test persisting data and reopening database (globals persist, locals don't)
- [ ] Test multiple globals in same database
- [ ] Test multiple locals in same session
- [ ] Test that `Name::Global` and `Name::Local` with same name are separate
- [ ] Test large datasets (millions of keys)
- [ ] Test edge cases:
  - Empty keys
  - Very long keys
  - Very large values
  - Deep nesting (many subscript levels)

### 6.2b Concurrency Tests
- [ ] Create `crates/rumps-storage/tests/concurrency_tests.rs`
- [ ] Test concurrent reads from multiple tasks (should succeed)
- [ ] Test concurrent writes to different keys (should be serialized)
- [ ] Test concurrent writes to same key (should be serialized, last write wins)
- [ ] Test read-write concurrency (reads should see consistent state)
- [ ] Test many readers with occasional writer (verify no deadlocks)
- [ ] Stress test: spawn 100+ tasks doing random operations
- [ ] Test that iterators work correctly under concurrent modifications

### 6.3 Property-Based Testing
- [ ] Add `proptest` or `quickcheck` dependency
- [ ] Write property tests for:
  - SET then GET returns same value
  - ORDER returns keys in lexicographic order
  - KILL removes all descendants
  - Serialization round-trips preserve data
  - Database persistence preserves all data

### 6.4 Performance Testing
- [ ] Create `benches/` directory with criterion benchmarks
- [ ] Benchmark SET operation throughput
- [ ] Benchmark GET operation latency
- [ ] Benchmark ORDER iteration speed
- [ ] Benchmark database open time (cold start)
- [ ] Profile memory usage under load

### 6.5 Documentation Tests
- [ ] Ensure all doc examples compile and run
- [ ] Add `cargo test --doc` to CI (future)

---

## Phase 7: Polish & Refine

### 7.1 Error Handling
- [ ] Define custom error types in `rumps-types`:
  - `StorageError` enum
  - `SerializationError` enum
- [ ] Use `thiserror` for error derivations
- [ ] Add context to errors (which global, which key, etc.)
- [ ] Ensure all `Result` types have meaningful errors

### 7.2 Code Quality
- [ ] Run `cargo fmt` on all crates
- [ ] Run `cargo clippy` and fix all warnings
- [ ] Ensure `use_self` lint is respected (use `Self` where appropriate)
- [ ] Review all `unwrap()` calls and replace with proper error handling
- [ ] Add `#![warn(missing_docs)]` to crate roots

### 7.3 Final Validation
- [ ] Run full test suite: `cargo test --workspace`
- [ ] Run benchmarks: `cargo bench --workspace`
- [ ] Build docs: `cargo doc --workspace --no-deps`
- [ ] Review CLAUDE.md and check off Goal 1 items

---

## Future Considerations (Post-Goal 1)

These are not part of the current plan but should be kept in mind:

- **Transactions**: ACID guarantees for multi-operation workflows (multi-key transactions)
- **Advanced Concurrency**: Lock-free data structures, optimistic concurrency control (current plan uses RwLock)
- **Compression**: Compress nodes/pages to save disk space
- **Encryption**: Optional encryption at rest
- **Query Language**: Parser and evaluator for MUMPS commands (rewrite old parser)
- **Networking**: Client-server protocol for remote access
- **Replication**: Multi-node deployment with data replication

---

## Progress Tracking

**Status**: In Progress
**Current Phase**: Phase 1.2 (Core Type Definitions)
**Completed Checkboxes**: 8 / ~140

**Recent Changes**:
- Updated to support both `Name::Global` and `Name::Local` variables
- Only `Name::Global` entries are persisted to disk
- Both namespaces support the same MUMPS operations (SET, GET, KILL, DATA, ORDER)
- Public API will be async with concurrent reads and exclusive writes (using `tokio::sync::RwLock`)
- Added comprehensive concurrency testing suite

---

Last Updated: 2025-11-11
