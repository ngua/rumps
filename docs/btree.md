# RUMPS B-tree Storage Engine

This document describes the B-tree storage engine used by RUMPS for persistent, hierarchical data storage. The implementation lives in `crates/rumps-storage/src/btree.rs` with namespace management in `crates/rumps-storage/src/database.rs`.

---

## Overview

RUMPS stores all data in **B-tree** structures that back the MUMPS-style hierarchical variable system. Unlike traditional MUMPS implementations that use B+-trees (data only in leaves), RUMPS uses **B-tree semantics** where data can exist in both internal and leaf nodes.

### Key Properties

For a B-tree with minimum degree `t` (default `t = 3`):

| Property                   | Value                                       |
|----------------------------|---------------------------------------------|
| Keys per node (non-root)   | `t-1` to `2t-1` (i.e., `2-5` for `t=3`)     |
| Children per internal node | `t` to `2t` (i.e., `3-6` for `t=3`)         |
| Root keys                  | `1` to `2t-1` (can be smaller)              |
| Leaf depth                 | All leaves at same depth                    |
| Key ordering               | Sorted ascending within each node           |

### Hierarchical Semantics via `has_descendants`

RUMPS presents data as a logical hierarchy (like `^PATIENT(123, "NAME")`), but stores it in a flat B-tree where keys are complete paths. This creates a problem: how do we know if a key like `^PATIENT(123)` has children without scanning the entire tree?

The solution is the `has_descendants` flag stored alongside each value. When you insert `^PATIENT(123, "NAME")`, RUMPS automatically creates an ancestor entry for `^PATIENT(123)` with `has_descendants = true`. This flag enables efficient hierarchical operations:

1. **`DATA(key)`** — Returns node status:
   - `0`: No data (doesn't exist)
   - `1`: Has value only (leaf node)
   - `10`: Has descendants only (intermediate node)
   - `11`: Has both value and descendants
   - **Note**: RUMPS returns a native `enum`; numerical values are for MUMPS compatibility

2. **`ORDER(key)`** — Navigates the hierarchy correctly:
   - Needs to know whether to descend into children or skip to next sibling
   - Without `has_descendants`, cannot distinguish leaves from branches

3. **`KILL(key)`** — Removes subtrees and updates parents:
   - When killing a node, must update parent's `has_descendants` if no siblings remain
   - Incorrect flags would leave orphaned metadata

---

## Architecture: Three-Layer Design

RUMPS uses a **three-layer architecture** that separates logical operations, tree structure, and physical persistence:

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│  DATABASE LAYER (Name-aware, logical operations, WAL coordination)          │
│  ───────────────────────────────────────────────────────────────────────    │
│                                                                             │
│  • Maps Name → NodeId via roots: BTreeMap<Name, NodeId>                     │
│  • Lazy-loads global roots from on-disk registry                            │
│  • Logs logical WAL records with variable Names (SET, KILL)                 │
│  • Coordinates transaction commits and recovery                             │
│                                                                             │
│  MUMPS Operations (with WAL logging for globals):                           │
│  • set(name, key, val):                                                     │
│      1. Get old value via btree.get_internal() for undo log                 │
│      2. Log WalRecord::Set {name, key, old, new} (write-ahead!)             │
│      3. Call btree.set_at(root, key, val) to modify tree                    │
│      4. Update root if changed via update_root()                            │
│  • kill(name, key): Similar flow with WalRecord::KillEntry                  │
│  • flush(): Logs TxnCommit, syncs WAL, flushes dirty pages                  │
│  • get(name, key): Read-only, no WAL                                        │
│                                                                             │
│  Namespace Management:                                                      │
│  • get_root(name) → Option<NodeId>                                          │
│  • ensure_root(name) → NodeId (creates if needed)                           │
│  • update_root(name, new_root) (after tree structure changes)               │
│  • remove_root(name) (removes from cache + registry)                        │
└─────────────────────────────┬───────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  BTREE LAYER (NodeId-only, pure tree operations, NO WAL knowledge)          │
│  ─────────────────────────────────────────────────────────────────          │
│                                                                             │
│  • Operates ONLY on NodeIds - no Name awareness whatsoever                  │
│  • Maintains in-memory tree structure in nodes: HashMap<NodeId, Node>       │
│  • Modifies tree and marks pages dirty - NEVER writes to disk or WAL        │
│                                                                             │
│  Root-Based API:                                                            │
│  • get_at(root, key) → Option<Value>                                        │
│  • set_at(root, key, val) → new_root (may split, change root)               │
│  • kill_at(root, key) → Option<new_root> (may merge, empty tree)            │
│  • data_at(root, key) → DataStatus                                          │
│  • order_at(root, after) → Option<Key>                                      │
│  • collects_at(root, start, pred, extract) → Stream                         │
│  • create_tree() → NodeId (allocate empty tree)                             │
│  • delete_tree(root) → count (deallocate all nodes)                         │
│                                                                             │
│  Node Persistence:                                                          │
│  • load_node(id): Check cache, load from storage.read() on miss             │
│  • save_node(id, node): Update cache + storage.mark_dirty() (NO write!)     │
└─────────────────────────────┬───────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  STORAGE LAYER (Physical I/O, WAL files, page cache)                        │
│  ──────────────────────────────────────────────────────                     │
│                                                                             │
│  FileStorageEngine provides:                                                │
│  • Page cache with LRU eviction                                             │
│  • Global registry (name → root PageId) with chaining                       │
│  • Write-Ahead Log for durability                                           │
│  • Bitmap allocator for page allocation                                     │
│                                                                             │
│  BTree Interface:                                                           │
│  • read(id) → Node (load from disk/cache)                                   │
│  • mark_dirty(id, node) (updates cache, marks dirty - no immediate write)   │
│  • allocate() → PageId                                                      │
│  • deallocate(id)                                                           │
│                                                                             │
│  Database Interface (WAL operations):                                       │
│  • wal_append(record) → WalSequence (append logical record)                 │
│  • wal_sync() (fsync WAL to disk for durability)                            │
│  • flush() (write all dirty pages to data file)                             │
│  • registry_get(name) → Option<PageId>                                      │
│  • registry_insert(name, page_id)                                           │
│  • registry_remove(name)                                                    │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Why this three-layer separation?**

1. **Database = Logical Coordinator**:
   - Knows about variable `Name`s (like `^PATIENT`)
   - Logs **logical** WAL records that include Names for meaningful recovery
   - Manages namespace (name → root mapping) with lazy-loading from registry
   - Coordinates transactions and commits

2. **BTree = Pure Tree Operations**:
   - Operates ONLY on `NodeId`s with zero knowledge of `Name`s or WAL
   - Implements B-tree algorithms (split, merge, search, delete)
   - Only marks pages dirty via `storage.mark_dirty()` - never writes to disk
   - Simple, testable, focused on tree correctness

3. **Storage = Physical I/O**:
   - Handles actual disk reads/writes
   - Manages page cache, WAL files, registry files
   - Provides durability guarantees (WAL sync, page flush)
   - Abstracts physical storage from logical operations

**Critical architectural rule**: WAL records need variable `Name`s for crash recovery (e.g., "replay SET to `^PATIENT`"). Only the Database layer has both Names and the BTree context, so WAL logging MUST happen at the Database layer, not in BTree or Storage.

---

## Data Structures

### `Database`

Namespace management and logical operation coordinator (`database.rs`):

```rust
pub(crate) struct Database {
    roots: RwLock<BTreeMap<Name, NodeId>>,         // Name → root (lazy-loaded)
    btree: Arc<BTree>,                              // The underlying tree
    storage: Option<Arc<FileStorageEngine>>,        // Optional disk persistence
}
```

**Design rationale:**

- `BTreeMap` for roots enables ordered iteration over variable names
- Lazy-loading from registry avoids loading all globals at startup
- Separates namespace concerns from tree structure
- `storage` is `Option`: `Some` for persistent databases, `None` for in-memory
- `Database` owns the interface to WAL operations via `storage.wal_append()` / `storage.wal_sync()`

**Key methods:**

- **Namespace**: `get_root()`, `ensure_root()`, `update_root()`, `remove_root()`
- **MUMPS Operations**: `set()`, `kill()`, `get()`, `data()`, `order()`, `collects()`
- **Persistence**: `create()`, `open()`, `flush()`, `close()`
- **Recovery**: `recover()` (private, called during `open()`)

### `BTree`

The tree storage structure (`btree.rs`):

```rust
pub(crate) struct BTree {
    nodes: RwLock<HashMap<NodeId, Node>>,       // In-memory node cache
    allocator: Arc<dyn NodeAllocator>,          // Node ID allocation
    storage: Option<Arc<dyn AsyncStorageEngine>>, // Optional persistence
    min_degree: usize,                          // B-tree parameter `t`
    max_memory_bytes: Option<usize>,            // Optional memory limit
    stats: RwLock<BTreeStats>,                  // Statistics tracking
}
```

**Design rationale:**

- `HashMap<NodeId, Node>` is an in-memory cache—`NodeId`s are internal references with no semantic ordering
- Collation order is maintained by sorted `keys` within each `Node` and the tree structure itself
- `storage` is `Option`: `Some` for disk-backed trees, `None` for in-memory
- `RwLock` enables concurrent reads with exclusive writes
- All operations are `async` for consistent API regardless of storage backend

**Key internal methods:**

- `load_node(id)`: Check cache, load from `storage.read()` on miss
- `save_node(id, node)`: Update cache, call `storage.mark_dirty()` (no write!)
- Pure tree operations: `set_at()`, `get_at()`, `kill_at()`, `data_at()`, etc.
- **No WAL awareness**: Just modifies tree and marks pages dirty

### `Node`

A B-tree node (`node.rs:137`):

```rust
pub(crate) struct Node {
    keys: Vec<Key>,              // Complete key paths (sorted)
    children: Vec<NodeId>,       // Child references (empty for leaves)
    values: Vec<Arc<NodeData>>,  // Data for each key
    is_leaf: bool,               // Leaf flag
}
```

**`Arc<NodeData>` optimization:** Values are wrapped in `Arc` for efficient hierarchy navigation. Operations like `DATA` and ancestor maintenance frequently check `has_descendants` without needing ownership. `Arc::clone()` (refcount increment) is much cheaper than cloning the entire `NodeData`.

**Collation order:** The `keys: Vec<Key>` is always kept sorted according to the extended MUMPS collation. All tree operations use `binary_search()` on this vector. The tree structure (parent-child relationships via `children`) combined with sorted keys maintains global ordering.

### `NodeData`

Data stored at each key (`node.rs`):

```rust
pub(crate) struct NodeData {
    value: Option<Value>,    // The actual data (if any)
    has_descendants: bool,   // Whether key has children in the hierarchy
}
```

Four possible states:

| State        | `value`   | `has_descendants` | `DATA` result |
|--------------|-----------|-------------------|---------------|
| Empty        | `None`    | `false`           | `0`           |
| Intermediate | `None`    | `true`            | `10`          |
| Leaf         | `Some(v)` | `false`           | `1`           |
| Both         | `Some(v)` | `true`            | `11`          |

### `NodeId`

Reference to a node (`node.rs:98`):

```rust
#[repr(transparent)]
pub(crate) struct NodeId(u64);
```

Uses indirection (not `Box<Node>`) to enable:
- **Lazy loading**: Load nodes from disk on demand
- **Scalability**: Only working set in memory
- **Page cache integration**: LRU eviction
- **Unified model**: Same structure for globals (disk) and locals (memory)

### `Key`

A hierarchical path through the tree (`key.rs`):

```rust
pub struct Key(Vec<Subscript>);
```

Keys are sequences of `Subscript` values with **extended collation order**:

1. **Booleans**: `false < true`
2. **Numbers**: Numeric order (`-10 < 0 < 1.5 < 10`)
3. **Chars**: Unicode order
4. **Strings**: Lexicographic order
5. **JSON**: By string representation

---

## Storage Model: Flat B-tree

RUMPS uses a **flat B-tree** where keys are complete paths, not a true hierarchy:

```text
Physical B-tree storage:
┌─────────────────────────────────────┐
│ Keys (sorted):                      │
│   Key([123])                        │  ← Ancestor (intermediate)
│   Key([123, "ADDR"])                │  ← Descendant 1
│   Key([123, "DOB"])                 │  ← Descendant 2
│   Key([123, "NAME"])                │  ← Descendant 3
│   Key([124])                        │  ← Different subtree
│   Key([124, "NAME"])                │  ← Descendant
└─────────────────────────────────────┘

Logical hierarchy (user view):
^PATIENT
  ├─ 123
  │   ├─ "ADDR"
  │   ├─ "DOB"
  │   └─ "NAME"
  └─ 124
      └─ "NAME"
```

The hierarchy is **implicit** in key structure. The `has_descendants` flag on each `NodeData` enables hierarchical queries without scanning.

---

## BTree Operations (Root-Based API)

All `BTree` methods operate on `NodeId` roots, not variable names. The `Database` layer handles name→root resolution.

### `set_at` — Insert/Update

```rust
pub async fn set_at(&self, root: NodeId, key: &Key, value: Value, ctx: &TransactionContext) -> Result<NodeId>
```

Returns the (possibly new) root `NodeId`. The root may change if the tree grows due to node splitting.

**Algorithm:**

1. **Ensure ancestors** (`ensure_ancestors`):
   - Generate ancestor keys via `Key::ancestors()`
   - For each ancestor (root to leaf):
     - If not found: insert `NodeData::with_descendants()`
     - If found but `has_descendants = false`: update flag to `true`

2. **Insert target key** (`set_at_node`):
   - If root is full (`2t-1` keys): split root, create new root
   - Insert via `insert_non_full_with_data`

3. **Handle splits** during insertion:
   - Split full nodes before descending
   - Median key+value promoted to parent

**Merge semantics for existing keys:**

```rust
// Idempotent merge: safe for concurrent ancestor creation
has_descendants = old.has_descendants || new.has_descendants  // OR
value = new.value.or(old.value)                               // Prefer new
```

### `get_at` — Retrieve Value

```rust
pub async fn get_at(&self, root: NodeId, key: &Key, ctx: Option<&TransactionContext>) -> Result<Option<Value>>
```

**Algorithm:**

1. Binary search to find key position in node
2. If found: return value
3. If not found and leaf: return `None`
4. If not found and internal: recurse to appropriate child

**Complexity:** `O(log n)` where `n` is total key count.

### `kill_at` — Delete Subtree

```rust
pub async fn kill_at(&self, root: NodeId, key: &Key, ctx: &TransactionContext) -> Result<Option<NodeId>>
```

Returns `Some(new_root)` or `None` if the tree became empty.

**Algorithm:**

1. **Collect all keys** with prefix (`collect_keys_with_prefix`)
2. **Sort deepest-first** to minimize rebalancing
3. **Delete each key** (`delete_key_from_node`):
   - Leaf: remove directly
   - Internal: replace with predecessor, delete predecessor
4. **Rebalance** if nodes become underfull:
   - Try borrowing from sibling
   - If not possible: merge with sibling
5. **Update ancestors** (`update_ancestors_after_kill`):
   - If no descendants remain: set `has_descendants = false`
   - If no value and no descendants: remove node entirely

### `data_at` — Check Node Status

```rust
pub async fn data_at(&self, root: NodeId, key: &Key, ctx: Option<&TransactionContext>) -> Result<DataStatus>
```

Returns:
- `NoData` (0): Node doesn't exist
- `HasValue` (1): Value only
- `HasDescendants` (10): Descendants only
- `Both` (11): Both value and descendants

### `order_at` — Next Key

```rust
pub async fn order_at(&self, root: NodeId, after: Option<&Key>, ctx: Option<&TransactionContext>) -> Result<Option<Key>>
```

**Algorithm:**

1. If `after` is `None`: return leftmost key (`find_leftmost_key`)
2. Otherwise: find successor (`find_successor_key`)
   - Binary search to find position
   - If exact match in internal node: leftmost key in right subtree
   - If not found: navigate to appropriate child, then check next key

### `collects_at` — Stream Iteration

```rust
pub fn collects_at<P, F, T>(&self, root: NodeId, start: Option<&Key>, pred: P, extract: F, ctx: Option<&TransactionContext>) -> impl Stream<Item = Result<T>>
```

Creates a lazy stream that:
- Iterates in lexicographic order
- Filters by predicate
- Transforms via extract function

### `create_tree` / `delete_tree` — Tree Lifecycle

```rust
pub async fn create_tree(&self) -> Result<NodeId>
pub async fn delete_tree(&self, root: NodeId) -> Result<usize>
```

- `create_tree()`: Allocates an empty leaf node, returns its `NodeId`
- `delete_tree()`: Recursively deallocates all nodes, returns count freed

---

## Core Algorithms

### Node Splitting (`split_node`)

When a node reaches `2t-1` keys, split at median:

```text
Before (min_degree=3, 5 keys):
Node: [10, 20, 30, 40, 50]

After:
Left:  [10, 20]
Median: 30 (promoted to parent with its value)
Right: [40, 50]
```

**B-tree semantics**: The median key AND its associated `NodeData` (including value) are promoted to the parent. This differs from B+-trees where only the key is promoted.

### Node Merging (`merge_nodes`)

When siblings are both at minimum size, merge:

```text
Before:
Parent: [..., 30, ...]
            /  \
Left:   [10, 20]
Right:  [40, 50]

After (left node):
Merged: [10, 20, 30, 40, 50]
```

The separator key (30) and its value from the parent are included in the merge.

### Sibling Borrowing (`borrow_from_sibling`)

When a node is underfull but sibling has extra keys:

```text
Before (borrowing from left):
Parent: [..., 30, ...]
            /  \
Left:   [10, 15, 20, 25]  (has extra)
Right:  [35]              (underfull)

After:
Parent: [..., 25, ...]
            /  \
Left:   [10, 15, 20]
Right:  [30, 35]          (borrowed 25, got old separator 30)
```

### Ancestor Maintenance (`ensure_ancestors`)

For key `Key([a, b, c, d])`, ancestors are:
- `Key([a])`
- `Key([a, b])`
- `Key([a, b, c])`

Each ancestor is ensured to exist with `has_descendants = true`:

```rust
stream::iter(ancestors)
    .try_fold(root, |cur_root, ancestor_key| async move {
        match self.get_internal(cur_root, &ancestor_key).await? {
            Some(data) if !data.has_descendants => {
                self.update_descendants_flag(cur_root, &ancestor_key, true).await?;
                Ok(cur_root)
            }
            None => {
                self.set_at_node(cur_root, &ancestor_key, NodeData::with_descendants()).await
            }
            _ => Ok(cur_root)
        }
    })
```

---

## Thread Safety

All operations use `RwLock`:
- **Readers**: Multiple concurrent reads allowed
- **Writers**: Exclusive access for writes

**Idempotent merge semantics** ensure concurrent ancestor creation is safe:
- Multiple operations setting same ancestor won't conflict
- `has_descendants` uses OR (once true, stays true)

---

## Performance Characteristics

### Complexity

| Operation | Average      | Notes                            |
|-----------|--------------|----------------------------------|
| `get_at`  | O(log n)     | Binary search at each level      |
| `set_at`  | O(d × log n) | `d` = key depth for ancestors    |
| `kill_at` | O(k × log n) | `k` = keys deleted               |
| `data_at` | O(log n)     | Same as `get_at`                 |
| `order_at`| O(log n)     | May traverse multiple nodes      |

### Benchmark Results

Empirical benchmarks with async Tokio runtime:

| Benchmark                         | Time (µs) | Description                           |
|-----------------------------------|-----------|---------------------------------------|
| `insert_by_depth/2`               | 0.997     | Depth 2 — creates 1 ancestor          |
| `insert_by_depth/3`               | 1.804     | Depth 3 — creates 2 ancestors         |
| `insert_by_depth/4`               | 2.605     | Depth 4 — creates 3 ancestors         |
| `insert_by_depth/5`               | 3.469     | Depth 5 — creates 4 ancestors         |
| `insert_by_depth/10`              | 11.463    | Depth 10 — creates 9 ancestors        |
| `depth_5_with_existing_ancestors` | 6.204     | Two inserts at depth 5 (amortization) |
| `ensure_ancestors_depth_5`        | 5.401     | Isolates ancestor creation            |
| `best_case_depth_2_fresh_tree`    | 1.033     | Shallow nesting baseline              |
| `worst_case_depth_10_fresh_tree`  | 11.111    | Deep nesting scenario                 |

**Strengths:**
- Excellent performance for typical depths (2-3): 1-2 µs
- Linear scaling with depth (no exponential blowup)
- Reasonable worst-case at depth 10: ~11 µs
- Smooth, predictable scaling

Most MUMPS data is 2-3 levels deep, so typical operations complete in 1-2 µs.

---

## Serialization

### `NodeData` Compact Encoding

Uses a single tag byte:

| Tag    | State        | Size            |
|--------|--------------|-----------------|
| `0x00` | Empty        | 1 byte          |
| `0x01` | Intermediate | 1 byte          |
| `0x02` | Leaf         | 1 + value bytes |
| `0x03` | Both         | 1 + value bytes |

### `Node` Serialization

Serializes via `NodeRaw` (unwraps `Arc`s):

```rust
#[serde(from = "NodeRaw", into = "NodeRaw")]
struct Node { ... }

struct NodeRaw {
    keys: Vec<Key>,
    children: Vec<NodeId>,
    values: Vec<NodeData>,  // No Arc wrapper
    is_leaf: bool,
}
```

---

## Disk Persistence & WAL (Phase 4.6 Complete)

The three-layer architecture enables full disk persistence with crash recovery:

### Components

1. **`FileStorageEngine`** ✅ Complete
   - Page cache with LRU eviction
   - Write-Ahead Log (WAL) for durability
   - Bitmap allocator for page allocation
   - Global registry with chaining for name→root persistence

2. **Database Layer WAL Integration** ✅ Complete
   - `set()`: Logs `WalRecord::Set` before modifying tree
   - `kill()`: Logs `WalRecord::KillEntry` for each deleted entry
   - `flush()`: Logs `WalRecord::TxnCommit`, syncs WAL, flushes dirty pages
   - `recover()`: Replays committed WAL operations on startup

3. **BTree Disk Integration** ✅ Complete
   - `Option<Arc<dyn AsyncStorageEngine>>` on `BTree`
   - `load_node()`: Checks in-memory cache, loads from `storage.read()` on miss
   - `save_node()`: Updates cache and calls `storage.mark_dirty()` (no immediate write)
   - No WAL knowledge - just marks pages dirty

4. **Namespace Persistence** ✅ Complete
   - `Database.get_root()`: Checks in-memory cache first
   - For globals: Lazy-loads from `storage.registry_get()` on cache miss
   - `Database.ensure_root()`: Creates tree and registers in registry
   - `Database.update_root()`: Updates registry after tree structure changes

5. **Transaction Model** (Phase 4.6 uses implicit transactions)
   - All operations use `TransactionId::IMPLICIT` for WAL logging
   - Phase 5 will add multi-transaction support with proper isolation

### Write-Ahead Logging Flow

```text
User: db.set(&global!("PATIENT"), &key![123, "NAME"], Value::from("Alice"))
  ↓
Database.set():
  1. Get old value via btree.get_internal() [for undo log]
  2. storage.wal_append(WalRecord::Set {              [WRITE-AHEAD!]
       name: "^PATIENT",
       key: [123, "NAME"],
       old: None,
       new: NodeData { value: Some("Alice"), has_descendants: false }
     })
  3. btree.set_at(root, key, val)                     [Modify in-memory tree]
       → Calls save_node() → storage.mark_dirty()     [Mark page dirty, no write]
  4. update_root(name, new_root) if root changed      [Update registry if needed]

Later: db.flush()
  1. storage.wal_append(WalRecord::TxnCommit)         [Log commit]
  2. storage.wal_sync()                               [fsync WAL = DURABLE!]
  3. storage.flush()                                  [Write dirty pages to disk]
```

### Crash Recovery

On startup, `Database::open()` calls `recover()`:

```rust
1. WalReader::open(&wal_dir).recover()
   → Returns list of committed operations (between TxnBegin/TxnCommit pairs)

2. For each committed WalOp::Set { name, key, new, old }:
   - ensure_root(name)
   - btree.set_at(root, key, new.value)
   - update_root(name, new_root) if changed

3. For each committed WalOp::KillEntry { name, key, data }:
   - get_root(name)
   - btree.kill_at(root, key)
   - update_root or remove_root based on result
```

WAL records are **logical** (include variable `Name`s), making recovery meaningful and debuggable.

### Locals vs Globals

- **Globals** (`^NAME`):
  - Persisted to disk via WAL and page cache
  - Lazy-loaded from registry on first access
  - Survive database restarts

- **Locals** (`NAME`):
  - Memory-only (no WAL logging, no registry)
  - Discarded when database closes
  - Fast, ephemeral storage for session state

The async API and root-based BTree methods enabled this transition without breaking changes.

---

## Design Decisions Summary

| Decision                      | Rationale                                          |
|-------------------------------|----------------------------------------------------|
| B-tree (not B+-tree)          | Simpler implementation, data in all nodes          |
| Flat key storage              | Efficient range scans, standard B-tree algos       |
| `has_descendants` flag        | Enables MUMPS hierarchy operations cheaply         |
| `Arc<NodeData>`               | Cheap cloning for frequent flag checks             |
| Idempotent merge              | Safe concurrent ancestor creation                  |
| Async from day one            | Future disk I/O without API changes                |
| `NodeId` indirection          | Enables lazy loading, page cache, MVCC             |
| `HashMap` for nodes           | Storage pool—ordering is in tree structure         |
| Three-layer architecture      | Clear separation: logic, tree, physical I/O        |
| Database/BTree separation     | Decouples namespace from tree structure            |
| Root-based BTree API          | BTree has no registry/WAL/Name awareness           |
| WAL at Database layer         | WAL needs Names; only Database has Name context    |
| BTree only marks dirty        | Database coordinates WAL + tree + flush            |
| Logical WAL records           | Recovery replays meaningful ops with Names         |
| `mark_dirty()` not `write()`  | Separates intent (dirty) from action (flush)       |

---

## Potential Future Optimizations

While current performance is production-ready, areas for future optimization:

1. **Ancestor caching**: Cache "known ancestors" via bloom filter or `HashSet` to reduce repeated `get_internal()` calls in `ensure_ancestors()`

2. **Batch ancestor creation**: Currently sequential with `try_fold`; could batch for better locality

3. **Path compression**: For very deep hierarchies; adds complexity not justified for typical MUMPS use

4. **Memory pooling for `Arc` allocations**: Could reduce allocation overhead, though modern allocators are efficient

---

## Key Implementation Files

| File                                      | Purpose                                    |
|-------------------------------------------|--------------------------------------------|
| `crates/rumps-storage/src/database.rs`    | Database layer: namespace + WAL + MUMPS ops |
| `crates/rumps-storage/src/btree.rs`       | BTree layer: pure tree operations          |
| `crates/rumps-storage/src/node.rs`        | `Node`, `NodeData`, `NodeId` types         |
| `crates/rumps-storage/src/engine/`        | Storage layer: `FileStorageEngine`         |
| `crates/rumps-storage/src/engine/file.rs` | Page cache, registry, disk I/O             |
| `crates/rumps-storage/src/wal/`           | Write-Ahead Log: writer, reader, recovery  |
| `crates/rumps-storage/src/transaction.rs` | Transaction types (`TransactionId`, etc.)  |
| `crates/rumps-types/src/key.rs`           | `Key`, `Subscript`, `Name` types           |
| `crates/rumps-types/src/value.rs`         | `Value` enum for stored data               |
