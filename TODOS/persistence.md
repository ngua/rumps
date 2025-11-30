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
- [ ] Define `PageId` type (u64 offset into file)
- [ ] Implement `PageCache` struct:
  - LRU cache of pages in memory
  - Dirty page tracking
  - Flush mechanism
- [ ] Implement `PageAllocator`:
  - Track free pages (bitmap or free list)
  - Allocate new pages on demand
  - Reclaim pages on node deletion

### 4.3 AsyncStorageEngine Implementation
- [ ] Create `crates/rumps-storage/src/engine.rs` module
- [ ] Define `AsyncStorageEngine` trait:
  ```rust
  #[async_trait]
  pub trait AsyncStorageEngine: Send + Sync {
      async fn read_node(&self, id: NodeId) -> Result<Node>;
      async fn write_node(&self, id: NodeId, node: &Node) -> Result<()>;
      async fn allocate_page(&self) -> Result<NodeId>;
      async fn deallocate_page(&self, id: NodeId) -> Result<()>;
      async fn flush(&self) -> Result<()>;
      async fn metadata(&self) -> StorageMetadata;
  }
  ```
- [ ] Implement `FileStorageEngine` struct:
  - `data_file: Arc<RwLock<tokio::fs::File>>` - async file handle
  - `wal: Arc<WalWriter>` - write-ahead log
  - `cache: Arc<PageCache>` - LRU page cache
  - `page_allocator: Arc<PageAllocator>` - free page management
  - `config: StorageConfig` - configuration (page size, cache size, sync mode)
- [ ] Implement `FileStorageEngine::open(path: &Path, config: StorageConfig) -> Result<Self>`:
  - Open data file with async I/O
  - Initialize page cache and allocator
  - Open WAL file
  - Run WAL recovery if needed
- [ ] Implement `FileStorageEngine::create(path: &Path, config: StorageConfig) -> Result<Self>`
- [ ] Implement async storage methods:
  - `async fn read_node(&self, id: NodeId) -> Result<Node>` - read from disk
  - `async fn write_node(&self, id: NodeId, node: &Node) -> Result<()>` - write to WAL + cache
  - `async fn allocate_page(&self) -> Result<NodeId>` - get free page
  - `async fn deallocate_page(&self, id: NodeId) -> Result<()>` - mark page as free
  - `async fn flush(&self) -> Result<()>` - flush dirty pages to disk
- [ ] Add WAL-aware methods:
  - `async fn begin_transaction() -> TransactionId`
  - `async fn log_operation(txn_id, operation)` - append to WAL
  - `async fn commit_transaction(txn_id)` - write commit record, fsync WAL
  - `async fn abort_transaction(txn_id)` - write abort record

### 4.4 Global Management
- [ ] Define `GlobalRegistry` struct:
  - Map from global name strings to root `PageId` (only persists `Name::Global`)
  - Store in header page (page 0)
- [ ] Implement `GlobalRegistry::register_global(name: String, root: PageId)`
- [ ] Implement `GlobalRegistry::get_root(name: &str) -> Option<PageId>`
- [ ] Serialize/deserialize global registry to/from page 0
- [ ] Add tests for multi-global persistence
- [ ] Add tests verifying Local variables are NOT persisted

### 4.5 BTree Persistence Integration
- [ ] Update `BTree` struct to support disk persistence:
  - Add `storage: Option<Arc<dyn AsyncStorageEngine>>` field
  - Add `DiskNodeAllocator` that delegates to storage engine
- [ ] Implement `BTree::with_storage(min_degree, storage) -> Result<Self>`:
  - Initialize with storage engine
  - Use `DiskNodeAllocator` instead of `IncrementingAllocator`
  - Load root nodes from disk for existing database
- [ ] Update node access methods for cache + disk:
  - `async fn load_node(&self, id: NodeId) -> Result<Node>`:
    - Check cache first (`nodes` HashMap)
    - Load from disk if miss (only for `Name::Global`)
    - Keep `Name::Local` entirely in memory
    - Add to cache with LRU eviction
  - `async fn save_node(&self, id: NodeId, node: Node) -> Result<()>`:
    - Update cache
    - Write to WAL + disk (only for `Name::Global`)
- [ ] Integrate WAL with all write operations:
  - All modifications logged to WAL first
  - Writes marked dirty in page cache
  - Actual disk writes happen on flush/checkpoint
- [ ] Add checkpoint/flush logic:
  - `async fn checkpoint(&self) -> Result<()>` - flush dirty pages
  - Periodic background checkpointing task
  - Write checkpoint record to WAL
- [ ] Add `async fn close(self) -> Result<()>` - flush and close storage

---

## Phase 5: Transaction-Based Public API

**Transaction Model**: ALL writes to globals must occur within explicit transactions. Locals can be modified freely outside transactions.

**Concurrency Model**: The public API will be async with snapshot isolation for reads and exclusive locks for transaction commits.

### Two-Layer Public API Architecture

The public API has **two layers** above the internal `BTree`:

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
│  Layer 1: Database                                                      │
│  ─────────────────                                                      │
│  • Main entry point for users                                           │
│  • Holds Arc<BTree> + Arc<TransactionManager>                           │
│  • Provides transaction closure API: db.transaction(...)                │
│  • Direct methods for reads (any namespace) and local writes            │
│  • Rejects global writes outside transactions                           │
│                                                                         │
│  Methods:                                                               │
│  • get(), data(), order(), collects() → delegate to BTree w/ context    │
│  • set(), kill() → for locals only; globals require Transaction         │
│  • transaction(), transaction_with() → create Transaction scope         │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│  Layer 2: Transaction                                                   │
│  ────────────────────                                                   │
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
│  Internal: BTree                                                        │
│  ───────────────                                                        │
│  • Low-level B-tree operations                                          │
│  • All methods accept TransactionContext parameter                      │
│  • Phase 2 implementations ignore context (added for future-proofing)   │
│  • Phase 5.4 retrofits context usage for isolation/buffering            │
│                                                                         │
│  Methods:                                                               │
│  • set(name, key, value, &ctx)                                          │
│  • get(name, key, Option<&ctx>)                                         │
│  • kill(name, key, &ctx)                                                │
│  • data(name, key, Option<&ctx>)                                        │
│  • order(name, key, Option<&ctx>)                                       │
│  • collects(name, start, predicate, extract, Option<&ctx>)              │
└─────────────────────────────────────────────────────────────────────────┘
```

**Key Insight**: `Transaction` and `Database` expose the same method names (`get`, `set`, `kill`, etc.), but:
- `Database` methods are for **direct access** (reads anywhere, writes to locals only)
- `Transaction` methods are for **transactional access** (buffers writes, provides isolation)

Both ultimately delegate to `BTree` methods, but with different `TransactionContext` configurations.

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
  - `commit() -> Result<()>` - validate, write to WAL, apply changes
  - `rollback()` - discard buffered writes
- [ ] Implement `Transaction` MUMPS operation methods (used inside `db.transaction(|txn| ...)` closures):
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>`:
    - Check `self.writes` buffer first for pending `WriteOp::Set`
    - Check `self.deleted_subtrees` for pending kills (return `None` if deleted)
    - Fall back to `self.db.btree.get(name, key, Some(&self.context))` using snapshot
    - Track key in `self.read_set` (for Serializable isolation)
  - `async fn set(&mut self, name: &Name, key: &Key, value: Value) -> Result<()>`:
    - Buffer write in `self.writes` as `WriteOp::Set(NodeData { value: Some(value), ... })`
    - Do NOT call `BTree::set` yet (deferred until commit)
    - Update `self.ops_count`
  - `async fn kill(&mut self, name: &Name, key: &Key) -> Result<()>`:
    - Buffer deletion in `self.writes` as `WriteOp::KillSubtree`
    - Track in `self.deleted_subtrees` for read consistency
    - Do NOT call `BTree::kill` yet (deferred until commit)
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>`:
    - Check write buffer and deleted subtrees first
    - Fall back to `self.db.btree.data(name, key, Some(&self.context))`
    - Combine buffered state with snapshot state
  - `async fn order(&self, name: &Name, after: Option<&Key>) -> Result<Option<Key>>`:
    - Must merge snapshot iteration with buffered writes
    - Buffered sets may insert new keys; buffered kills may remove keys
    - Fall back to `self.db.btree.order(name, after, Some(&self.context))`
  - `fn collects<P, F, T>(&self, name: &Name, start: Option<&Key>, pred: P, ext: F) -> impl Stream`:
    - Stream must reflect buffered writes + snapshot
    - Delegates to `self.db.btree.collects(...)` with buffer overlay
  - **Note**: These methods have the same signatures as `Database` methods but different semantics (buffering vs direct)

### 5.2 Async Database Handle
- [ ] Create `crates/rumps-storage/src/database.rs` module
- [ ] Define `Database` struct as main entry point:
  ```rust
  pub struct Database {
      btree: Arc<BTree>,
      transaction_manager: Arc<TransactionManager>,
  }

  impl Clone for Database {
      fn clone(&self) -> Self {
          Self {
              btree: Arc::clone(&self.btree),
              transaction_manager: Arc::clone(&self.transaction_manager),
          }
      }
  }
  ```
  - Uses `Arc<BTree>` for thread-safe sharing
  - B-tree handles both `Name::Global` (persistent) and `Name::Local` (ephemeral)
  - Transaction manager coordinates concurrent transactions
  - **Important**: Database implements `Clone` by cloning the Arc fields, making it cheap to clone
  - This pattern allows passing `&Database` to APIs while enabling cheap cloning when needed (e.g., for storing in Transaction)
  - Common pattern in async Rust (similar to `reqwest::Client`, `sqlx::Pool`, etc.)
- [ ] Implement `Database::open(path: &Path) -> Result<Self>`:
  - Create `FileStorageEngine` with config
  - Initialize `BTree::with_storage(min_degree, storage)`
  - Wrap in Arc for sharing
- [ ] Implement `Database::create(path: &Path) -> Result<Self>`
- [ ] Implement `Database::in_memory() -> Result<Self>` for testing
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
- [ ] Implement `Database` MUMPS operation methods (direct access, no write buffering):
  - **Read operations** (work with or without active transaction):
    - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>`:
      - Delegates to `self.btree.get(name, key, None)` (no transaction context)
      - Works for both globals and locals
    - `async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>`:
      - Delegates to `self.btree.data(name, key, None)`
    - `async fn order(&self, name: &Name, after: Option<&Key>) -> Result<Option<Key>>`:
      - Delegates to `self.btree.order(name, after, None)`
    - `fn collects<P, F, T>(&self, ...) -> impl Stream`:
      - Delegates to `self.btree.collects(..., None)`
      - Public `Database` API exposes `&Option<Value>` (hides `NodeData` internals)
  - **Write operations** (locals only; globals require `Transaction`):
    - `async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()>`:
      - If `name` is `Name::Global(...)`: return `Err(StorageError::GlobalRequiresTransaction)`
      - If `name` is `Name::Local(...)`: create ephemeral context, delegate to `self.btree.set(name, key, value, &ctx)`
    - `async fn kill(&self, name: &Name, key: &Key) -> Result<()>`:
      - If `name` is `Name::Global(...)`: return `Err(StorageError::GlobalRequiresTransaction)`
      - If `name` is `Name::Local(...)`: create ephemeral context, delegate to `self.btree.kill(name, key, &ctx)`
  - **Note**: Unlike `Transaction` methods, `Database` methods do NOT buffer writes—they apply immediately (for locals) or reject (for globals)
- [ ] Enforce transaction rules:
  - Writes to `Name::Global` MUST be in transaction (return error otherwise)
  - `Name::Local` modifications work outside transactions
    - Creates temporary transaction context internally for locals
    - Note: Locals still need a transaction context internally (`BTree::set`) but the Database API handles this transparently
  - GET/DATA/ORDER can work with or without transactions

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

### 5.4 Adding Transaction Awareness to B-Tree Primitives

This section covers updating the Phase 2 B-tree primitives (SET, GET, KILL, DATA, ORDER) to use the `TransactionContext` parameter that was added for future-proofing.

**Note**: Phase 2 implementations intentionally ignore `TransactionContext`. This section adds the actual transaction awareness.

- [ ] Update `BTree::set()` to use transaction context:
  - Check transaction isolation level from `ctx.isolation_level`
  - Track write operations in transaction context (for conflict detection)
  - Use transaction timestamp for MVCC ordering (future enhancement)
  - Buffer writes for atomic commit (coordinate with `Transaction` struct)
- [ ] Update `BTree::get()` to use transaction context when provided:
  - Implement snapshot isolation: reads see database state as of `ctx.start_timestamp`
  - Check buffered writes in transaction before reading committed data
  - Return most recent visible version based on transaction timestamp
- [ ] Update `BTree::kill()` to use transaction context:
  - Track deletion operations in transaction context
  - Buffer deletions for atomic commit
  - Update transaction write set
- [ ] Update `BTree::data()` to use transaction context when provided:
  - Apply snapshot isolation to DATA checks
  - Consider buffered writes when determining node status
  - Return status based on transaction's view of data
- [ ] Update `BTree::order()` to use transaction context when provided:
  - Apply snapshot isolation to iteration
  - Skip uncommitted writes from other transactions
  - Include buffered writes from current transaction in iteration order
- [ ] Update `BTree::collects()` to use transaction context when provided:
  - Ensure stream sees consistent snapshot throughout iteration
  - Apply same snapshot isolation rules as individual operations
- [ ] Add tests for transaction isolation:
  - Test that reads within transaction don't see uncommitted writes from other transactions
  - Test that reads within transaction DO see own buffered writes
  - Test that concurrent transactions maintain isolation
- [ ] Add tests for write buffering:
  - Test that writes are buffered, not immediately visible
  - Test that commit makes all writes visible atomically
  - Test that rollback discards all buffered writes

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
**Current Phase**: Phase 4.1 WAL (Checkpointing Complete) - continuing with Phase 4.2+
**Completed Checkboxes**: ~80 / ~160

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

Last Updated: 2025-11-30
