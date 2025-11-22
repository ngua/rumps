# Hierarchical Semantics in RUMPS

## Overview

RUMPS maintains hierarchical semantics through the `has_descendants` flag on nodes, which enables proper implementation of MUMPS tree operations. This flag is critical for distinguishing between leaf nodes, intermediate nodes, and nodes that have both values and descendants.

## Why `has_descendants` Is Critical

The `has_descendants` flag enables three essential MUMPS operations:

1. **`$DATA(key)`** - Returns different values based on node state:
   - `0`: No data (doesn't exist)
   - `1`: Has value only (leaf node)
   - `10`: Has descendants only (intermediate node)
   - `11`: Has both value and descendants

2. **`$ORDER(key)`** - Navigates the hierarchy correctly:
   - Needs to know whether to descend into children or skip to next sibling
   - Without `has_descendants`, cannot distinguish leaves from branches

3. **`KILL(key)`** - Removes subtrees and updates parents:
   - When killing a node, must update parent's `has_descendants` if no siblings remain
   - Incorrect flags would leave orphaned metadata

---

## Storage Model

RUMPS uses a **flat B-tree** (not a true hierarchy) where keys are complete paths:

```text
Physical B-tree storage:
┌─────────────────────────────────────┐
│ Keys (sorted):                      │
│   Key([123])                        │  ← Ancestor (intermediate node)
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

The hierarchy is **implicit** in the key structure. We must maintain `has_descendants` flags to enable hierarchical queries.

---

## Design Decision: Arc<NodeData>

**Critical Optimization**: `Node` stores `Vec<Arc<NodeData>>` instead of `Vec<NodeData>` directly.

### Rationale

- Hierarchy navigation (checking `has_descendants` flags) is extremely frequent in MUMPS operations
- `$DATA`, `$ORDER`, `ensure_ancestors()` all repeatedly access NodeData without needing ownership
- `Arc::clone()` (incrementing refcount) is much cheaper than cloning the entire `NodeData`
- When extracting values in the public `get()` API, we simply clone the `Option<Value>`

### Implementation Impact

1. **Node Type Change**: `pub values: Vec<Arc<NodeData>>` in `rumps-types/src/node.rs`
2. **Return Type**: `get_internal()` returns `Option<Arc<NodeData>>` instead of `Option<NodeData>`
3. **Node Creation**: All values wrapped with `Arc::new(NodeData { ... })`
4. **Serialization**: Custom serialize/deserialize unwraps/wraps Arc
5. **Public API**: `get_with_context()` extracts value by cloning, and `get()` delegates to it:
   ```rust
   pub async fn get_with_context(
       &self,
       name: &Name,
       key: &Key,
       _context: Option<()>, // Will later be `Option<TransactionContext>`
   ) -> Result<Option<Value>> {
       // Handle transaction context here...

       Ok(self.get_internal(name, key).await?.and_then(|arc_data| arc_data.value.clone()))
   }

   pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
       self.get_with_context(name, key, None).await
   }
   ```

### Performance Benefits

- Hierarchy checks: O(1) Arc clone instead of O(value_size) data clone
- Typical case: Most operations just check `has_descendants` flag, never clone the actual data
- Value extraction: Simple clone of the `Option<Value>` for GET operations

---

## Algorithm

### Ancestor Key Generation

For a key `Key([a, b, c, d])`, the ancestors are:
- `Key([a])`
- `Key([a, b])`
- `Key([a, b, c])`

Implemented by the `Key::ancestors()` method in `crates/rumps-types/src/key.rs`.

### SET Operation with Hierarchy Maintenance

When setting `Key([a, b, c])` with value `v`:

1. **Generate ancestors**: `[Key([a]), Key([a, b])]`
2. **For each ancestor** (in order from root to leaf):
   - Search for the key in the B-tree
   - If **not found**: Insert `NodeData::with_descendants()` (value = None, has_descendants = true)
   - If **found**: Update `has_descendants` to `true` if needed
3. **Insert target key**: Insert `Key([a, b, c])` with `NodeData::with_value(v)`
4. **Handle splits** as needed during insertion

### Idempotent Merge Semantics

The `set_internal()` method implements idempotent merge semantics:
- `has_descendants`: Uses OR operation (old || new)
- `value`: Prefers new value if provided, otherwise keeps old value

This ensures concurrent ancestor creation is safe and deterministic.

### Complexity

- **Worst Case**: O(d * log n) where d = key depth
- **Typical Case**: Most MUMPS data is 2-3 levels deep (1-2 ancestors)

---

## Performance Assessment

### Empirical Benchmark Results

Using Criterion benchmarks with async Tokio runtime:

| Benchmark                            | Time (µs) | Description                             |
|--------------------------------------|-----------|-----------------------------------------|
| `insert_by_depth/2`                  | 0.997     | Depth 2 - creates 1 ancestor            |
| `insert_by_depth/3`                  | 1.804     | Depth 3 - creates 2 ancestors           |
| `insert_by_depth/4`                  | 2.605     | Depth 4 - creates 3 ancestors           |
| `insert_by_depth/5`                  | 3.469     | Depth 5 - creates 4 ancestors           |
| `insert_by_depth/10`                 | 11.463    | Depth 10 - creates 9 ancestors          |
| `depth_5_with_existing_ancestors`    | 6.204     | Two inserts at depth 5 (amortization)   |
| `ensure_ancestors_depth_5`           | 5.401     | Isolates ancestor creation              |
| `best_case_depth_2_fresh_tree`       | 1.033     | Shallow nesting baseline                |
| `worst_case_depth_10_fresh_tree`     | 11.111    | Deep nesting scenario                   |

### Strengths

1. **Excellent performance for typical cases**: At depths 2-3 (most common in MUMPS), operations complete in 1-2 microseconds
2. **Linear scaling**: The time increases linearly with depth, showing the expected O(d * log n) characteristics without any exponential behavior
3. **Reasonable worst-case**: Even at extreme depth 10, operations complete in ~11 microseconds
4. **No performance cliffs**: The scaling is smooth and predictable

### Current Implementation Trade-offs

The `Arc<NodeData>` wrapper adds minimal overhead but provides significant benefits for hierarchy navigation. This is a good engineering decision that prioritizes common operations (checking `has_descendants` flags) over rare ones (deep cloning).

### Potential Future Optimizations

While the current performance is good, here are some areas that could be optimized in future phases:

1. **Ancestor caching**:
   - Cache "known ancestors" to avoid repeated checks
   - Could use a bloom filter or simple HashSet
   - Would reduce repeated `get_internal()` calls in `ensure_ancestors()`

2. **Batch ancestor creation**:
   - Currently processes ancestors sequentially with `try_for_each`
   - Could potentially batch operations for better locality
   - However, the current approach is correct for maintaining ordering

3. **Path compression**:
   - For very deep hierarchies, could implement path compression techniques
   - But this adds complexity and typical MUMPS usage doesn't justify it

4. **Memory pooling for Arc allocations**:
   - Could reduce allocation overhead with a pool
   - But modern allocators are already quite efficient

### Recommendation

The current implementation is production-ready. The performance characteristics are:
- Well within acceptable ranges for typical use cases
- Predictable and well-understood
- Without any pathological cases

The code is also:
- Safe (no panics from indexing - all indexing replaced with safe `.get()` calls)
- Maintainable
- Well-tested

The noted optimization opportunities (especially ancestor caching) can be implemented later if profiling shows they're needed in real workloads.

---

## Integration with Future Operations

### GET

- Uses `get_internal()` helper which returns `Arc<NodeData>`
- Main implementation is `get_with_context()` which accepts optional transaction context:
  ```rust
  pub async fn get_with_context(
      &self,
      name: &Name,
      key: &Key,
      _context: Option<()>,  // Will be Option<&TransactionContext> in Phase 5
  ) -> Result<Option<Value>> {
      Ok(self.get_internal(name, key).await?.and_then(|arc_data| arc_data.value.clone()))
  }
  ```
- Public `get()` API is a convenience wrapper:
  ```rust
  pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
      self.get_with_context(name, key, None).await
  }
  ```
- Simple and clear: just clone the `Option<Value>` from the Arc

### DATA

- Uses `get_internal()` to access full `Arc<NodeData>`
- Can cheaply read both `value` and `has_descendants` fields from the Arc
- Returns enum based on `value` and `has_descendants` flags:
  - `NoData` (0): Neither value nor descendants
  - `HasValue` (1): Value only
  - `HasDescendants` (10): Descendants only
  - `Both` (11): Both value and descendants

### KILL

- Must **update** ancestors after deletion
- If all children of a node are killed, set `has_descendants = false`
- This is the inverse operation of SET's ancestor creation

### ORDER

- Uses `has_descendants` to know whether to descend or skip subtrees
- Critical for efficient tree navigation
- Enables proper iteration over hierarchical data structures

---

## Implementation Notes

### Key Components

- **`Key::ancestors()`** (`crates/rumps-types/src/key.rs`): Generates all ancestor keys for a given path
- **`get_internal()`** (`crates/rumps-storage/src/btree.rs`): Retrieves full `Arc<NodeData>` for hierarchy operations
- **`set_internal()`** (`crates/rumps-storage/src/btree.rs`): Accepts `NodeData` directly with idempotent merge semantics
- **`ensure_ancestors()`** (`crates/rumps-storage/src/btree.rs`): Ensures all ancestor keys exist with `has_descendants = true`
- **`insert_non_full_with_data()`** (`crates/rumps-storage/src/btree.rs`): Helper for insertion with `NodeData` merge behavior

### Critical Edge Cases

1. **Intermediate nodes with both value and descendants**: A node at `Key([123])` can have both a value AND descendants like `Key([123, "NAME"])`. The `has_descendants` flag must be preserved when updating the value.

2. **Concurrent ancestor creation**: Multiple threads may try to create the same ancestor simultaneously. The idempotent merge semantics ensure this is safe.

3. **Flag preservation during splits**: When nodes split during insertion, the median key's `NodeData` (including `has_descendants` flag) is promoted to the parent node.

### Thread Safety

All operations use interior mutability via `RwLock`, allowing multiple concurrent readers with exclusive writers. The idempotent merge semantics in `set_internal()` ensure that concurrent ancestor creation is safe and deterministic.
