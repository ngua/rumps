# RUMPS Persistent Storage Implementation Plan

**IMPORTANT NOTE**: For _all_ file formats (e.g. WAL, superblocks, etc...) DO NOT increment version numbers. We have not finished the DB yet!

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
- [x] Ensure `Node` and `NodeData` have `Serialize`/`Deserialize` (via `NodeRaw` for `Node`, custom for `NodeData`)
- [x] Add size calculation methods for nodes (needed for B-tree splitting)

---

## Phase 2: In-Memory B-Tree Implementation (Async-First)

**Note**: The B-tree supports both `Name::Global` and `Name::Local` variables with the same operations. Only `Name::Global` entries will be persisted to disk in Phase 4.

**Design Principle**: All APIs are async from day one to avoid breaking changes when adding disk persistence in Phase 4. Uses `Arc<BTree>` pattern for thread-safe sharing.

### 2.1 B-Tree Structure (rumps-storage) ✅ COMPLETE
- [x] Create `crates/rumps-storage/src/error.rs` with `StorageError` enum
- [x] Define `BTree` struct with:
  - `roots: RwLock<BTreeMap<Name, NodeId>>` - ordered variable registry
  - `nodes: RwLock<HashMap<NodeId, Node>>` - node storage pool
  - `allocator: Arc<dyn NodeAllocator>` - flexible ID allocation (with `#[async_trait]`)
  - `min_degree: usize` - B-tree branching factor
  - `max_memory_bytes: Option<usize>` - optional memory limit
  - `stats: RwLock<BTreeStats>` - metrics tracking
  - `storage: Option<Arc<dyn AsyncStorageEngine>>` (for Phase 4)
- [x] Implement `BTree::new(min_degree) -> Result<Self>` constructor with validation
- [x] Implement `BTree::with_config(min_degree, max_memory_bytes) -> Result<Self>`
- [x] Implement `Default` trait (min_degree = 3)
- [x] Implement accessor methods: `min_degree()`, `node_count()`, `has_memory_limit()`, `stats()`, `check_memory_limit()`
- [x] Add `BTreeStats` tracking (height, node_count, key_count, splits, merges, memory usage)
- [x] Add `NodeAllocator` trait with `#[async_trait]` and `IncrementingAllocator` implementation
- [x] Add comprehensive unit tests (12 tests including concurrent access tests)
- [x] Add complete rustdoc with examples and scalability documentation
- [x] Implement `async fn find_node(&self, id: NodeId) -> Result<Node>` - navigate tree
- [x] Implement `async fn split_node(&self, id: NodeId) -> Result<(Key, NodeData, NodeId)>` - split full nodes, returns median key+value to prevent data loss
- [x] Implement `async fn merge_nodes(&self, left_id: NodeId, separator_key: Key, separator_value: NodeData, right_id: NodeId) -> Result<()>` - merge underfull nodes by combining left + separator + right (inverse of split; B-tree semantics require separator value from parent)

### 2.2 MUMPS Operations - SET ✅ IMPLEMENTATION COMPLETE

**Status**: Core implementation complete. Tests organized into `set_internal_tests` module. Transaction awareness will be added in Phase 5.

**NOTE**: Hierarchy semantics have been implemented, see `docs/hierarchy-semantics.md`

**Transaction Note**: The signature includes `ctx: &TransactionContext` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction awareness (snapshot isolation, write tracking) will be added in Phase 5.

- [x] Implement `async fn set(&self, name: &Name, key: &Key, value: Value, ctx: &TransactionContext) -> Result<()>`:
  - [x] Use `load_node()` for cache-aware node access (via `set_internal` → `insert_non_full_with_data`)
  - [x] Navigate to appropriate leaf node
  - [x] Insert/update key-value pair
  - [x] Update parent `has_descendants` flags up the path (via `ensure_ancestors`)
  - [x] Handle node splits and tree growth
  - [x] Use `save_node()` to persist changes (currently in-memory only; disk persistence in Phase 4)
  - [x] Update `BTreeStats` (key count, splits)
  - ~~Check transaction isolation level and track writes~~ (moved to Phase 5.4)
- [x] **→ See `docs/hierarchy-semantics.md` for complete hierarchical semantics implementation** (required before continuing; now completed)
- [x] Add async tests for SET on empty tree (Global) - **7 tests in `btree::tests::set_internal_tests` module** at `crates/rumps-storage/src/btree.rs:2925`
  - `creates_ancestors`
  - `deep_nesting_creates_all_ancestors`
  - `intermediate_node_becomes_both`
  - `preserves_has_descendants_on_update`
  - `multiple_children_same_parent`
  - `sibling_paths`
  - `concurrent_ancestor_creation`
- [x] Add async tests for SET with existing keys (updates) - covered by `preserves_has_descendants_on_update`
- [x] Add async tests for SET triggering node splits - covered by `test_get_internal_with_tree_splits` at line 2819
- [x] Add async tests for SET on multi-level subscripts (e.g., `["A", "B", "C"]`) - covered by `deep_nesting_creates_all_ancestors`
- [ ] ~~Add async tests verifying Global and Local namespaces are separate - TODO: Add to `set_internal_tests` module~~ 
- [x] Add concurrent SET tests with `Arc<BTree>` - covered by `concurrent_ancestor_creation`

**Missing Tests** (defer to Phase 5 or when transaction layer is ready):
- Tests with Local namespace (`Name::Local`)
- Tests verifying Global and Local namespaces are separate
- Full transaction-aware `set()` tests (will add when Phase 5 transaction support is implemented)

### 2.3 MUMPS Operations - GET ✅ IMPLEMENTATION COMPLETE

**Status**: Core implementation complete. Tests organized into `get_internal_tests` module. Transaction awareness will be added in Phase 5.

**Transaction Note**: The signature includes optional `ctx: Option<&TransactionContext>` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction snapshot isolation will be added in Phase 5.

- [x] Public API implemented - calls `get_internal()` with optional transaction context
- [x] Internal `get_internal()` fully implemented - navigates tree, returns `Option<Arc<NodeData>>`
- [x] Internal `search_from_node()` recursively searches B-tree from given node
- [x] Public `get()` extracts `Option<Value>` from `NodeData`
- [x] Add async tests for GET on non-existent keys - **11 tests in `btree::tests::get_internal_tests` module** at `crates/rumps-storage/src/btree.rs:2585`
  - `nonexistent_variable`
  - `exact_match_single_key`
  - `exact_match_nested_key`
  - `nonexistent_key`
  - `partial_match_no_such_path`
  - `multiple_keys_same_variable`
  - `different_variables`
  - `returns_nodedata_with_flags`
  - `with_tree_splits`
  - `deep_nesting`
  - `concurrent_reads`
- [ ] ~~Complete implementation with full transaction snapshot isolation support~~ (moved to Phase 5.4)

**Note on Value Cloning**: All read operations return owned `Value` rather than references due to async lock lifetime constraints. Values must be cloned from the `RwLock` guard before it drops. This follows standard patterns in async concurrent data structures (like `dashmap::DashMap`). MUMPS values are typically small, making cloning cost acceptable. The public `get()` API simply clones the `Option<Value>` from the `Arc<NodeData>` returned by `get_internal()`. See `TODOS/btree.md` "Value Cloning and Lock Semantics" section for detailed rationale and future optimization strategies.

### 2.4 MUMPS Operations - KILL ✅ IMPLEMENTATION COMPLETE

**Status**: Core implementation complete. Tests organized into `kill_internal_tests` module. Transaction awareness will be added in Phase 5.

**Transaction Note**: The signature includes `ctx: &TransactionContext` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction write tracking will be added in Phase 5.

- [x] Implement KILL operation:
  - Use `load_node()` for cache-aware node access
  - Navigate to node
  - Delete entire subtree rooted at key
  - Update parent `has_descendants` flags
  - Handle node merging and tree shrinking (make sure to promote intermediate value!)
  - Use `save_node()` to persist changes
  - Update `BTreeStats` (key count, merges)
  - ~~Track deletions in transaction context~~ (moved to Phase 5.4)
- [x] Add async tests for KILL leaf nodes (both Global and Local) - **35 tests in `btree::tests::kill_internal_tests` module** at `crates/rumps-storage/src/btree/tests.rs:2618`
  - `kill_single_key`, `kill_nonexistent_key`, `kill_nonexistent_variable`
  - `kill_local_namespace`, `kill_namespaces_are_separate`
- [x] Add async tests for KILL intermediate nodes (removes subtree)
  - `kill_with_descendants`, `kill_subtree_preserves_siblings`, `kill_deep_subtree`
  - `kill_key_that_is_only_ancestor`, `kill_from_internal_node`, `kill_predecessor_replacement_chain`
- [x] Add async tests for KILL root
  - `kill_entire_variable`, `kill_root_shrinks`, `kill_empty_key`
- [x] Verify tree structure remains valid after KILL
  - `kill_verifies_tree_structure`, `kill_causes_node_merge`, `kill_consecutive_merges`
  - `kill_borrow_left_and_right`, `kill_intermixed_keys_survive`
  - `kill_stress_many_keys`, `kill_stress_nested_subtrees`, `min_degree_2_stress`
  - `interleaved_set_kill_invariants`, `set_kill_roundtrip_boundary`

### 2.5 MUMPS Operations - DATA ✅ IMPLEMENTATION COMPLETE

**Status**: Core implementation complete. Tests organized into `data_internal_tests` module. Transaction awareness will be added in Phase 5.

**Transaction Note**: The signature includes optional `ctx: Option<&TransactionContext>` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction snapshot isolation will be added in Phase 5.

- [x] Implement proper `DataStatus` enum with variants: NoData(0), HasValue(1), HasDescendants(10), Both(11)
- [x] Implement DATA operation:
  - Uses `get_internal()` to fetch `NodeData`
  - Maps `(value.is_some(), has_descendants)` to `DataStatus`
  - ~~Use transaction snapshot isolation if context provided~~ (moved to Phase 5.4)
- [x] Add async tests for all four DATA states - **12 tests in `btree::tests::data_internal_tests` module** at `crates/rumps-storage/src/btree/tests.rs:4674`
  - `nonexistent_variable`, `nonexistent_key` (NoData)
  - `has_value_only` (HasValue)
  - `has_descendants_only` (HasDescendants)
  - `has_both_value_and_descendants` (Both)
  - `local_variable`, `namespaces_separate` (namespace tests)
  - `deep_hierarchy`, `multiple_children` (hierarchical tests)
  - `after_kill_becomes_no_data`, `kill_child_updates_parent`, `kill_subtree_updates_ancestor` (state transitions)

### 2.6 MUMPS Operations - ORDER (Iterator) ✅ IMPLEMENTATION COMPLETE

**Status**: Core implementation complete. Tests organized into `order_internal_tests` module. Transaction awareness will be added in Phase 5.

**Transaction Note**: The signature includes optional `ctx: Option<&TransactionContext>` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction snapshot isolation will be added in Phase 5.

- [x] Implement ORDER operation:
  - Uses `load_node()` for cache-aware node access
  - `order_internal(name, None)` returns first (smallest) key
  - `order_internal(name, Some(key))` returns next key after given key
  - `find_leftmost_key()` traverses left spine for minimum
  - `find_successor_key()` handles navigation between leaf nodes via binary search
  - ~~Use transaction snapshot isolation if context provided~~ (moved to Phase 5.4)
- [ ] Implement `BTreeIterator` with async next() method (deferred - can use `order_internal` directly)
- [x] Add async tests for ORDER on empty tree - **15 tests in `btree::tests::order_internal_tests` module** at `crates/rumps-storage/src/btree/tests.rs:4998`
  - `empty_tree_returns_none`, `nonexistent_variable_returns_none`
  - `first_key_single_entry`, `first_key_multiple_entries`
  - `successor_existing_key`, `successor_nonexistent_key`, `successor_last_key_returns_none`, `successor_past_last_key_returns_none`
  - `hierarchical_keys_sorted_correctly`
  - `namespaces_are_separate`
  - `full_iteration`
  - `iteration_across_tree_splits`
  - `extended_collation_order`
  - `stress_many_keys` (500 keys), `stress_deep_hierarchy`

### 2.7 RUMPS Extension - COLLECT (Stream-Based Functional Iterator)

**Note**: This is a RUMPS-specific extension not found in traditional MUMPS. It provides a functional, Rust-idiomatic stream-based interface for iterating and collecting values from the tree, designed for efficient handling of large datasets.

**DSL Design Note**: The RUMPS DSL design, including how `$COLLECT` will serve as the foundation for ALL looping constructs, is documented in **`TODOS/dsl.md`**. See that document for:
- Comparison between traditional MUMPS loops and RUMPS declarative streams
- DSL syntax and operation examples
- Console output formatting options
- Complete language design and implementation roadmap

**Transaction Note**: The signature includes optional `ctx: Option<&TransactionContext>` for future-proofing, but Phase 2 implementation does NOT need to use it. Transaction snapshot isolation will be added in Phase 5.

**Implementation Rationale**: The `$COLLECT` primitive provides a Rust-native stream interface that:
- Integrates naturally with Rust's async ecosystem (futures, tokio)
- Enables lazy evaluation and backpressure handling
- Supports functional composition with standard stream combinators
- Provides memory-efficient processing of large datasets
- Allows parallel processing when appropriate

#### Primary Stream-Based Method Signatures

**Note on API Layers:**
- The **B-tree layer** (`BTree::collects`) passes `&NodeData` to callbacks, exposing both `value` and `has_descendants`
- The **Database layer** (`Database::collects`) will pass `&Option<Value>` to callbacks, hiding internal `NodeData` structure
- This provides a cleaner public API while allowing internal code to access full node metadata

- [x] Implement `BTree::collects_internal` - internal implementation with `&NodeData` (DONE)
- [x] Implement `BTree::collects` - wrapper with transaction context placeholder (DONE)

#### Convenience Methods for Vec Collection

- [x] Implement `async fn collects_vec<P, F, T>(&self, name: &Name, start: Option<&Key>, predicate: P, extract: F, ctx: Option<&TransactionContext>) -> Result<Vec<T>>` (DONE):

#### Supporting Internal Methods

- [x] Implement `async fn get_next_internal(&self, name: &Name, after: Option<&Key>) -> Result<Option<(Key, Arc<NodeData>)>>` (DONE):
  - Navigate B-tree to find the next key after `after` (or first key if `None`)
  - Return both key and data for the next entry
  - Handle transitions between leaf nodes
  - Combines `order_internal` + `get_internal` into single call for efficiency

#### Testing (Completed)

- [x] Test stream iteration over empty tree
- [x] Test stream with start key positioning
- [x] Test predicate-based filtering and early termination
- [x] Test extract function transformations
- [x] Test collecting stream to Vec for small datasets
- [x] Test stream combinators (take, filter_map, fold, etc.)
- [x] Test error propagation through stream
- [x] Test namespaces are separate (global vs local)
- [x] Test hierarchical keys
- [x] Test stress with many entries

---

## Phase 3: Serialization Layer

### 3.1 Bincode Setup ✅ COMPLETE
- [x] Add `bincode` dependency to `rumps-storage/Cargo.toml`
- [x] Create `crates/rumps-storage/src/page.rs` module with `PAGE_SIZE` constant
- [x] Use transparent serde via `#[serde(from = "NodeRaw", into = "NodeRaw")]` on `Node`
  - `NodeRaw` is an intermediate struct without `Arc`s for efficient serialization
  - Derived `Serialize`/`Deserialize` on `NodeRaw` (no custom serde needed)
  - `From<Node> for NodeRaw` and `From<NodeRaw> for Node` conversions
- [x] Configure `PAGE_SIZE` via `RUMPS_PAGE_SIZE` env var at compile time:
  - `build.rs` reads `RUMPS_PAGE_SIZE` (defaults to `4096`)
  - `page.rs` uses `env!("RUMPS_PAGE_SIZE")` for compile-time constant
  - Example: `RUMPS_PAGE_SIZE=8192 cargo build`

**Design Notes**:
- No `SerializeConfig` struct needed; bincode defaults are sufficient
- `bincode::with_limit()` only enforces on serialize, not deserialize from slice
- Page size is compile-time constant (like SQLite, PostgreSQL, MySQL)
- **DoS protection**: Enforced at two layers:
  1. **Serialize**: Phase 4's `FileStorageEngine::write_node()` will check `serialized_size() <= PAGE_SIZE` before writing
  2. **Deserialize**: Phase 4's `FileStorageEngine::read_node()` will read at most `PAGE_SIZE` bytes from disk, bounding the input slice

### 3.2 Node Serialization Format ✅ COMPLETE
- [x] `Node` serializes transparently via `NodeRaw` (derived bincode)
- [x] Keys serialized as `Vec<Key>` (bincode handles length prefixes)
- [x] Values serialized as `Vec<NodeData>` (without `Arc` wrapper)
- [x] Child pointers serialized as `Vec<NodeId>` (page offsets)
- [x] `is_leaf` flag serialized as bool
- [x] `Node::serialized_size()` calculates size via `NodeRaw` conversion
- [x] `Node::would_fit(limit)` checks if node fits in page size

### 3.3 Serialization Tests ✅ COMPLETE
- [x] Test round-trip serialization for leaf nodes (`roundtrip_empty_leaf`, `roundtrip_leaf_with_data`)
- [x] Test round-trip serialization for internal nodes (`roundtrip_internal_with_children`)
- [x] Test serialization of nodes with all Value types (`roundtrip_all_value_types`)
- [x] Test serialized size is reasonable (`serialized_size_reasonable`)
- [x] Test truncated bytes produce error (`truncated_bytes_error`)
- [x] Test `PAGE_SIZE` is power of two and within reasonable range

---

## Phase 4: Disk Persistence with AsyncStorageEngine

**Note**: Only `Name::Global` entries are persisted to disk. `Name::Local` entries remain in memory only and are not serialized.

**Design Principle**: All storage operations are async from the start. The `AsyncStorageEngine` trait abstracts disk operations, allowing the B-tree to remain agnostic about storage details.

### 4.1 Write-Ahead Log (WAL)
- [x] Create `crates/rumps-storage/src/wal.rs` module
- [x] Define WAL record types (`WalRecord` enum):
  - `TxnBegin { txn_id }` / `TxnCommit { txn_id }` / `TxnAbort { txn_id }`
  - `Set { txn_id, name, key, old, new }` - with old `NodeData` for undo
  - `KillEntry { txn_id, name, key, subtree }` - single entry of subtree as `NodeData` for undo
  - `Checkpoint { seq }` - monotonic sequence number
- [x] Define WAL file format (`wal/format.rs`):
  - `FileHeader` (16 bytes): magic `b"RWAL"`, version, flags, first_seq
  - `RecordHeader` (20 bytes): CRC32 checksum, payload length, sequence number, flags
  - Payload: bincode-serialized `WalRecord`
  - CRC32 (IEEE polynomial) for corruption detection
- [x] Implement `WalWriter`:
  - Append records to WAL file
  - Flush/fsync on transaction commit (configurable sync policy)
  - Handle WAL file rotation when size exceeds threshold
- [x] Implement `WalReader`:
  - Read WAL records sequentially
  - Verify checksums
  - Parse records by type

**Architecture: Reader → Writer Lifecycle**

The WAL system enforces a single initialization path:

```
WalReader::open(dir)  →  iterate for recovery  →  reader.into_writer(cfg)
```

This design ensures:

1. **Single source of truth**: The reader is the authority on file state. There's no separate "open for writing" path that might disagree about sequence numbers or file position.

2. **No double-scanning**: The reader tracks position and sequence numbers as it iterates. Converting to a writer reuses this state without re-scanning.

3. **Forced acknowledgment**: Callers must explicitly handle existing WAL records (even if just iterating to EOF) before writing new ones. This prevents accidentally ignoring recovery.

4. **Clear lifecycle**: Read phase (recovery) → Write phase (runtime). No ambiguity about which operations are valid when.

`WalWriter` has no public constructor; it can only be created via `WalReader::into_writer`.

- [x] Add WAL recovery logic (`wal/recovery.rs`):
  - [x] Replay committed transactions on startup via `recover()` / `recover_from_dir()`
  - [x] Handle partial writes (incomplete records return `Ok(None)` at EOF)
  - [x] Track uncommitted transactions (began but never committed/aborted)
  - [x] Filter operations before last checkpoint
- [x] Implement WAL checkpointing:
  - [x] `WalWriter::checkpoint(flushed_seq)` writes checkpoint record and cleans up old files
  - [x] Checkpoint writes `Checkpoint { seq }` record to mark flushed data
  - [x] Rotation after checkpoint isolates post-checkpoint records in new file
  - [x] `cleanup_archived_files()` deletes archived WAL files with `last_seq <= checkpoint_seq`
  - [x] `parse_archived_wal_name()` parses `wal.{first}-{last}.log` format
  - [x] Tests for checkpoint write/rotate, archive cleanup, recovery integration
  - Note: Page flushing is a no-op until PageCache is implemented (Phase 4.2)
- [x] Add tests for WAL write/read round-trip (`reads_single_record`, `reads_multiple_records`, `all_record_types`, `reopen_continues_seq`)
- [x] Add tests for crash recovery scenarios (`partial_write_at_eof_ignored`, `uncommitted_transaction_reported`, `detects_checksum_corruption`, `interleaved_transactions`)

### 4.2 Page-Based Storage
- [x] Define `PAGE_SIZE` constant (done in Phase 3.1 via `page.rs`)
- [x] Create `crates/rumps-storage/src/page.rs` module (done in Phase 3.1)
- [x] Define `PageId` type (u64 offset into file)
- [x] Implement `PageCache` struct:
  - LRU cache of pages in memory
  - Dirty page tracking
  - Flush mechanism
- [x] Implement `PageAllocator`:
  - Track free pages (bitmap or free list)
  - Allocate new pages on demand
  - Reclaim pages on node deletion

### 4.3 AsyncStorageEngine Implementation
- [x] Create `crates/rumps-storage/src/engine.rs` module
- [x] Define `AsyncStorageEngine` trait:
  ```rust
  #[async_trait]
  pub trait AsyncStorageEngine: Send + Sync {
      async fn read(&self, id: NodeId) -> Result<Node>;
      async fn write(&self, id: NodeId, node: &Node) -> Result<()>;
      async fn allocate(&self) -> Result<NodeId>;
      async fn deallocate(&self, id: NodeId) -> Result<()>;
      async fn flush(&self) -> Result<()>;
      async fn metadata(&self) -> StorageMetadata;
  }
  ```
- [x] Implement `FileStorageEngine` struct:
  - `data_file: Arc<RwLock<tokio::fs::File>>` - async file handle
  - `wal: Arc<WalWriter>` - write-ahead log
  - `cache: Arc<PageCache>` - LRU page cache
  - `page_allocator: Arc<PageAllocator>` - free page management
  - `config: StorageConfig` - configuration (page size, cache size, sync mode)
- [x] Implement `FileStorageEngine::open(path: &Path, config: StorageConfig) -> Result<Self>`:
  - Open data file with async I/O
  - Initialize page cache and allocator
  - Open WAL file
  - Run WAL recovery if needed
- [x] Implement `FileStorageEngine::create(path: &Path, config: StorageConfig) -> Result<Self>`
- [x] Implement async storage methods:
  - `async fn read(&self, id: NodeId) -> Result<Node>` - read from disk
  - `async fn write(&self, id: NodeId, node: &Node) -> Result<()>` - write to WAL + cache
  - `async fn allocate(&self) -> Result<NodeId>` - get free page
  - `async fn deallocate(&self, id: NodeId) -> Result<()>` - mark page as free
  - `async fn flush(&self) -> Result<()>` - flush dirty pages to disk
#### 4.3.1 Superblock, Metadata Page, and Global Registry
- [x] Define `Superblock` struct in `engine.rs`:
- [x] Define `MetadataPage` struct for database configuration:
- [x] Define `GlobalRegistry` struct for global name → root mappings:
- [x] Implement `Superblock::serialize() -> [u8; PAGE_SIZE]` with CRC32 checksum
- [x] Implement `Superblock::deserialize(&[u8]) -> Result<Self>` with checksum validation
- [x] Update `PageAllocator` to track multiple reserved pages (not just page 0)
- [x] Add `PageAllocator::extend_capacity(bits: usize)` for bitmap growth
- [x] Add `PageAllocator::reserved_pages() -> Vec<PageId>` accessor
- [x] Update `FileStorageEngine::create()`:
  - Create superblock with initial bitmap page at page 1
  - Create metadata page at page 2, registry page at page 3
  - Mark pages 0-3 as reserved/allocated
  - Update superblock with `metadata_root` and `registry_root` pointers
- [x] Update `FileStorageEngine::open()`:
  - Read and validate superblock
  - Load all bitmap pages, concatenate into `PageAllocator`
  - Load metadata page and validate runtime compatibility
  - Load registry page
- [x] Update `FileStorageEngine::allocate()`:
  - `ensure_bitmap_capacity()` checks if growth needed before allocation
  - `grow_bitmap()` allocates new bitmap page if capacity exhausted
- [x] Update `FileStorageEngine::flush()`:
  - `flush_metadata()` writes all bitmap pages
  - Updates and writes superblock with checksum
- [x] Add tests for:
  - Superblock serialization round-trip (`superblock_serialize_deserialize_roundtrip`)
  - Adding bitmap pages (`superblock_add_bitmap_page`)
  - Invalid magic fails (`superblock_invalid_magic_fails`)
  - Checksum validation failure (`superblock_checksum_mismatch_fails`)
  - Superblock created on `create()` (`create_uses_superblock`)
  - Superblock preserved across open/close (`create_then_open_preserves_superblock`)
  - Reserved pages for superblock and bitmap (`allocate_reserves_superblock_and_bitmap`)

### 4.3.2 Scalable Storage: Indirect Bitmaps and Registry Chaining

**Problem**: Current design limits database to ~62.5 GiB and ~200-370 globals.

| Constraint   | Current Limit | Target (SQLite-level) |
|--------------|---------------|-----------------------|
| Max DB size  | 62.5 GiB      | ~32 TiB               |
| Max globals  | ~200-370      | Unlimited (chained)   |
| Bitmap pages | 500 direct    | 400 direct + indirect |

**Note**: Superblock version remains **1** (not yet released, can overwrite format).

#### 4.3.2.1 Indirect Bitmap Pages

The superblock currently stores up to 500 direct bitmap page IDs. Each bitmap page
tracks `PAGE_SIZE × 8` pages (32,768 at 4KB). This gives `500 × 32,768 = 16.4M pages = 62.5 GiB`.

To reach SQLite-level capacity (~16-32 TiB), we use need to use filesystem-style indirect blocks.

**Implementation Tasks:**

- [x] Update `Superblock` struct (**still version 1**):
- [x] Update `Superblock` constants:
  - `MAX_DIRECT_BITMAP_PAGES = 400` (reduced from 500 to make room)
  - `INDIRECT_ENTRIES_PER_PAGE = PAGE_SIZE / 8 = 512`
  - `OFF_SINGLE_INDIRECT = 3232`
  - `OFF_DOUBLE_INDIRECT = 3240`
  - `OFF_METADATA_ROOT = 3248`
  - `OFF_REGISTRY_ROOT = 3256`
  - `OFF_CHECKSUM = 4080` (moved from 4088)
- [x] Update `Superblock::serialize()` / `deserialize()` for new layout
- [x] Add `IndirectPage` struct:
- [x] Implement `IndirectPage::serialize()` / `deserialize()`
- [x] Update `FileStorageEngine::load_superblock()`:
  - Load direct bitmap pages as before
  - If `single_indirect.is_some()`: load indirect page, then load all referenced bitmap pages
  - If `double_indirect.is_some()`: load double-indirect, then each indirect, then bitmap pages
  - Concatenate all bitmap data for `PageAllocator`
- [x] Update `FileStorageEngine::grow_bitmap()`:
  - If direct slots available: add to `direct_bitmap_ids`
  - Else if single-indirect has room: add to single-indirect page
  - Else if double-indirect has room: add to double-indirect chain
  - Else: return `Err(StorageError::MaxBitmapCapacity)`
- [x] Add helper: `resolve_bitmap_pages(&self) -> Result<Vec<PageId>>`:
  - Returns all bitmap page IDs in order (direct + indirect + double-indirect)
  - Used by both `load_superblock` and `flush_metadata`
  - Implemented as `collect_all_bitmap_ids()` and `resolve_bitmap_pages_full()`
- [x] Update `FileStorageEngine::flush_metadata()`:
  - Write all bitmap pages
  - Write indirect pages if used
  - Write updated superblock
- [x] Add tests:
  - [x] Direct bitmap allocation (existing tests still pass)
  - [x] `collect_all_bitmap_ids()` returns correct order (3 tests)
  - [x] `IndirectPage` serialization/deserialization (5 tests)
  - [x] `Superblock` with indirect pointers (4 tests)
  - [x] Single-indirect allocation + round-trip (`#[ignore]` - ~50GB, run manually)

#### 4.3.2.2 GlobalRegistry Chaining ✅ COMPLETE

The `GlobalRegistry` has a `next_page` field for chaining when a single page
cannot hold all global entries. Each page can hold ~290 entries with short names.

**Architecture: On-Disk vs In-Memory Representation**

The `next_page` field is **only used for on-disk serialization**. Callers never
follow the chain manually—the `FileStorageEngine` abstracts this away entirely:

```text
On-Disk Format:                         In-Memory (`FileStorageEngine`):
┌──────────────────┐                    ┌─────────────────────────────────────┐
│ Registry Page 3  │                    │ registry_chain: Vec<(PageId, Reg)>  │
│  entries: [...]  │                    │                                     │
│  next_page: 42 ──┼───┐                │  [(3, Reg{entries, next:42}),       │
└──────────────────┘   │                │   (42, Reg{entries, next:99}),      │
                       ▼                │   (99, Reg{entries, next:None})]    │
┌──────────────────┐                    │                                     │
│ Registry Page 42 │                    └─────────────────────────────────────┘
│  entries: [...]  │
│  next_page: 99 ──┼───┐                Callers use these methods (chain-unaware):
└──────────────────┘   │                • registry_get(name) → Option<PageId>
                       ▼                • registry_insert(name, root) → Result<()>
┌──────────────────┐                    • registry_remove(name)
│ Registry Page 99 │                    • registry_entries() → Vec<(name, root)>
│  entries: [...]  │
│  next_page: None │
└──────────────────┘
```

**Lifecycle:**
1. **Startup** (`open()`): `load_registry_chain()` follows all `next_page` links,
   building the in-memory `Vec<(PageId, GlobalRegistry)>`
2. **Runtime**: All operations work on the `Vec`—no disk I/O, no link-following
3. **Flush** (`flush()`): `flush_registry_chain()` writes all pages back to disk
   with correct `next_page` pointers reconstructed from the `Vec` order

**Implementation Summary:**

- [x] Added `GlobalRegistry` helper methods:
  - `entry_size()`, `used_bytes()`, `can_insert()`, `is_empty()`, `len()`
  - `insert_unchecked()` for internal use when space is guaranteed
  - `iter()` for iterating entries in a single page
- [x] Updated `FileStorageEngine` struct:
  - Changed `registry: RwLock<GlobalRegistry>` to `registry_chain: RwLock<Vec<(PageId, GlobalRegistry)>>`
- [x] Implemented `load_registry_chain()`:
  - Recursive async function that follows `next_page` links
  - Returns complete chain as `Vec<(PageId, GlobalRegistry)>`
- [x] Implemented `registry_insert()`:
  - Searches for existing entry (update case) or first page with space
  - Calls `registry_chain_extend()` when all pages are full
- [x] Implemented `registry_chain_extend()`:
  - Allocates new page (not marked as reserved, so it can be freed on compaction)
  - Updates previous tail's `next_page` pointer
  - Appends new page to chain
- [x] Implemented `registry_get()`:
  - Searches all pages in chain using `find_map`
- [x] Implemented `registry_remove()`:
  - Removes entry from correct page
  - Compacts empty pages (except first page) by unlinking and freeing
- [x] Implemented `registry_entries()`:
  - Returns all `(name, root)` pairs across the chain
- [x] Implemented `flush_registry_chain()`:
  - Writes all registry pages to disk
  - Called as part of `flush()`
- [x] Added tests (14 new tests):
  - Single page operations: insert/get, update, remove, iteration
  - Persist and reopen
  - Chain overflow to second page
  - Persist multiple pages and reopen
  - Remove from second page
  - Compact empty page
  - Stress test: 1000+ globals
  - Stress test: persist and reopen with 500+ globals

#### 4.3.2.3 Migration Notes

Since we haven't released, no migration is needed. The superblock format simply changes:
- Version stays at 1
- Layout changes (fewer direct slots, add indirect pointers)
- There are no test databases to recreate

### 4.3.3 AsyncStorageEngine Implementation (Part 2)
- [ ] Add WAL-aware methods:
  - `async fn begin_transaction() -> TransactionId`
  - `async fn log_operation(txn_id, operation)` - append to WAL
  - `async fn commit_transaction(txn_id)` - write commit record, call `wal.sync()` based on `SyncMode`
  - `async fn abort_transaction(txn_id)` - write abort record
- [ ] Integrate WAL sync based on `SyncMode`:
  - `SyncMode::Immediate` - `sync()` called after every `append()`
  - `SyncMode::OnCommit` (default) - `sync()` called in `commit_transaction()` after commit record
  - `SyncMode::Periodic(Duration)` - spawn background task that calls `sync()` at interval

### 4.4 Global Management
- [x] Define `GlobalRegistry` struct:
  - Map from global name strings to root `PageId` (only persists `Name::Global`)
  - Stored in separate registry page (page 3 by default), with chaining support
- [x] Implement `GlobalRegistry::serialize/deserialize`
- [x] Implement `GlobalRegistry::insert(name: String, root: PageId)` (register/update)
- [x] Implement `GlobalRegistry::get(name: &str) -> Option<PageId>`
- [x] Implement `GlobalRegistry::remove(name: &str)`
- [x] Integrate with `Superblock` - added `registry_root: Option<PageId>` field
- [x] Add tests for registry serialization round-trip

### 4.5 BTree Refactor & Database Layer

**Architecture Decision**: `BTree` operates purely on `NodeId`s—it has no knowledge
of variable names or the registry. Namespace management (name→root mapping) is
handled by `Database`. See `TODOS/btree-root-architecture-options.md` for rationale.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│  Database                       BTree                    FileStorageEngine  │
│  ────────                       ─────                    ─────────────────  │
│                                                                             │
│  roots: BTreeMap<Name, NodeId>  nodes: HashMap<NodeId, Node>   PageCache    │
│  (lazy-loaded from registry)    (no names, no roots!)          WAL          │
│         │                              │                       registry     │
│         │ lookup/create root           │ load/save nodes                    │
│         ▼                              ▼                                    │
│    ┌─────────┐                   ┌───────────┐                              │
│    │ NodeId  │ ───────────────── │  BTree    │ ◄────────── storage.read()   │
│    └─────────┘                   │  methods  │ ─────────── storage.write()  │
│                                  └───────────┘                              │
│                                                                             │
│  Database calls:                 BTree methods (root-based):                │
│  • registry_get(name)            • get_at(root, key)                        │
│  • registry_insert(name, root)   • set_at(root, key, val)                   │
│  • registry_remove(name)         • kill_at(root, key)                       │
│                                  • create_tree() → NodeId                   │
│                                  • delete_tree(root)                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

#### 4.5.1 Create Database Layer (namespace management only)

**New file**: `crates/rumps-storage/src/database.rs`

The `Database` layer owns namespace management with **lazy-loading** from registry.
Transaction support and full MUMPS operations are deferred to Phase 5.

```rust
pub struct Database {
    /// Name → root NodeId mapping (lazy-loaded cache).
    roots: RwLock<BTreeMap<Name, NodeId>>,

    /// The underlying B-tree (operates on NodeIds only).
    btree: Arc<BTree>,

    /// Optional persistent storage (None = in-memory only).
    storage: Option<Arc<FileStorageEngine>>,
}
```

- [x] Create `database.rs` module with `Database` struct
- [x] Implement `Database::in_memory() -> Result<Self>`:
  - Create `BTree` with `IncrementingAllocator`
  - Empty `roots` map, no storage
- [x] Implement lazy root loading (core namespace logic):
  ```rust
  impl Database {
      /// Look up root, lazy-loading from registry if needed.
      async fn get_root(&self, name: &Name) -> Result<Option<NodeId>> {
          // 1. Check in-memory cache
          // 2. If Global + storage: lazy-load via registry_get()
          // 3. Cache result for future lookups
      }

      /// Get or create root for a name (for SET operations).
      async fn ensure_root(&self, name: &Name) -> Result<NodeId> {
          // 1. Try get_root()
          // 2. If None: btree.create_tree(), cache, registry_insert() if Global
      }

      /// Remove a root (for KILL entire variable).
      async fn remove_root(&self, name: &Name) -> Result<()> {
          // 1. Remove from cache
          // 2. If Global + storage: registry_remove()
      }
  }
  ```

#### 4.5.2 Refactor BTree API (remove roots, use root-based methods)

- [x] Remove `roots: RwLock<BTreeMap<Name, NodeId>>` field from `BTree`
- [x] Rename all public methods from name-based to root-based:

  | Current Method | New Method | Notes |
  |----------------|------------|-------|
  | `get(&Name, &Key)` | `get_at(NodeId, &Key)` | Remove name lookup |
  | `set(&Name, &Key, Value, &Ctx)` | `set_at(NodeId, &Key, Value, &Ctx)` | Remove name lookup |
  | `kill(&Name, &Key, &Ctx)` | `kill_at(NodeId, &Key, &Ctx)` | Remove name lookup |
  | `data(&Name, &Key)` | `data_at(NodeId, &Key)` | Remove name lookup |
  | `order(&Name, Option<&Key>)` | `order_at(NodeId, Option<&Key>)` | Remove name lookup |
  | `collects(...)` | `collects_at(...)` | Remove name lookup |

- [x] Add tree lifecycle methods:
  ```rust
  impl BTree {
      /// Create a new empty tree, returning its root NodeId.
      pub async fn create_tree(&self) -> Result<NodeId>;

      /// Delete an entire tree, deallocating all nodes. Returns node count.
      pub async fn delete_tree(&self, root: NodeId) -> Result<usize>;
  }
  ```
- [x] Remove helper methods that reference `roots`:
  - `get_or_create_root()` → DELETE (never existed)
  - Any method accessing `self.roots` → refactor or delete (none found)

#### 4.5.3 Rewrite BTree Tests

All ~90 BTree tests must be updated to use the new root-based API:

```rust
// BEFORE (name-based)
let btree = BTree::new(3).unwrap();
let name = Name::Global("TEST".into());
btree.set(&name, &key, val, &ctx).await?;
let v = btree.get(&name, &key).await?;

// AFTER (root-based)
let btree = BTree::new(3).unwrap();
let root = btree.create_tree().await?;
btree.set_at(root, &key, val, &ctx).await?;
let v = btree.get_at(root, &key).await?;
```

Tests that move to `Database` layer:
- Namespace separation (`Global` vs `Local`)
- Registry persistence tests

#### 4.5.4 Add Database Tests (namespace management only) ✅ COMPLETE

- [x] Test `in_memory()` creates empty Database - `in_memory_creates_empty_database`
- [x] Test `get_root()` returns `None` for unknown names - `get_root_returns_none_for_unknown`
- [x] Test `ensure_root()` creates tree and caches root - `ensure_root_creates_and_caches`
- [x] Test `ensure_root()` returns cached root on second call - `ensure_root_returns_cached_on_second_call`
- [x] Test `remove_root()` removes from cache - `remove_root_removes_from_cache`
- [x] Test `Global("X")` and `Local("X")` have separate roots - `global_and_local_have_separate_roots`
- [x] Test `update_root()` changes mapping - `update_root_changes_mapping`

Full MUMPS operation tests (get/set/kill/data/order) are in Phase 5.

#### 4.5.5 Rewrite BTree Benchmarks ✅ COMPLETE

The existing benchmarks use root-based API:

- [x] Update `benches/btree_bench.rs` to use root-based API
- [x] Remove `Name` from benchmark setup (uses `NodeId` directly)
- [x] Add new benchmark for `create_tree()` / `delete_tree()` lifecycle

**Benchmarks implemented** (in `btree/tests.rs` → `benches::run_benchmarks`):
- `btree_create_delete_tree` - Tree lifecycle operations
- `btree_set_at_sequential` - Sequential SET with 100 keys
- `btree_get_at_existing` - GET on existing key
- `btree_delete_tree_populated` - Delete tree with 50 keys

### 4.6 Database Layer Persistence & WAL Integration

**Architecture Decision**: WAL logging happens at the `Database` layer, NOT `BTree`.
The `BTree` operates purely in-memory and only marks pages dirty. The `Database` layer:
- Logs logical operations (SET, KILL) to WAL with variable `Name` context
- Coordinates WAL sync and page flush on commit
- Handles crash recovery by replaying WAL records

```text
┌─────────────────────────────────────────────────────────────────────────┐
│ Database Layer (Name-aware, logical operations)                        │
│ • set(name, key, val):                                                  │
│   1. Get old value via btree.get_internal(root, key)                   │
│   2. Log WalRecord::Set {name, key, old, new} to WAL (write-ahead!)    │
│   3. Call btree.set_at(root, key, val, &ctx)                           │
│   4. Update root if changed                                            │
│                                                                         │
│ • kill(name, key):                                                      │
│   1. Collect all entries to delete via btree.collects_at()             │
│   2. Log WalRecord::KillEntry {name, key, data} for each (write-ahead!)│
│   3. Call btree.kill_at(root, key, &ctx)                               │
│   4. Update or remove root                                             │
│                                                                         │
│ • flush():                                                              │
│   1. Log WalRecord::TxnCommit {txn_id} to WAL                          │
│   2. Call storage.wal_sync() (durability!)                             │
│   3. Call storage.flush() to write dirty pages                         │
└───────────────────────────┬─────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ BTree Layer (NodeId-only, in-memory tree operations)                   │
│ • set_at(root, key, val, &ctx):                                         │
│   - Modifies in-memory tree structure                                   │
│   - Calls save_node() for modified nodes                                │
│                                                                         │
│ • save_node(id, node):                                                  │
│   - Updates self.nodes in-memory cache                                  │
│   - Calls storage.mark_dirty(id, node) if storage exists                │
│   - NEVER writes to disk or WAL directly                                │
└───────────────────────────┬─────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────────────────────────────┐
│ Storage Layer (Physical persistence)                                   │
│ • mark_dirty(id, node): Updates cache, marks dirty                     │
│ • wal_append(record): Appends logical record to WAL                    │
│ • wal_sync(): Syncs WAL to disk (fsync)                                │
│ • flush(): Writes all dirty pages to data file                         │
└─────────────────────────────────────────────────────────────────────────┘
```

#### 4.6.1 BTree Persistence (In-Memory + Mark Dirty Only) ✅ COMPLETE

- [x] Add `storage: Option<Arc<dyn AsyncStorageEngine>>` field to `BTree`
- [x] Implement `BTree::with_storage(min_degree, storage) -> Result<Self>`:
  - Initialize with storage engine
  - Use `DiskNodeAllocator` (delegates to storage)
  - **Note**: Does NOT load roots or write to disk
- [x] Update node access methods:
  - [x] `load_node(&self, id: NodeId) -> Result<Node>`:
    - Check `nodes` cache first
    - Load from `storage.read(id)` if cache miss
    - Add to cache (LRU eviction handled by storage layer)
  - [x] `save_node(&self, id: NodeId, node: Node) -> Result<()>`:
    - Update in-memory `nodes` cache
    - Call `storage.mark_dirty(id, node)` (NOT `storage.write()`)
    - **Does NOT write to disk or WAL** - just marks dirty
- [x] Make `get_internal()` pub(crate) so Database can read old values for WAL

#### 4.6.2 Storage Engine WAL Interface ✅ COMPLETE

- [x] Rename `AsyncStorageEngine::write()` → `mark_dirty()`:
  - Update signature and documentation
  - Clarify that it only marks cache entry dirty
  - Evicted dirty pages written to disk during eviction
- [x] Add `FileStorageEngine::wal_append(&WalRecord) -> Result<WalSequence>`:
  - Called by Database to log logical operations
  - Returns sequence number for the appended record
- [x] Add `FileStorageEngine::wal_sync() -> Result<()>`:
  - Called by Database during flush/commit
  - Ensures WAL is durably written (fsync)

#### 4.6.3 Database Layer MUMPS Operations with WAL ✅ COMPLETE

- [x] Implement `Database::create(path)` - creates new persistent database
- [x] Implement `Database::open(path)` - opens existing database with recovery
- [x] Add `Database::recover(&self, path)` - internal recovery method:
  - Call `WalReader::open(&wal_dir).recover()`
  - Replay committed operations from `recovery.committed_ops`
  - For each `WalOp::Set`: apply via `btree.set_at()`, update root
  - For each `WalOp::KillEntry`: apply via `btree.kill_at()`, update root
- [x] Implement `Database::set(name, key, val)`:
  - Get old value via `btree.get_internal()` for undo log
  - Log `WalRecord::Set {txn_id, name, key, old, new}` to WAL (write-ahead!)
  - Call `btree.set_at(root, key, val, &ctx)` to modify tree
  - Update root if changed
- [x] Implement `Database::kill(name, key)`:
  - Collect all entries to delete via `btree.collects_at()`
  - Log `WalRecord::KillEntry {txn_id, name, key, data}` for each entry
  - Call `btree.kill_at(root, key, &ctx)` to delete subtree
  - Update or remove root
- [x] Implement `Database::get(name, key)` - read-only, no WAL
- [x] Implement `Database::data(name, key)` - read-only, no WAL
- [x] Implement `Database::order(name, after)` - read-only, no WAL
- [x] Implement `Database::collects(name, start, pred, extract)` - read-only, no WAL
- [x] Update `Database::flush()`:
  - Log `WalRecord::TxnCommit {txn_id}` to WAL
  - Call `storage.wal_sync()` to ensure durability
  - Call `storage.flush()` to write dirty pages to disk
- [x] Implement `Database::close(self)` - flush and close storage

**Note**: All operations currently use `TransactionId::IMPLICIT` - Phase 5 will add
proper multi-transaction support with user-provided transaction contexts.

#### 4.6.4 Integration Tests ✅ COMPLETE

- [x] Test `Database::create()` and `Database::open()` with persistence
  - `disk_persistence_create_and_reopen` - basic create/reopen cycle
  - `disk_persistence_multiple_globals` - multiple globals persist correctly
- [x] Test full CRUD cycle with WAL logging:
  - `database_operations_with_wal` - comprehensive test:
    - SET multiple keys with WAL logging
    - GET verifies values
    - DATA checks node status
    - KILL removes subtree with WAL logging
    - flush() writes commit + syncs WAL
    - Reopen and verify persistence via WAL recovery

---

## Phase 5: Multi-Transaction Support & Isolation

**Prerequisites**:
- Phase 4.6 is complete with `Database` providing MUMPS operations (`get`, `set`, `kill`,
  `data`, `order`, `collects`)
- WAL logging with `TransactionId::IMPLICIT` for all writes
- WAL recovery replays committed operations on `open()`
- `BTree` uses root-based methods and marks pages dirty

**What Phase 5 Adds**: Replace the single implicit transaction with proper multi-transaction
support including:
- Concurrent transactions with snapshot isolation
- Write buffering and conflict detection
- Transaction API: `db.transaction(|txn| async { ... })`
- Explicit transaction contexts replacing `TransactionId::IMPLICIT`

**Transaction Model**: ALL writes to globals must occur within explicit transactions.
Locals can be modified freely outside transactions.

**Concurrency Model**: The public API will be async with snapshot isolation for reads
and exclusive locks for transaction commits.

### Two-Layer Public API Architecture

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                         User Code                                       │
├─────────────────────────────────────────────────────────────────────────┤
│  db.transaction(|txn| async {                                           │
│      txn.set(&global, &key, val).await?;  // ← Transaction methods      │
│      txn.get(&global, &key).await?;                                     │
│  })                                                                     │
│                                                                         │
│  db.get(&local, &key).await?;             // ← Database methods         │
│  db.set(&local, &key, val).await?;        //   (locals only for writes) │
│                                           //   (returns err for global) │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Database (from Phase 4.6, extended here)                               │
│  ────────────────────────────────────────                               │
│  • Owns namespace: roots map + lazy-loading from registry               │
│  • Holds Arc<BTree> + Arc<TransactionManager>                           │
│  • Provides transaction closure API: db.transaction(...)                │
│  • Direct methods for reads (any namespace) and local writes            │
│  • Rejects global writes outside transactions                           │
│                                                                         │
│  From Phase 4.6:               Added in Phase 5:                        │
│  • get(), set(), kill()        • transaction(), transaction_with()      │
│  • data(), order(), collects() • TransactionManager integration         │
│  • flush() with WAL logging    • Reject global writes outside txn       │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Transaction                                                            │
│  ───────────                                                            │
│  • Used inside db.transaction(|txn| ...) closures                       │
│  • Buffers writes until commit                                          │
│  • Provides snapshot isolation for reads                                │
│  • Tracks read/write sets for conflict detection                        │
│                                                                         │
│  Methods (same names as Database, different semantics):                 │
│  • get() → check write buffer first, then snapshot                      │
│  • set() → buffer write, don't apply yet                                │
│  • kill() → buffer deletion, track subtree                              │
│  • data(), order(), collects() → use snapshot + write buffer            │
│  • commit() → validate, write WAL, apply buffered writes                │
│  • rollback() → discard write buffer                                    │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  BTree (refactored in Phase 4.5)                                        │
│  ───────────────────────────────                                        │
│  • Operates on NodeIds only—no Name awareness                           │
│  • All methods accept TransactionContext parameter                      │
│                                                                         │
│  Methods (root-based):                                                  │
│  • get_at(root, key, Option<&ctx>)                                      │
│  • set_at(root, key, value, &ctx)                                       │
│  • kill_at(root, key, &ctx)                                             │
│  • data_at(root, key, Option<&ctx>)                                     │
│  • order_at(root, Option<key>, Option<&ctx>)                            │
│  • collects_at(root, start, predicate, extract, Option<&ctx>)           │
│  • create_tree() → NodeId                                               │
│  • delete_tree(root)                                                    │
└─────────────────────────────────────────────────────────────────────────┘
```

**Key Insight**: `Transaction` and `Database` expose the same method names (`get`,
`set`, `kill`, etc.), but:
- `Database` methods are for **direct access** (reads anywhere, writes to locals only)
- `Transaction` methods are for **transactional access** (buffers writes, provides isolation)

Both use `Database.get_root()`/`ensure_root()` for namespace resolution, then delegate
to `BTree.*_at()` methods with different `TransactionContext` configurations.

### 5.1 Transaction Infrastructure
- [ ] Add `tokio` dependency to `rumps-storage/Cargo.toml`
- [ ] Create `crates/rumps-storage/src/transaction.rs` module
- [ ] Define `TransactionBuilder` struct:
  ```rust
  pub struct TransactionBuilder {
      isolation: IsolationLevel,
      conflict_strategy: ConflictStrategy,
      timeout: Option<u64>, // in ms
      priority: TransactionPriority,
      retry_count: u32,
  }
  
  impl Default for TransactionBuilder { 
    // defaults
  }

  impl TransactionBuilder {
      pub async fn begin(&self, db: &Database) -> Result<Transaction> {
          // Purpose: Creates and initializes a new transaction with the configured settings.
          //
          // What it does:
          // 1. Generates a unique TransactionId (monotonically increasing or UUID)
          // 2. Captures the current database timestamp for snapshot isolation
          // 3. Takes a read-only snapshot of the database state at this moment
          //    - For SnapshotIsolation: All reads will see data as of this timestamp
          //    - For ReadCommitted: Snapshot updated on each read
          //    - For Serializable: Tracks all reads/writes for conflict detection
          // 4. Initializes empty write buffer for staging changes
          // 5. Registers transaction with the database's TransactionManager
          //    - Allows coordination with other concurrent transactions
          //    - Enables deadlock detection and priority scheduling
          // 6. Starts optional timeout timer if configured
          // 7. Logs transaction start to WAL (for recovery tracking)
          // 8. Returns Transaction struct in Active state
          //
          // The returned Transaction holds:
          // - A clone of the Database (via db.clone(), which is cheap since Database contains Arc fields)
          // - Its unique ID and timestamp
          // - The configuration from this builder
          // - Empty write buffer ready for operations
          // - Snapshot view for consistent reads
          //
          // Note: Calls db.clone() to store in Transaction, which is cheap due to Arc fields
      }

      pub fn isolation(mut self, level: IsolationLevel) -> Self { /* ... */ }
      pub fn conflict(mut self, strategy: ConflictStrategy) -> Self { /* ... */ }
      pub fn timeout(mut self, ms: u64) -> Self { /* ... */ }
      pub fn priority(mut self, priority: TransactionPriority) -> Self { /* ... */ }
      pub fn retries(mut self, count: u32) -> Self { /* ... */ }
  }
  ```
- [ ] Define transaction-related enums:
  ```rust
  pub enum IsolationLevel {
      ReadCommitted,
      SnapshotIsolation, // default
      Serializable,
  }

  pub enum ConflictStrategy {
      Abort,     // default - fail on conflict
      Retry(u32), // retry N times
      Skip,      // skip transaction on conflict
      Overwrite, // last-write-wins
  }

  pub enum TransactionPriority {
      Low,
      Normal,  // default
      High,
  }
  ```
- [ ] Define `Transaction` struct:
  ```rust
  pub struct Transaction {
      // Identity & Lifecycle
      id: TransactionId,                           // Unique identifier for this transaction
      state: RwLock<TransactionState>,            // Active, Committed, or Aborted
      start_timestamp: TransactionTimestamp,       // Snapshot timestamp for isolation

      // Database Reference
      db: Database,                               // Database (cheap to clone via Arc fields)

      // Configuration (from builder)
      isolation: IsolationLevel,                  // Determines read/write behavior
      conflict_strategy: ConflictStrategy,        // How to handle conflicts at commit
      priority: TransactionPriority,              // For scheduling and deadlock resolution
      timeout: Option<Instant>,                   // Deadline for transaction completion
      retry_count: u32,                           // Remaining retries on conflict

      // Write Buffering
      writes: RwLock<HashMap<(Name, Key), WriteOp>>,  // Buffered write operations
      deleted_subtrees: RwLock<HashSet<(Name, Key)>>, // Tracks KILL operations

      // Read Tracking (for conflict detection)
      read_set: RwLock<HashSet<(Name, Key)>>,    // Keys read (for Serializable isolation)

      // Snapshot Data
      snapshot: Arc<Snapshot>,                    // Immutable view of DB at start_timestamp

      // Metrics
      ops_count: AtomicU64,                       // Number of operations performed
      start_time: Instant,                        // Wall clock time when started
  }

  // Write operation types for the write buffer
  enum WriteOp {
      Set(NodeData),      // SET operation with new value
      Delete,             // DELETE single key
      KillSubtree,        // KILL entire subtree
  }

  // Snapshot represents a consistent view of the database
  struct Snapshot {
      timestamp: TransactionTimestamp,
      // In practice, might reference:
      // - Immutable B-tree roots at this timestamp (MVCC)
      // - Or a copy-on-write data structure
      // - Or version chains with timestamps
      roots: HashMap<Name, NodeId>,  // Root nodes at snapshot time
  }
  ```

  Key design decisions:
  - Uses `RwLock` for concurrent access to mutable fields
  - Buffers all writes in memory until commit (no partial visibility)
  - Tracks reads for Serializable isolation conflict detection
  - Holds immutable snapshot for consistent reads
  - Includes metrics for monitoring and debugging
- [ ] Implement `Default` for `TransactionBuilder`:
  ```rust
  impl Default for TransactionBuilder {
      fn default() -> Self {
          Self {
              isolation: IsolationLevel::SnapshotIsolation,
              conflict_strategy: ConflictStrategy::Abort,
              timeout: None,
              priority: TransactionPriority::Normal,
              retry_count: 0,
          }
      }
  }
  ```
- [ ] Implement transaction lifecycle methods:
  - `begin()` - create transaction, get snapshot
  - `commit() -> Result<()>` - validate conflicts, then apply buffered writes:
    ```rust
    async fn commit(self) -> Result<()> {
        // 1. Validate no conflicts (check read/write sets against other txns)
        self.validate_no_conflicts()?;

        // 2. Apply all buffered writes by delegating to Database methods
        //    (Database handles WAL logging)
        for ((name, key), write_op) in self.writes {
            match write_op {
                WriteOp::Set(data) => {
                    let val = data.value.unwrap();
                    // Database.set() logs to WAL and calls btree.set_at()
                    self.db.set_with_txn_id(&name, &key, val, self.id).await?;
                }
                WriteOp::KillSubtree => {
                    // Database.kill() logs to WAL and calls btree.kill_at()
                    self.db.kill_with_txn_id(&name, &key, self.id).await?;
                }
                _ => {}
            }
        }

        // 3. Flush: writes TxnCommit record, syncs WAL, flushes dirty pages
        self.db.flush().await?;

        // 4. Unregister transaction from manager
        self.db.transaction_manager.complete(self.id).await?;

        Ok(())
    }
    ```
  - `rollback()` - discard buffered writes, unregister from manager
- [ ] Implement `Transaction` MUMPS operation methods (used inside `db.transaction(|txn| ...)` closures):

  **Delegation pattern**: `Transaction` holds `db: Database` and delegates to it:
  ```
  txn.get(name, key)
    → check txn.writes buffer
    → check txn.deleted_subtrees
    → self.db.get_root(name)  ← namespace resolution via Database
    → self.db.btree.get_at(root, key, ctx)  ← tree operation via BTree
  ```

  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>`:
    - Check `self.writes` buffer first for pending `WriteOp::Set`
    - Check `self.deleted_subtrees` for pending kills (return `None` if deleted)
    - Resolve name→root via `self.db.get_root(name)` (uses snapshot)
    - Delegate to `self.db.btree.get_at(root, key, Some(&self.context))`
    - Track key in `self.read_set` (for Serializable isolation)
  - `async fn set(&mut self, name: &Name, key: &Key, value: Value) -> Result<()>`:
    - Buffer write in `self.writes` as `WriteOp::Set(NodeData { value: Some(value), ... })`
    - Do NOT call `btree.set_at()` yet (deferred until commit)
    - Update `self.ops_count`
  - `async fn kill(&mut self, name: &Name, key: &Key) -> Result<()>`:
    - Buffer deletion in `self.writes` as `WriteOp::KillSubtree`
    - Track in `self.deleted_subtrees` for read consistency
    - Do NOT call `btree.kill_at()` yet (deferred until commit)
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>`:
    - Check write buffer and deleted subtrees first
    - Resolve name→root, fall back to `btree.data_at(root, key, Some(&self.context))`
    - Combine buffered state with snapshot state
  - `async fn order(&self, name: &Name, after: Option<&Key>) -> Result<Option<Key>>`:
    - Must merge snapshot iteration with buffered writes
    - Buffered sets may insert new keys; buffered kills may remove keys
    - Resolve name→root, fall back to `btree.order_at(root, after, Some(&self.context))`
  - `fn collects<P, F, T>(&self, name: &Name, start: Option<&Key>, pred: P, ext: F) -> impl Stream`:
    - Stream must reflect buffered writes + snapshot
    - Resolve name→root, delegates to `btree.collects_at(...)` with buffer overlay
  - **Note**: These methods have the same signatures as `Database` methods but different semantics (buffering vs direct)

### 5.2 Add Multi-Transaction Support to Database

**Note**: `Database` already has MUMPS operations (`get`, `set`, `kill`, etc.) from
Phase 4.6 with WAL logging using `TransactionId::IMPLICIT`. This section adds:
- Multi-transaction coordinator (`TransactionManager`)
- Transaction-based API (`db.transaction(...)`)
- Protection: Reject global writes outside transactions

- [ ] Add `transaction_manager: Arc<TransactionManager>` field to `Database`
- [ ] Implement `Clone` for `Database` (clone Arc fields)
- [ ] Implement `Database::open(path)` and `Database::create(path)`:
  - Open/create `FileStorageEngine`
  - Create `BTree::with_storage(min_degree, storage)`
  - Initialize `TransactionManager`
  - `roots` starts empty (lazy-loaded via `get_root()`)
- [ ] Implement transaction API:
  - Simple default transaction:
    ```rust
    async fn transaction<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction) -> Future<Result<R>>
    {
        let txn = TransactionBuilder::default().begin(self).await?;
        // Auto-commit on Ok, auto-rollback on Err
        // ...
    }
    ```
  - Configured transaction with builder:
    ```rust
    async fn transaction_with<F, R>(&self, builder: TransactionBuilder, f: F) -> Result<R>
    where
        F: FnOnce(&mut Transaction) -> Future<Result<R>>
    {
        let txn = builder.begin(self).await?;
        // Auto-commit on Ok, auto-rollback on Err
        // ...
    }
    ```
  - Direct transaction builder access:
    ```rust
    fn build_transaction(&self) -> TransactionBuilder {
        TransactionBuilder::default()
    }
    ```
  - Example usage - Simple default transaction:
    ```rust
    // Simple default transaction (snapshot isolation, abort on conflict)
    db.transaction(|txn| async move {
        let name = txn.get(&Name::Global("PATIENT".into()), &key).await?;
        txn.set(&Name::Global("PATIENT".into()), &key, new_value).await?;
        Ok(())
    }).await?;
    ```
  - Example usage - Configured transaction:
    ```rust
    // Transaction with retry on conflict
    let builder = db.build_transaction()
        .conflict(ConflictStrategy::Retry(3))
        .timeout(5000);

    db.transaction_with(builder, |txn| async move {
        // Critical operation that may conflict
        let balance = txn.get(&Name::Global("ACCOUNT".into()), &from_key).await?
            .unwrap_or(Value::Integer(0));
        // ... perform transfer ...
        Ok(())
    }).await?;

    // High-priority serializable transaction
    let builder = TransactionBuilder::default()
        .isolation(IsolationLevel::Serializable)
        .priority(TransactionPriority::High);

    db.transaction_with(builder, |txn| async move {
        // Critical financial transaction
        Ok(())
    }).await?;
    ```
- [ ] Update existing `Database` MUMPS operations to enforce transaction requirements:
  - **Read operations** (already implemented in Phase 4.6, no changes needed):
    - `get()`, `data()`, `order()`, `collects()` work as-is
    - Can be called inside or outside transactions
  - **Write operations** (ADD transaction check to existing Phase 4.6 implementations):
    - `async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()>`:
      - **ADD**: If `Name::Global`: check `TransactionManager` for active transaction
        - If no active transaction: return `Err(StorageError::GlobalRequiresTransaction)`
        - If transaction active: proceed with existing Phase 4.6 logic (WAL + set)
      - If `Name::Local`: existing logic works as-is (no WAL, direct set)
    - `async fn kill(&self, name: &Name, key: &Key) -> Result<()>`:
      - **ADD**: If `Name::Global`: check `TransactionManager` for active transaction
        - If no active transaction: return `Err(StorageError::GlobalRequiresTransaction)`
        - If transaction active: proceed with existing Phase 4.6 logic (WAL + kill)
      - If `Name::Local`: existing logic works as-is
  - **Note**: Phase 4.6 already implements WAL logging in `set()`/`kill()`. Phase 5 adds
    the transaction check and replaces `TransactionId::IMPLICIT` with actual transaction IDs
- [ ] Enforce transaction rules:
  - Writes to `Name::Global` MUST be in transaction
  - `Name::Local` modifications work outside transactions
  - GET/DATA/ORDER work with or without transactions

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

### 5.4 Transaction Context Usage (Minimal BTree Changes)

**Architecture Note**: BTree remains simple - it only marks pages dirty. Transaction
buffering happens at the `Transaction` layer, and WAL logging happens at the `Database`
layer. The `TransactionContext` parameter exists primarily for:
1. Tracking which transaction ID made changes (for future MVCC)
2. Potential future snapshot isolation at BTree level

**Current Phase 5 Scope**: BTree keeps its Phase 4.6 behavior:
- `set_at()`, `kill_at()` modify tree immediately and mark pages dirty via `storage.mark_dirty()`
- `get_at()`, `data_at()`, `order_at()`, `collects_at()` read committed state
- `TransactionContext` parameter is accepted but minimally used (stores `txn_id` only)

This is sufficient because:
- **Write buffering** happens in `Transaction.writes: HashMap<(Name, Key), WriteOp>`
- **WAL logging** happens in `Database.set()`/`kill()` before calling `btree.*_at()`
- **Snapshot isolation** for reads is handled by `Transaction` checking its write buffer

**Future MVCC Enhancement** (Post-Phase 5):
When full MVCC is added later, BTree will:
- Store version chains in nodes (multiple versions per key)
- Use `ctx.start_timestamp` to select visible version
- Use `ctx.id` to track which transaction created each version

**Phase 5 Tasks**:
- [ ] Verify BTree methods accept `TransactionContext` (already done in Phase 4.6)
- [ ] Add `TransactionManager` to coordinate active transactions
- [ ] Transaction commit flow:
  1. Validate no conflicts (check read/write sets)
  2. For each buffered write in `txn.writes`:
     - Call `db.set(name, key, val)` with `txn.id` → logs to WAL + calls `btree.set_at()`
     - Or `db.kill(name, key)` with `txn.id` → logs to WAL + calls `btree.kill_at()`
  3. Call `db.flush()` → writes commit record + syncs WAL
- [ ] Add tests for transaction semantics:
  - Writes inside transaction are buffered (not visible to other transactions)
  - Reads inside transaction see own buffered writes
  - Commit makes all writes visible atomically
  - Rollback discards all buffered writes
  - Concurrent transactions don't interfere

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

### 6.2b WAL & Recovery Tests (Phase 4.6)
- [x] Basic WAL integration test exists: `database_operations_with_wal`
- [ ] Test WAL recovery after simulated crash (kill process mid-operation)
- [ ] Test WAL recovery with multiple transactions
- [ ] Test WAL checkpointing and archive cleanup
- [ ] Test corrupted WAL file handling
- [ ] Test WAL replay applies operations in correct order
- [ ] Verify dirty pages are written on flush

### 6.2c Transaction Tests (Phase 5)
- [ ] Create `crates/rumps-storage/tests/transaction_tests.rs`
- [ ] Test transaction commit writes to WAL and applies changes
- [ ] Test transaction rollback discards all changes
- [ ] Test that writes outside transaction to globals return error
- [ ] Test that writes to locals work outside transactions
- [ ] Test snapshot isolation (reads see consistent state)
- [ ] Test transaction conflict detection (if applicable)
- [ ] Test WAL recovery replays committed transactions correctly
- [ ] Test WAL recovery ignores aborted transactions

### 6.2d Concurrency Tests
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
- [ ] Add `#![warn(missing_docs)]` to crate roots

### 7.3 Final Validation
- [ ] Run full test suite: `cargo test --workspace`
- [ ] Run benchmarks: `cargo bench --workspace`
- [ ] Build docs: `cargo doc --workspace --no-deps`

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
- **$COLLECT Enhancements**:
  - Parallel processing with `buffer_unordered` for concurrent record processing:
    ```rust
    let results: Vec<_> = btree.collects(...)
        .map(|r| async move { process_record_async(r).await })
        .buffer_unordered(10)
        .try_collect()
        .await?;
    ```
  - Bidirectional iteration via `get_prev_internal` for reverse traversal
  - Batch node reads and read-ahead buffering for performance
  - Range queries with end bounds (inclusive/exclusive)

---

## Progress Tracking

**Status**: In Progress
**Current Phase**: Phase 4.5 BTree Refactor & Database Layer - COMPLETE
**Next Up**: Phase 4.6 BTree Disk Persistence
**Completed Checkboxes**: ~130 / ~180

**Recent Changes** (2025-12-03 - Phase 4.5 BTree Refactor & Database Layer COMPLETE):
- ✅ Phase 4.5.1: Created `Database` layer with namespace management
  - `Database::in_memory()`, `with_btree()` constructors
  - `get_root()`, `ensure_root()`, `remove_root()`, `update_root()` methods
  - Lazy-loading from registry (stub for Phase 4.6)
- ✅ Phase 4.5.2: Refactored BTree to root-based API
  - All methods now use `NodeId` root parameter (`get_at`, `set_at`, `kill_at`, etc.)
  - Added `create_tree()` and `delete_tree()` lifecycle methods
  - Removed all `roots` field and name-based methods from BTree
- ✅ Phase 4.5.3: All BTree tests updated to use root-based API
- ✅ Phase 4.5.4: Added 7 Database tests for namespace management
- ✅ Phase 4.5.5: Rewrote BTree benchmarks with 4 benchmarks using root-based API
- All 318 tests passing, clippy clean

**Recent Changes** (2025-12-03 - Phase 4.3.2.2 GlobalRegistry Chaining COMPLETE):
- ✅ Implemented `GlobalRegistry` chaining for unlimited globals
  - Added helper methods: `entry_size()`, `used_bytes()`, `can_insert()`, `is_empty()`, `len()`, `iter()`
  - Changed `FileStorageEngine.registry` to `registry_chain: Vec<(PageId, GlobalRegistry)>`
  - Implemented `load_registry_chain()` with recursive async following `next_page` links
  - Implemented `registry_insert()` with automatic chain extension when full
  - Implemented `registry_get()` searching across all pages
  - Implemented `registry_remove()` with compaction of empty pages
  - Implemented `registry_entries()` for iterating all entries
  - Implemented `flush_registry_chain()` to persist all pages
- ✅ Added 14 new tests for registry chaining
  - Basic operations (insert/get/update/remove/iteration)
  - Chain overflow and persistence across reopen
  - Page compaction when entries removed
  - Stress tests with 500-1000 globals
- All 420 tests passing, clippy clean

**Recent Changes** (2025-12-03 - Phase 4.4 Global Management COMPLETE):
- ✅ Added `MetadataPage` struct for database configuration
  - Fields: `version`, `created_at`, `page_size`, `min_degree`, `last_checkpoint`
  - Serialize/deserialize with CRC32 checksum and magic validation
  - Runtime validation ensures page size matches compiled binary
- ✅ Added `GlobalRegistry` struct for global name → root page mapping
  - Variable-length entries: (name_len, name_bytes, root_page_id)
  - ~4070 bytes for entries per page (~200 globals at average 20 bytes/entry)
  - Supports chaining via `next_page` pointer (not yet implemented)
  - Methods: `new()`, `get()`, `insert()`, `remove()`, `serialize()`, `deserialize()`
- ✅ Updated `Superblock` with metadata/registry pointers:
  - `metadata_root: Option<PageId>` at offset 4032
  - `registry_root: Option<PageId>` at offset 4040
- ✅ Updated `FileStorageEngine::create()`:
  - Now allocates 4 pages: superblock (0), bitmap (1), metadata (2), registry (3)
- ✅ Updated `FileStorageEngine::open()`:
  - Loads metadata page and validates runtime compatibility
  - Loads registry page (or creates defaults for legacy DBs without these pages)
- ✅ Added 12 new tests for MetadataPage and GlobalRegistry
- All 384 tests passing, clippy clean

**Recent Changes** (2025-12-02 - Phase 4.3.1 Superblock COMPLETE):
- ✅ Implemented Superblock and Multi-Page Bitmap system
  - `Superblock` struct with serialize/deserialize and CRC32 checksum
  - Supports up to 500 bitmap pages (~62 GiB at 4KB page size)
  - `PageAllocator` updated for multiple reserved pages
  - `extend_capacity()`, `add_reserved()`, `reserved_pages()`, `is_reserved()` methods
  - `FileStorageEngine::create()` writes superblock + first bitmap page
  - `ensure_bitmap_capacity()` and `grow_bitmap()` for automatic bitmap growth
  - `flush_metadata()` writes bitmap pages and updates superblock
  - 7 new superblock tests, all 372 tests passing, clippy clean
- ✅ Replaced manual `Bitmap` implementation with `bitvec` crate wrapper
  - Added `bitvec = "1.0"` dependency
  - `Bitmap` is now a transparent newtype: `struct Bitmap(BitVec<u64, Lsb0>)`

**Recent Changes** (2025-11-30 - Phase 4.1 WAL Checkpointing):
- ✅ Implemented WAL checkpointing
  - Added `WalWriter::checkpoint(flushed_seq)` method
  - Writes `Checkpoint { seq }` record to mark flushed data
  - Rotates WAL file after checkpoint (isolates post-checkpoint records)
  - `cleanup_archived_files()` deletes old archived files (`last_seq <= checkpoint_seq`)
  - `parse_archived_wal_name()` parses `wal.{first:016x}-{last:016x}.log` format
  - 8 new tests: parse_archived_wal_name (valid/invalid), checkpoint_writes_record_and_rotates,
    checkpoint_deletes_old_archives, checkpoint_keeps_newer_archives, checkpoint_recovery_integration,
    checkpoint_in_same_file
  - All 278 tests passing, clippy clean
  - Note: Page flushing placeholder until PageCache (Phase 4.2) is implemented
  - Note: Multi-file WAL recovery not yet implemented (recovery reads only current wal.log)

**Recent Changes** (2025-11-28 - Phase 3 Serialization Complete):
- ✅ Completed Phase 3: Serialization Layer
  - Added `bincode` dependency (already present)
  - Created `page.rs` with compile-time `PAGE_SIZE` constant
  - Implemented transparent serde for `Node` via `NodeRaw` intermediate struct
  - `NodeRaw` avoids `Arc` overhead in serialization
  - `#[serde(from = "NodeRaw", into = "NodeRaw")]` provides seamless `bincode::serialize`/`deserialize`
  - `PAGE_SIZE` configurable via `RUMPS_PAGE_SIZE` env var at compile time (default: `4096`)
  - Added `build.rs` to read env var and pass to rustc
  - Simplified from original plan: no `SerializeConfig` needed, bincode defaults sufficient
  - DoS protection comes from bounded page reads, not bincode limits
  - All 221 tests passing

**Recent Changes** (2025-11-27 - Phase 2.6 ORDER Complete):
- ✅ Completed Phase 2.6: MUMPS Operations - ORDER
  - Implemented `order_internal` for finding next key in lexicographic order
  - `find_leftmost_key()` traverses left spine for minimum key
  - `find_successor_key()` navigates B-tree to find successor, handles leaf-to-leaf transitions
  - 15 tests covering empty trees, successors, hierarchical keys, namespaces, collation order
  - Stress tests with 500 keys and deep hierarchies
  - All 181 tests passing, clippy clean

**Recent Changes** (2025-11-27 - Phase 2.5 DATA Complete):
- ✅ Completed Phase 2.5: MUMPS Operations - DATA
  - Implemented `data_internal` using `get_internal` + mapping to `DataStatus`
  - 12 tests covering all four states, namespaces, hierarchy, and state transitions
  - All tests passing, clippy clean

**Recent Changes** (2025-11-27 - Phase 2.4 KILL Complete):
- ✅ Completed Phase 2.4: MUMPS Operations - KILL
  - Implemented `kill_internal` with full B-tree deletion algorithm
  - Handles subtree deletion, ancestor flag updates, and tree rebalancing
  - 35 comprehensive tests covering all edge cases
  - Tests for both Global and Local namespaces
  - Stress tests for rebalancing (merges, borrows, consecutive operations)
  - All tests passing, clippy clean

**Recent Changes** (2025-11-18 - Phase 2.1 Complete):
- ✅ Completed Phase 2.1: B-Tree Structure - Initial Setup
  - Created `crates/rumps-storage/src/error.rs` with comprehensive error handling
  - Created `crates/rumps-storage/src/btree.rs` with full structure and documentation
  - Implemented `BTreeStats`, `NodeAllocator` trait, `IncrementingAllocator`
  - Implemented `BTree` struct with all accessor methods and configuration options
  - Added 12 unit tests including concurrent access tests
  - Updated `src/lib.rs` with module declarations and exports
  - Configured `Cargo.toml` with all dependencies (async-trait, tokio, futures, etc.)
  - All tests passing, clippy clean, documentation complete
  - Added scalability considerations and typical MUMPS deployment pattern documentation

**Recent Changes** (2025-11-16):
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

Last Updated: 2025-12-03
