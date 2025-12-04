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

## Architecture: Database and BTree Layers

RUMPS separates concerns between two layers:

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
│    └─────────┘                   │  *_at()   │ ─────────── storage.write()  │
│                                  └───────────┘                              │
│                                                                             │
│  Database calls:                 BTree methods (root-based):                │
│  • get_root(name)                • get_at(root, key)                        │
│  • ensure_root(name)             • set_at(root, key, val) → new_root        │
│  • update_root(name, root)       • kill_at(root, key) → Option<new_root>    │
│  • remove_root(name)             • create_tree() → NodeId                   │
│                                  • delete_tree(root)                        │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Why this separation?**

1. **`BTree` is a pure tree data structure**: It operates only on `NodeId`s with no knowledge of variable names or the registry. This makes it simpler to test and reason about.

2. **`Database` owns namespace management**: The mapping from `Name` (like `^PATIENT`) to root `NodeId` lives here. For persistent globals, roots are lazy-loaded from the on-disk registry.

3. **Registry calls happen at the right layer**: When creating a new global, `Database` calls `registry_insert()` after `btree.create_tree()`. The `BTree` never needs to know about persistence details.

4. **Enables lazy-loading**: `Database.get_root()` can check its cache first, then lazy-load from the registry for globals that haven't been accessed yet.

---

## Data Structures

### `Database`

Namespace management layer (`database.rs`):

```rust
pub(crate) struct Database {
    roots: RwLock<BTreeMap<Name, NodeId>>,  // Name -> root (lazy-loaded)
    btree: Arc<BTree>,                       // The underlying tree
    // storage: Option<Arc<FileStorageEngine>>,  // Future: disk persistence
}
```

**Design rationale:**

- `BTreeMap` for roots enables ordered iteration over variable names
- Lazy-loading from registry avoids loading all globals at startup
- Separates namespace concerns from tree structure

### `BTree`

The tree storage structure (`btree.rs:146`):

```rust
pub(crate) struct BTree {
    nodes: RwLock<HashMap<NodeId, Node>>,   // Node pool (page cache)
    allocator: Arc<dyn NodeAllocator>,      // Node ID allocation
    min_degree: usize,                      // B-tree parameter `t`
    max_memory_bytes: Option<usize>,        // Optional memory limit
    stats: RwLock<BTreeStats>,              // Statistics tracking
}
```

**Design rationale:**

- `HashMap<NodeId, Node>` is a storage pool—`NodeId`s are internal references with no semantic ordering
- Collation order is maintained by sorted `keys` within each `Node` and the tree structure itself
- `RwLock` enables concurrent reads with exclusive writes
- All operations are `async` to support future disk persistence without API changes

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

## Disk Persistence (Phase 4)

The `Database` layer now exists with namespace management. Disk persistence adds:

1. **`FileStorageEngine`** with page cache, WAL, and bitmap allocator (complete)
2. **Global registry** for name→root persistence with chaining (complete)
3. **`BTree` disk integration** (Phase 4.6, in progress):
   - `Option<Arc<FileStorageEngine>>` on `BTree`
   - `load_node()` checks cache, falls back to `storage.read()`
   - Writes go through WAL for durability
4. **Lazy-loading** in `Database.get_root()`:
   - Check in-memory cache first
   - For globals: call `storage.registry_get()` on cache miss
   - Cache result for future lookups
5. **Locals** remain memory-only (no persistence)

The async API and root-based methods enable this transition without breaking changes.

---

## Design Decisions Summary

| Decision                   | Rationale                                       |
|----------------------------|-------------------------------------------------|
| B-tree (not B+-tree)       | Simpler implementation, data in all nodes       |
| Flat key storage           | Efficient range scans, standard B-tree algos    |
| `has_descendants` flag     | Enables MUMPS hierarchy operations cheaply      |
| `Arc<NodeData>`            | Cheap cloning for frequent flag checks          |
| Idempotent merge           | Safe concurrent ancestor creation               |
| Async from day one         | Future disk I/O without API changes             |
| `NodeId` indirection       | Enables lazy loading, page cache, MVCC          |
| `HashMap` for nodes        | Storage pool—ordering is in tree structure      |
| Database/BTree separation  | Decouples namespace from tree structure         |
| Root-based BTree API       | BTree has no registry awareness                 |

---

## Potential Future Optimizations

While current performance is production-ready, areas for future optimization:

1. **Ancestor caching**: Cache "known ancestors" via bloom filter or `HashSet` to reduce repeated `get_internal()` calls in `ensure_ancestors()`

2. **Batch ancestor creation**: Currently sequential with `try_fold`; could batch for better locality

3. **Path compression**: For very deep hierarchies; adds complexity not justified for typical MUMPS use

4. **Memory pooling for `Arc` allocations**: Could reduce allocation overhead, though modern allocators are efficient

---

## Key Implementation Files

| File                                   | Purpose                               |
|----------------------------------------|---------------------------------------|
| `crates/rumps-storage/src/database.rs` | Namespace management (name→root)      |
| `crates/rumps-storage/src/btree.rs`    | B-tree implementation (root-based)    |
| `crates/rumps-storage/src/node.rs`     | `Node`, `NodeData`, `NodeId` types    |
| `crates/rumps-storage/src/engine/`     | `FileStorageEngine`, registry, WAL    |
| `crates/rumps-types/src/key.rs`        | `Key`, `Subscript`, `Name` types      |
| `crates/rumps-types/src/value.rs`      | `Value` enum for stored data          |
