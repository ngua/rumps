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

### 2.2 MUMPS Operations - SET

**Important**: After completing the first two checkboxes below (basic SET implementation), you MUST implement hierarchical semantics by following the complete plan in `TODOS/hierarchy.md`. This includes maintaining `has_descendants` flags on ancestor nodes, which is critical for `$DATA`, `$ORDER`, and `KILL` operations.

- [ ] Implement `async fn set_with_context(&self, name: &Name, key: &Key, value: Value, context: Option<&TransactionContext>) -> Result<()>`:
  - Use `load_node()` for cache-aware node access
  - Navigate to appropriate leaf node
  - Insert/update key-value pair
  - Update parent `has_descendants` flags up the path
  - Handle node splits and tree growth
  - Use `save_node()` to persist changes
  - Update `BTreeStats` (key count, splits)
  - If context is Some, check transaction isolation level and track writes
- [ ] Implement `async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()>`:
  - Simply delegate to `set_with_context(name, key, value, None)`
- [ ] **→ See `TODOS/hierarchy.md` for complete hierarchical semantics implementation** (required before continuing)
- [ ] Add async tests for SET on empty tree (both Global and Local)
- [ ] Add async tests for SET with existing keys (updates)
- [ ] Add async tests for SET triggering node splits
- [ ] Add async tests for SET on multi-level subscripts (e.g., `["A", "B", "C"]`)
- [ ] Add async tests verifying Global and Local namespaces are separate
- [ ] Add concurrent SET tests with `Arc<BTree>`

### 2.3 MUMPS Operations - GET
- [ ] Implement `async fn get_with_context(&self, name: &Name, key: &Key, context: Option<&TransactionContext>) -> Result<Option<Value>>`:
  - Use `get_internal()` to retrieve `Arc<NodeData>` (will use `load_node()` in Phase 4)
  - Extract value by cloning `Option<Value>` from the Arc
  - If context is Some, read from transaction's snapshot timestamp and see only committed values as of transaction start
  - Implementation:
    ```rust
    pub async fn get_with_context(
        &self,
        name: &Name,
        key: &Key,
        _context: Option<&TransactionContext>,
    ) -> Result<Option<Value>> {
        // TODO Phase 5: If context is Some, use transaction's snapshot isolation
        // to read from the snapshot timestamp and see only committed values
        // as of transaction start. This will require checking the transaction's
        // read timestamp against the write timestamps of modifications.
        Ok(self.get_internal(name, key).await?.and_then(|arc_data| arc_data.value.clone()))
    }
    ```
- [ ] Implement `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>`:
  - Simply delegate to `get_with_context(name, key, None)`
  - Implementation:
    ```rust
    pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
        self.get_with_context(name, key, None).await
    }
    ```
- [ ] Add async tests for GET on non-existent keys
- [ ] Add async tests for GET on existing keys
- [ ] Add async tests for GET on partial paths (should return None if no value at that node)
- [ ] Add concurrent GET tests during active writes

**Note on Value Cloning**: All read operations return owned `Value` rather than references due to async lock lifetime constraints. Values must be cloned from the `RwLock` guard before it drops. This follows standard patterns in async concurrent data structures (like `dashmap::DashMap`). MUMPS values are typically small, making cloning cost acceptable. The public `get()` API simply clones the `Option<Value>` from the `Arc<NodeData>` returned by `get_internal()`. See `TODOS/btree.md` "Value Cloning and Lock Semantics" section for detailed rationale and future optimization strategies.

### 2.4 MUMPS Operations - KILL
- [ ] Implement `async fn kill_with_context(&self, name: &Name, key: &Key, context: Option<&TransactionContext>) -> Result<()>`:
  - Use `load_node()` for cache-aware node access
  - Navigate to node
  - Delete entire subtree rooted at key
  - Update parent `has_descendants` flags
  - Handle node merging and tree shrinking
  - Use `save_node()` to persist changes
  - Update `BTreeStats` (key count, merges)
  - If context is Some, track deletions in transaction context
- [ ] Implement `async fn kill(&self, name: &Name, key: &Key) -> Result<()>`:
  - Simply delegate to `kill_with_context(name, key, None)`
- [ ] Add async tests for KILL leaf nodes (both Global and Local)
- [ ] Add async tests for KILL intermediate nodes (removes subtree)
- [ ] Add async tests for KILL root
- [ ] Verify tree structure remains valid after KILL

### 2.5 MUMPS Operations - DATA
- [ ] Implement `async fn data_with_context(&self, name: &Name, key: &Key, context: Option<&TransactionContext>) -> Result<DataResult>`:
  - Use `load_node()` for cache-aware node access
  - Return enum: `NoData`, `HasValue`, `HasDescendants`, `Both`
  - If context is Some, use transaction's snapshot isolation
- [ ] Implement `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>`:
  - Simply delegate to `data_with_context(name, key, None)`
- [ ] Add async tests for all four DATA states
- [ ] Verify correct behavior for partial paths

### 2.6 MUMPS Operations - ORDER (Iterator)
- [ ] Implement `async fn order_with_context(&self, name: &Name, key: &Key, context: Option<&TransactionContext>) -> Result<Option<Key>>`:
  - Use `load_node()` for cache-aware node access
  - Find next key in lexicographic order
  - Handle navigating between leaf nodes
  - If context is Some, use transaction's snapshot isolation
- [ ] Implement `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>`:
  - Simply delegate to `order_with_context(name, key, None)`
- [ ] Implement `BTreeIterator` with async next() method
- [ ] Add async tests for ORDER on empty tree
- [ ] Add async tests for ORDER returning next sibling
- [ ] Add async tests for ORDER wrapping to next parent's child
- [ ] Add async tests for exhaustive iteration over entire tree

### 2.7 RUMPS Extension - COLLECT (Stream-Based Functional Iterator)

**Note**: This is a RUMPS-specific extension not found in traditional MUMPS. It provides a functional, Rust-idiomatic stream-based interface for iterating and collecting values from the tree, designed for efficient handling of large datasets.

**Rationale**: Traditional MUMPS requires imperative loops with `$ORDER` to iterate through data:
```mumps
FOR  SET PID=$ORDER(^PATIENT(PID))  QUIT:PID=""  DO
. SET NAME=$GET(^PATIENT(PID,"NAME"))
. ; Process NAME...
```

The `$COLLECT` primitive enables functional-style stream processing that's memory-efficient and composable with Rust's async ecosystem.

#### Primary Stream-Based Method Signatures

- [ ] Implement `fn collect_stream_with_context<'a, P, F, T>(&'a self, name: &'a Name, start: Option<&'a Key>, predicate: P, extract: F, context: Option<&'a TransactionContext>) -> impl Stream<Item = Result<T>> + 'a`:
  ```rust
  /// Create a stream of values from the tree that match the given predicate.
  ///
  /// # Arguments
  /// * `name` - The global or local variable name
  /// * `start` - Optional starting key (None starts from beginning)
  /// * `predicate` - Function that determines whether to continue and include the entry
  /// * `extract` - Function that transforms the entry into the desired output type
  /// * `context` - Optional transaction context for snapshot isolation
  ///
  /// # Returns
  /// A stream that yields extracted values from matching entries
  ///
  /// # Examples
  /// ```ignore
  /// use futures::StreamExt;
  ///
  /// // Process patient names as a stream
  /// let mut name_stream = btree.collect_stream_with_context(
  ///     &Name::Global("PATIENT".into()),
  ///     None,
  ///     |key, data| key.subscripts().len() == 2 && key.subscripts()[1] == "NAME".into(),
  ///     |_key, data| data.value.clone().and_then(|v| match v {
  ///         Value::String(s) => Some(s),
  ///         _ => None,
  ///     }),
  ///     None,
  /// );
  ///
  /// // Process stream items one by one
  /// while let Some(result) = name_stream.next().await {
  ///     match result {
  ///         Ok(name) => println!("Patient: {}", name),
  ///         Err(e) => eprintln!("Error: {}", e),
  ///     }
  /// }
  /// ```
  pub fn collect_stream_with_context<'a, P, F, T>(
      &'a self,
      name: &'a Name,
      start: Option<&'a Key>,
      predicate: P,
      extract: F,
      context: Option<&'a TransactionContext>,
  ) -> impl Stream<Item = Result<T>> + 'a
  where
      P: Fn(&Key, &Arc<NodeData>) -> bool + Send + 'a,
      F: Fn(&Key, Arc<NodeData>) -> Option<T> + Send + 'a,
      T: Send + 'static,
  {
      // Implementation will use async_stream::stream! macro or manual Stream impl:
      // 1. Use context for transaction isolation if provided
      // 2. Start from `start` key or beginning of the tree
      // 3. Use get_next_internal to iterate in order
      // 4. For each entry, check predicate
      // 5. If predicate returns false, end stream
      // 6. If predicate returns true, apply extract function
      // 7. Yield Some(result) for non-None extractions
      // 8. Automatically handle backpressure
  }
  ```

- [ ] Implement `fn collect_stream<'a, P, F, T>(&'a self, name: &'a Name, start: Option<&'a Key>, predicate: P, extract: F) -> impl Stream<Item = Result<T>> + 'a`:
  ```rust
  /// Create a stream of values from the tree (without transaction context).
  /// Simply delegates to collect_stream_with_context with None context.
  pub fn collect_stream<'a, P, F, T>(
      &'a self,
      name: &'a Name,
      start: Option<&'a Key>,
      predicate: P,
      extract: F,
  ) -> impl Stream<Item = Result<T>> + 'a
  where
      P: Fn(&Key, &Arc<NodeData>) -> bool + Send + 'a,
      F: Fn(&Key, Arc<NodeData>) -> Option<T> + Send + 'a,
      T: Send + 'static,
  {
      self.collect_stream_with_context(name, start, predicate, extract, None)
  }
  ```

#### Convenience Methods for Vec Collection

- [ ] Implement `async fn collect_vec_with_context<P, F, T>(&self, name: &Name, start: Option<&Key>, predicate: P, extract: F, context: Option<&TransactionContext>) -> Result<Vec<T>>`:
  ```rust
  /// Collect all matching values into a Vec.
  /// Convenience method that collects the stream for cases where you need all results in memory.
  ///
  /// # Warning
  /// For large datasets, prefer using the stream directly to avoid memory issues.
  pub async fn collect_vec_with_context<P, F, T>(
      &self,
      name: &Name,
      start: Option<&Key>,
      predicate: P,
      extract: F,
      context: Option<&TransactionContext>,
  ) -> Result<Vec<T>>
  where
      P: Fn(&Key, &Arc<NodeData>) -> bool + Send,
      F: Fn(&Key, Arc<NodeData>) -> Option<T> + Send,
      T: Send + 'static,
  {
      use futures::StreamExt;

      self.collect_stream_with_context(name, start, predicate, extract, context)
          .try_collect()
          .await
  }
  ```

- [ ] Implement `async fn collect_vec<P, F, T>(&self, name: &Name, start: Option<&Key>, predicate: P, extract: F) -> Result<Vec<T>>`:
  ```rust
  /// Collect all matching values into a Vec (without transaction context).
  pub async fn collect_vec<P, F, T>(
      &self,
      name: &Name,
      start: Option<&Key>,
      predicate: P,
      extract: F,
  ) -> Result<Vec<T>>
  where
      P: Fn(&Key, &Arc<NodeData>) -> bool + Send,
      F: Fn(&Key, Arc<NodeData>) -> Option<T> + Send,
      T: Send + 'static,
  {
      self.collect_vec_with_context(name, start, predicate, extract, None).await
  }
  ```

#### Supporting Internal Methods

- [ ] Implement `async fn get_next_internal(&self, name: &Name, after: &Key) -> Result<Option<(Key, Arc<NodeData>)>>`:
  - Navigate B-tree to find the next key after `after`
  - Return both key and data for the next entry
  - Handle transitions between leaf nodes
  - Similar to ORDER but returns full entry

- [ ] Implement `async fn get_prev_internal(&self, name: &Name, before: &Key) -> Result<Option<(Key, Arc<NodeData>)>>`:
  - Navigate B-tree to find the previous key before `before`
  - Support for bidirectional iteration (future enhancement)

#### Stream-Based Usage Examples

```rust
use futures::StreamExt;

// Process large dataset as stream (memory efficient)
let mut data_stream = btree.collect_stream(
    &Name::Global("DATA".into()),
    Some(&Key::from(vec!["2025".into()])),
    |key, _| key.subscripts().len() == 2 && key.subscripts()[0] == "2025".into(),
    |_key, data| data.value.clone(),
);

// Process items one at a time without loading all into memory
while let Some(result) = data_stream.next().await {
    match result {
        Ok(value) => process_value(value),
        Err(e) => eprintln!("Error: {}", e),
    }
}

// Find first admin user using stream (early termination)
let admin = btree.collect_stream(
    &Name::Global("USERS".into()),
    None,
    |_key, data| data.value.is_some(),
    |key, data| match data.value {
        Some(Value::String(ref s)) if s.contains("admin") => Some((key.clone(), s.clone())),
        _ => None,
    },
)
.filter_map(|r| future::ready(r.ok()))
.next()
.await;

// Take first 100 matching entries
let first_100: Vec<String> = btree.collect_stream(
    &Name::Global("LOGS".into()),
    None,
    |key, _| key.subscripts().first() == Some(&"2025".into()),
    |_key, data| match data.value {
        Some(Value::String(ref s)) => Some(s.clone()),
        _ => None,
    },
)
.take(100)
.try_collect()
.await?;

// Count entries efficiently using fold
let count = btree.collect_stream(
    &Name::Global("STATS".into()),
    None,
    |key, _| key.subscripts().first() == Some(&"2025".into()),
    |_key, data| if data.has_descendants { Some(()) } else { None },
)
.try_fold(0usize, |acc, _| future::ready(Ok(acc + 1)))
.await?;

// Use collect_vec for small datasets where you need all results
let all_names = btree.collect_vec(
    &Name::Global("PATIENT".into()),
    None,
    |key, _| key.subscripts().len() == 2 && key.subscripts()[1] == "NAME".into(),
    |_key, data| match data.value {
        Some(Value::String(ref s)) => Some(s.clone()),
        _ => None,
    },
).await?;

// Parallel processing with buffered stream
use futures::stream::StreamExt;

let processed_results: Vec<ProcessedData> = btree.collect_stream(
    &Name::Global("RECORDS".into()),
    None,
    |_, data| data.value.is_some(),
    |key, data| Some((key.clone(), data.value.clone())),
)
.map(|result| async move {
    match result {
        Ok((key, value)) => process_record_async(key, value).await,
        Err(e) => Err(e),
    }
})
.buffer_unordered(10)  // Process up to 10 records concurrently
.try_collect()
.await?;
```

#### Implementation Strategy

1. **Phase 1**: Core Stream Infrastructure
   - Implement `get_next_internal` using existing B-tree navigation
   - Create stream wrapper using `async_stream` crate or manual `Stream` implementation
   - Ensure proper lifetime management for borrowed references

2. **Phase 2**: Stream-Based Collection
   - Implement `collect_stream_with_context` with lazy evaluation
   - Add support for early termination when predicate returns false
   - Implement backpressure handling for slow consumers

3. **Phase 3**: Optimizations
   - Batch node reads to reduce lock contention
   - Implement read-ahead buffering for sequential access patterns
   - Add parallel stream processing support with `buffer_unordered`

4. **Phase 4**: Advanced Features (Future)
   - Bidirectional iteration with `get_prev_internal`
   - Range queries with start and end bounds
   - Snapshot iteration for long-running streams

#### Testing

- [ ] Test stream iteration over empty tree
- [ ] Test stream with start key positioning
- [ ] Test predicate-based filtering and early termination
- [ ] Test extract function transformations
- [ ] Test stream cancellation and cleanup
- [ ] Test collecting stream to Vec for small datasets
- [ ] Test streaming with transaction context
- [ ] Test concurrent streams on same tree
- [ ] Test memory usage with millions of entries (stream should be constant memory)
- [ ] Test backpressure with slow consumers
- [ ] Test stream combinators (take, filter_map, fold, etc.)
- [ ] Benchmark stream vs Vec collection performance
- [ ] Test error propagation through stream

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

## Phase 4: Disk Persistence with AsyncStorageEngine

**Note**: Only `Name::Global` entries are persisted to disk. `Name::Local` entries remain in memory only and are not serialized.

**Design Principle**: All storage operations are async from the start. The `AsyncStorageEngine` trait abstracts disk operations, allowing the B-tree to remain agnostic about storage details.

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
  - `async fn set(&mut self, name: &Name, key: &Key, value: Value) -> Result<()>` - delegates to `btree.set_with_context(name, key, value, Some(&self.context))`
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>` - delegates to `btree.get_with_context(name, key, Some(&self.context))`
  - `async fn kill(&mut self, name: &Name, key: &Key) -> Result<()>` - delegates to `btree.kill_with_context(name, key, Some(&self.context))`
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>` - delegates to `btree.data_with_context(name, key, Some(&self.context))`
  - `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>` - delegates to `btree.order_with_context(name, key, Some(&self.context))`
- [ ] Enforce transaction rules:
  - Writes to `Name::Global` MUST be in transaction (return error otherwise)
  - `Name::Local` modifications work outside transactions
  - GET/DATA/ORDER can work with or without transactions

### 5.2 Async Database Handle
- [ ] Create `crates/rumps-storage/src/database.rs` module
- [ ] Define `Database` struct as main entry point:
  ```rust
  pub struct Database {
      btree: Arc<BTree>,
      transaction_manager: Arc<TransactionManager>,
  }
  ```
  - Uses `Arc<BTree>` for thread-safe sharing
  - B-tree handles both `Name::Global` (persistent) and `Name::Local` (ephemeral)
  - Transaction manager coordinates concurrent transactions
- [ ] Implement `Database::open(path: &Path) -> Result<Self>`:
  - Create `FileStorageEngine` with config
  - Initialize `BTree::with_storage(min_degree, storage)`
  - Wrap in Arc for sharing
- [ ] Implement `Database::create(path: &Path) -> Result<Self>`
- [ ] Implement `Database::in_memory() -> Result<Self>` for testing
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
  - `async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>` - delegates to `btree.get()` (simple snapshot read)
  - `async fn data(&self, name: &Name, key: &Key) -> Result<DataResult>` - delegates to `btree.data()`
  - `async fn order(&self, name: &Name, key: &Key) -> Result<Option<Key>>` - delegates to `btree.order()`
- [ ] Add local variable operations (no transaction required):
  - `async fn set_local(&self, name: &Name, key: &Key, value: Value) -> Result<()>` - delegates to `btree.set()`
  - Must verify `name.is_local()`, return error if global
  - Note: Locals use the simple `set()` method, not `set_with_context()`, since they never participate in transactions

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
**Current Phase**: Phase 2.1 Complete! Ready for Phase 2.2 (MUMPS Operations - SET)
**Completed Checkboxes**: 33 / ~160

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

Last Updated: 2025-11-16
