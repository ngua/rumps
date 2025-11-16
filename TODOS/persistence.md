# RUMPS Persistent Storage Implementation Plan

This document tracks the implementation of Goal 1: Create a MUMPS-style binary tree storage system with persistent B-tree-backed globals.

## Core Principles

- **Unified Data Model**: In-memory and on-disk structures must be equivalent and synchronized
- **MUMPS Semantics**: Both globals (`^NAME`) and locals (`NAME`) are sparse multi-dimensional arrays with extended collation ordering
- **Two Namespaces**: Globals are persistent (written to disk), locals are ephemeral (memory-only)
- **Explicit Transactions**: ALL writes to globals must occur within transactions (unlike MUMPS `LOCK`, RUMPS requires explicit transaction blocks)
- **Write-Ahead Logging (WAL)**: All transactional changes logged before commit for crash recovery and durability
- **Type Sharing**: Common types in `rumps-types` for use across storage and query layers
- **Idiomatic Rust**: Follow project lints and formatting rules

---

## Architecture: Query Layer vs Storage Layer

RUMPS separates **logical semantics** (how users interact with data) from **physical storage** (how data is stored on disk). This distinction is crucial to understanding the system architecture.

### Query Layer (Future: rumps-query)

The **query layer** provides MUMPS-style hierarchical data access with trie-like navigation semantics:

```mumps
; MUMPS code operates on hierarchical paths
SET ^PATIENT(123,"NAME") = "John Doe"
SET ^PATIENT(123,"DOB") = "1974-08-09"
SET ^PATIENT(123,"ADDR") = "123 Main St"

; Tree appears hierarchical to the user:
^PATIENT
  └─ 123
      ├─ "NAME"  = "John Doe"
      ├─ "DOB"   = "1974-08-09"
      └─ "ADDR"  = "123 Main St"
```

**Query operations** like `$ORDER`, `$QUERY`, and `$DATA` navigate this logical tree structure, providing:
- Hierarchical traversal (parent → child relationships)
- Subtree operations (KILL removes entire subtrees)
- Data presence checking (does a node have value? descendants? both?)

### Storage Layer (Current: rumps-storage)

The **storage layer** uses a flat B+-tree for efficient disk persistence. Keys are **complete paths**, not individual subscripts:

```text
Logical View (Query Layer):        Physical Storage (B+-tree):
^PATIENT                           ┌─────────────────────────────────┐
  └─ 123                           │ B+-tree Node (Leaf)             │
      ├─ "ADDR"                    ├─────────────────────────────────┤
      ├─ "DOB"                     │ keys: [                         │
      └─ "NAME"                    │   Key([123, "ADDR"]),           │
                                   │   Key([123, "DOB"]),            │
                                   │   Key([123, "NAME"]),           │
                                   │   Key([124, "NAME"]),           │
                                   │ ]                               │
                                   │ values: [                       │
                                   │   "123 Main St",                │
                                   │   "1974-08-09",                 │
                                   │   "John Doe",                   │
                                   │   "Jane Smith",                 │
                                   │ ]                               │
                                   └─────────────────────────────────┘
```

**Why B+-tree instead of trie?**

1. **Efficient Disk I/O**: Read many key-value pairs in one 4KB page
2. **Cache Locality**: Keeps related data together (e.g., all patient 123 fields)
3. **Scalability**: Logarithmic depth regardless of key hierarchy depth
4. **Sequential Scans**: Fast iteration over sorted key ranges

**Trie would be disastrous:**
- One disk page per subscript level → `^PATIENT(123,"NAME")` = 3 disk reads minimum
- Poor cache utilization (separate pages for each node)
- O(path-depth) I/O complexity instead of O(log n)

### Key Type Mapping

| Concept             | Query Layer View           | Storage Layer Reality           |
|---------------------|----------------------------|---------------------------------|
| **Global Root**     | `^PATIENT`                 | Root `NodeId` in B+-tree        |
| **Subscript Path**  | `(123, "NAME")`            | `Key([123, "NAME"])`            |
| **Hierarchical Node** | Parent-child relationship | Complete path in sorted order   |
| **Subtree**         | All descendants under path | Range of keys with common prefix |

### Example: SET Operation Flow

```rust
// User writes (Query Layer):
db.set(&Name::Global("PATIENT"), &key![123, "NAME"], "John Doe".into()).await?;

// Storage Layer receives:
// - Full key: Key([123, "NAME"])
// - Value: NodeData { value: Some("John Doe"), has_descendants: false }
// - Inserts into B+-tree at sorted position

// On disk (simplified):
// [Key([123,"ADDR"]) | Key([123,"DOB"]) | Key([123,"NAME"]) ← inserted here | Key([124,"NAME"])]
```

### Example: $ORDER (Next Key) Flow

```mumps
; User query (Query Layer):
SET next = $ORDER(^PATIENT(123,"DOB"))  ; Returns "NAME"
```

```rust
// Storage Layer implements:
// 1. Find key >= Key([123, "DOB"]) in B+-tree
// 2. Return next key in sorted order: Key([123, "NAME"])
// 3. Query layer extracts last subscript: "NAME"
```

### NodeData: Bridging Both Layers

`NodeData` connects the two layers by tracking whether nodes have descendants:

```rust
pub struct NodeData {
    pub value: Option<Value>,        // Actual data (if any)
    pub has_descendants: bool,        // Does this path have children?
}
```

This enables the query layer to provide MUMPS `$DATA` semantics:

| State             | `value`    | `has_descendants` | MUMPS `$DATA` | Example                                       |
|-------------------|------------|-------------------|---------------|-----------------------------------------------|
| Empty             | `None`     | `false`           | 0             | Deleted node                                  |
| Has Value         | `Some(v)`  | `false`           | 1             | `^PATIENT(123,"NAME")="John"` (leaf)          |
| Has Descendants   | `None`     | `true`            | 10            | `^PATIENT(123)` (no value, but has fields)    |
| Both              | `Some(v)`  | `true`            | 11            | `^PATIENT(123)="Active"` (plus fields)        |

### Implementation Phases

This plan focuses on the **storage layer** (Phases 1-7). The query layer will be built later and will:
- Map hierarchical operations to flat B+-tree operations
- Maintain the `has_descendants` flags during SET/KILL
- Implement `$ORDER`/`$QUERY` using B+-tree range scans
- Provide the MUMPS command interpreter

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
- [x] Define `Name` enum with `Global(String)` and `Local(String)` variants (e.g., `^PATIENT` vs `PATIENT`)
- [x] Implement `Serialize`/`Deserialize` for `Name` using serde
- [x] Add `Display` for `Name` (format with/without caret)
- [x] Define `Subscript` type (string subscript in a key path)
- [x] Define `Key` type (sequence of subscripts representing path: e.g., `["123", "NAME"]`)
- [x] Define `Value` enum with variants: `String`, `Integer(i64)`, `Double(f64)`, `Boolean(bool)`
- [x] Implement `Serialize`/`Deserialize` for `Value` using serde (with compact binary encoding!)
- [x] Add `Ord` and extended MUMPS collation ordering for `Key` and `Subscript`
- [x] Add comprehensive unit tests for key ordering (verify extended MUMPS collation)
- [x] Add unit tests for `Name` enum (both Global and Local variants)
- [x] Define transaction-related types: `TransactionId`, `TransactionState` enum

### 1.3 Node Structure (rumps-types)
- [x] Define `NodeData` struct containing:
  - Optional value: `Option<Value>`
  - Flag indicating whether descendants exist: `bool`
- [x] Define `Node` struct representing a B-tree node:
  - Keys: `Vec<Key>` (complete paths, sorted)
  - Children: `Vec<NodeId>` (decided: use NodeId for lazy loading)
  - Values: `Vec<NodeData>` (one per key)
  - Is leaf: `bool`
- [x] Add `NodeId` type (page offset or handle for disk references)
- [x] Ensure `Node` and `NodeData` have custom `Serialize`/`Deserialize` (compact encoding)
- [x] Add size calculation methods for nodes (needed for B-tree splitting)

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

## Phase 4: Disk Persistence with WAL

**Note**: Only `Name::Global` entries are persisted to disk. `Name::Local` entries remain in memory only and are not serialized.

**Design Note**: While the implementation in this phase will be synchronous, design data structures with async/concurrency in mind (e.g., avoid patterns that would be difficult to wrap with locks later). Phase 5 will add async transactions with WAL integration.

### 4.1 Write-Ahead Log (WAL)
- [ ] Create `crates/rumps-storage/src/wal.rs` module
- [ ] Define WAL record types:
  - Transaction begin/commit/abort records
  - SET operation records (name, key, old value, new value)
  - KILL operation records (name, key, subtree metadata)
  - Checkpoint records
- [ ] Define WAL file format:
  - Record header (type, length, transaction ID, checksum)
  - Serialized operation data
  - Transaction boundaries
- [ ] Implement `WalWriter`:
  - Append records to WAL file
  - Flush/fsync on transaction commit (configurable sync policy)
  - Handle WAL file rotation when size exceeds threshold
- [ ] Implement `WalReader`:
  - Read WAL records sequentially
  - Verify checksums
  - Parse records by type
- [ ] Add WAL recovery logic:
  - Replay uncommitted transactions on startup
  - Handle partial writes (incomplete records)
  - Rebuild state from last checkpoint + WAL replay
- [ ] Implement WAL checkpointing:
  - Periodically flush dirty pages to disk
  - Write checkpoint record to WAL
  - Truncate old WAL entries before checkpoint
- [ ] Add tests for WAL write/read round-trip
- [ ] Add tests for crash recovery scenarios

### 4.2 Page-Based Storage
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

### 4.3 Storage Engine with WAL Integration
- [ ] Create `crates/rumps-storage/src/engine.rs` module
- [ ] Define `StorageEngine` struct:
  - File handle for data file
  - Page cache
  - Page allocator
  - WAL writer/reader
  - Root page ID for each global (only `Name::Global` variants)
- [ ] Implement `StorageEngine::open(path: &Path) -> Result<Self>`:
  - Open data file
  - Initialize page cache and allocator
  - Open WAL file
  - Run WAL recovery if needed
- [ ] Implement `StorageEngine::create(path: &Path) -> Result<Self>`
- [ ] Implement `StorageEngine::write_page(page_id: PageId, data: &[u8])`:
  - Write to page cache (mark dirty)
  - DO NOT immediately flush (handled by checkpointing)
- [ ] Implement `StorageEngine::read_page(page_id: PageId) -> Result<Vec<u8>>`
- [ ] Add WAL-aware methods:
  - `begin_transaction() -> TransactionId`
  - `log_operation(txn_id, operation)` - append to WAL
  - `commit_transaction(txn_id)` - write commit record, fsync WAL
  - `abort_transaction(txn_id)` - write abort record

### 4.4 Global Management
- [ ] Define `GlobalRegistry` struct:
  - Map from global name strings to root `PageId` (only persists `Name::Global`)
  - Store in header page (page 0)
- [ ] Implement `GlobalRegistry::register_global(name: String, root: PageId)`
- [ ] Implement `GlobalRegistry::get_root(name: &str) -> Option<PageId>`
- [ ] Serialize/deserialize global registry to/from page 0
- [ ] Add tests for multi-global persistence
- [ ] Add tests verifying Local variables are NOT persisted

### 4.5 Persistence Integration with WAL
- [ ] Integrate `BTree` with `StorageEngine` and WAL:
  - Load nodes from disk on access (only for `Name::Global`)
  - Keep `Name::Local` entirely in memory
  - Lazy loading of child nodes
  - Modified nodes logged to WAL (not immediately written to disk)
- [ ] Implement `PersistedBTree` wrapper:
  - Holds reference to `StorageEngine`
  - Implements same MUMPS operations as `BTree`
  - Manages node loading/storing transparently
  - Filters out `Name::Local` from persistence operations
  - All writes go through WAL first
- [ ] Add checkpoint/flush logic:
  - `checkpoint()` method to flush dirty pages (only `Name::Global`)
  - Periodic background checkpointing
  - Write checkpoint record to WAL
- [ ] Add `close()` method to clean up resources (flush + close WAL)

---

## Phase 5: Transaction-Based Public API

**Transaction Model**: ALL writes to globals must occur within explicit transactions. Locals can be modified freely outside transactions.

**Concurrency Model**: The public API will be async with snapshot isolation for reads and exclusive locks for transaction commits.

### 5.1 Transaction Infrastructure
- [ ] Add `tokio` dependency to `rumps-storage/Cargo.toml`
- [ ] Create `crates/rumps-storage/src/transaction.rs` module
- [ ] Define `Transaction` struct:
  - Transaction ID
  - Reference to `Database` (via `Arc`)
  - Buffered writes (in-memory staging for transaction)
  - Snapshot of database state at transaction start
  - Transaction state (Active, Committed, Aborted)
- [ ] Implement transaction lifecycle methods:
  - `begin()` - create transaction, get snapshot
  - `commit() -> Result<()>` - validate, write to WAL, apply changes
  - `rollback()` - discard buffered writes
- [ ] Add MUMPS operations on `Transaction`:
  - `async fn set(&mut self, name: &Name, key: &Key, value: Value) -> Result<()>`
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>`
  - `async fn kill(&mut self, name: &Name, key: &Key) -> Result<()>`
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>`
  - `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>`
- [ ] Enforce transaction rules:
  - Writes to `Name::Global` MUST be in transaction (return error otherwise)
  - `Name::Local` modifications work outside transactions
  - GET/DATA/ORDER can work with or without transactions

### 5.2 Async Database Handle
- [ ] Create `crates/rumps-storage/src/database.rs` module
- [ ] Define `Database` struct as main entry point:
  - Wraps `StorageEngine` with `Arc<Mutex<_>>` for exclusive write access during commits
  - Manages multiple globals and locals
  - Separate storage for `Name::Global` (persistent) and `Name::Local` (ephemeral)
  - Transaction manager
- [ ] Implement `Database::open(path: &Path) -> Result<Self>` (sync, returns async-compatible handle)
- [ ] Implement `Database::create(path: &Path) -> Result<Self>` (sync, returns async-compatible handle)
- [ ] Implement transaction API:
  - `async fn transaction<F, R>(&self, f: F) -> Result<R>` where `F: FnOnce(&mut Transaction) -> Future<Result<R>>`
  - Auto-commit on Ok, auto-rollback on Err
  - Example usage:
    ```rust
    db.transaction(|txn| async move {
        let name = txn.get(&Name::Global("PATIENT".into()), &key).await?;
        txn.set(&Name::Global("PATIENT".into()), &key, new_value).await?;
        Ok(())
    }).await?;
    ```
- [ ] Add read-only operations (no transaction required):
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>` (snapshot read)
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>` (snapshot read)
  - `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>` (snapshot read)
- [ ] Add local variable operations (no transaction required):
  - `async fn set_local(&self, name: &Name, key: &Key, value: Value) -> Result<()>`
  - Must verify `name.is_local()`, return error if global

### 5.3 API Documentation
- [ ] Add rustdoc comments to all public types
- [ ] Add usage examples in doc comments (with async/await and transactions)
- [ ] Create `examples/basic_usage.rs` demonstrating:
  - Opening database
  - Using transactions with `db.transaction()` closure
  - Setting values on globals within transaction
  - Setting values on locals (no transaction needed)
  - Getting values from both namespaces
  - Transaction rollback on error
  - Demonstrating that locals don't persist across database reopens
- [ ] Create `examples/transaction_examples.rs` demonstrating:
  - Simple transaction with multiple writes
  - Read-modify-write pattern in transaction
  - Transaction rollback (error handling)
  - Snapshot isolation (concurrent reads don't see uncommitted writes)
- [ ] Create `examples/concurrent_transactions.rs` demonstrating:
  - Multiple concurrent transactions
  - Conflict resolution at commit time
  - Using `tokio::spawn` for parallel transactions

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

### 6.2b Transaction Tests
- [ ] Create `crates/rumps-storage/tests/transaction_tests.rs`
- [ ] Test transaction commit writes to WAL and applies changes
- [ ] Test transaction rollback discards all changes
- [ ] Test that writes outside transaction to globals return error
- [ ] Test that writes to locals work outside transactions
- [ ] Test snapshot isolation (reads see consistent state)
- [ ] Test transaction conflict detection (if applicable)
- [ ] Test WAL recovery replays committed transactions correctly
- [ ] Test WAL recovery ignores aborted transactions

### 6.2c Concurrency Tests
- [ ] Create `crates/rumps-storage/tests/concurrency_tests.rs`
- [ ] Test concurrent transactions (multiple writers)
- [ ] Test concurrent reads during active transactions (snapshot isolation)
- [ ] Test transaction serialization at commit time
- [ ] Test many concurrent read-only transactions
- [ ] Test mixed read/write transactions
- [ ] Stress test: spawn 100+ concurrent transactions
- [ ] Test no deadlocks with many concurrent transactions

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

- **Advanced Concurrency**: Lock-free data structures, optimistic concurrency control (current plan uses Mutex for commit serialization)
- **Multi-Version Concurrency Control (MVCC)**: Full MVCC for better read concurrency (current plan uses snapshot isolation)
- **Savepoints**: Nested transactions with partial rollback
- **Compression**: Compress nodes/pages to save disk space
- **Encryption**: Optional encryption at rest
- **Query Language**: Parser and evaluator for MUMPS commands (rewrite old parser)
- **Networking**: Client-server protocol for remote access
- **Replication**: Multi-node deployment with data replication
- **Distributed Transactions**: Two-phase commit for multi-node transactions

---

## Progress Tracking

**Status**: In Progress
**Current Phase**: Phase 1 Complete! Ready for Phase 2 (In-Memory B-Tree Implementation)
**Completed Checkboxes**: 24 / ~160

**Recent Changes** (2025-11-16 - Today's Progress):
- ✅ Extended `Value` and `Subscript` types with `Char` and `Json` variants
- ✅ Updated collation order: Boolean < Number < Char < String < Json
- ✅ Fixed serialization for JSON compatibility with bincode
- ✅ Added comprehensive tests for all new type variants
- ✅ Completed transaction types module:
  - `TransactionId`: Unique transaction identifier
  - `TransactionState`: Lifecycle states (Active, Committed, Aborted)
  - `TransactionTimestamp`: Logical timestamps for snapshot isolation
  - `IsolationLevel`: Transaction isolation levels (SnapshotIsolation)
  - `TransactionMetadata`: Complete transaction metadata
- ✅ Completed node structure module (Phase 1.3):
  - `NodeId`: Transparent wrapper around u64 for disk page references
  - `NodeData`: Stores optional value + has_descendants flag with 4-state encoding
  - `Node`: B-tree node with keys, children, values, and is_leaf flag
  - Custom compact serialization for both `NodeData` (tag-based) and `Node`
  - Size calculation methods: `serialized_size()` and `would_fit()` for B-tree splitting
- ✅ Added full test coverage for all node types (103 tests passing)

**Previous Changes** (2025-11-15):
- ✅ Completed `Name` enum with Global/Local variants and full serde support
- ✅ Completed `Subscript` type with extended collation
- ✅ Completed `Key` type with newtype pattern and transparent serde
- ✅ Completed `Value` enum with compact binary encoding
- ✅ Refactored to functional programming style (no early returns, iterator methods)

**Previous Changes** (2025-11-15 - Planning):
- **Transaction Model**: Added explicit transaction requirement for all global writes
- **Write-Ahead Log (WAL)**: Restructured Phase 4 to make WAL the foundation of persistence
- **Public API**: Rewrote Phase 5 for transaction-based API with `db.transaction()` closure pattern
- **Concurrency**: Changed from RwLock to transaction-level isolation with snapshot reads
- **Type System**: Added transaction-related types (TransactionId, TransactionState)
- **Testing**: Added comprehensive transaction and WAL recovery tests

**Previous Changes** (2025-11-11):
- Updated to support both `Name::Global` and `Name::Local` variables
- Only `Name::Global` entries are persisted to disk
- Both namespaces support the same MUMPS operations (SET, GET, KILL, DATA, ORDER)
- Extended MUMPS collation order: Boolean < Number < String
- Added comprehensive concurrency testing suite

---

Last Updated: 2025-11-16
