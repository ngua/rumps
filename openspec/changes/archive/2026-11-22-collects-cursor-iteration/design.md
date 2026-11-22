## Context

RUMPS' `collects` iteration system currently uses `stream::unfold` with a closure that calls `get_next_internal` for every entry. Each `get_next_internal` calls `find_successor_entry`, which descends from the root to a leaf (`O(log n)`) to find the next key strictly greater than the cursor. For a scan of `m` entries in a tree of `n` keys, this costs `O(m * log n)` node loads.

The B-tree is a standard B-tree (not B+-tree): data lives in both internal and leaf nodes, and each node has `keys`, `values`, `children`, and `is_leaf`. Nodes are loaded via `BTree::load_node(NodeId)`, which checks a `DashMap`-based cache and falls back to disk. The tree uses `Arc<Node>` for cheap cloning.

The existing `MergeState` in `Transaction` merges a snapshot stream (from `Database::collects`) with a `BTreeMap` of buffered writes. It uses `advance_buffered` (O(log n) range lookup on the `BTreeMap`) and `advance_snapshot` (pull next from the stream) to produce a merged, sorted sequence.

## Goals / Non-Goals

**Goals:**
- Replace per-entry root descent with `O(1)` amortized cursor-based iteration
- Add range-bounded queries with early termination (no full-tree scan for bounded ranges)
- Add reverse iteration via a unified `Scan` type combining range + direction
- Collapse the API: one `collect` method handles forward, reverse, bounded, and unbounded via `impl Into<Scan>`
- Remove `collects_vec` / `collects_prefix_vec`; callers use `.try_collect().await` on the stream
- Rename `collects` -> `collect` throughout (methods, internals, docs, tests, benchmarks)
- Add `Dir` to `collect_prefix` for reverse prefix iteration, using `Subscript::successor` / `Key::prefix_upper_bound` to compute the seek target
- Keep the cursor `Send`-safe and compatible with snapshot isolation

**Non-Goals:**
- Read-ahead / batch prefetching (stretch goal; deferred to a follow-up)
- Parallel processing with `buffer_unordered` (deferred; see rationale below)
- Changing the `$ORDER` or `$QUERY` implementations to use cursors (can follow naturally but is not in scope)
- Double-ended iteration (simultaneous forward + backward on one cursor)

## Decisions

### 1. Rename `collects` -> `collect`, remove `collects_vec`

**Choice**: Rename `collects` to `collect` at every layer (`BTree::collect_at`, `Database::collect`, `Transaction::collect`, internal helpers `collect_internal`, `collect_prefix_internal`). Remove `collects_vec`, `collects_prefix_vec`, and `collects_vec_at` entirely.

**Rationale**: `collects` was named to avoid shadowing `Iterator::collect()`, but `db.collect(name, scan, pred, extract)` has a completely different signature; there is no ambiguity for the compiler or the reader. `collect` is the natural name. The `_vec` variants are one-liners (`.try_collect().await` on the stream); having dedicated methods adds API surface without value and sets a precedent we would not follow (we would not add `collect_hashmap`, `collect_count`, etc.). Callers who want a `Vec` write `db.collect(...).await?.try_collect().await`.

### 2. Unified `Scan` type collapses the API

**Choice**: A single `Scan` struct in `rumps-types` bundles `Bounds` + `Dir`:

```rust
pub enum Dir { Forward, Reverse }

pub struct Bounds {
    start: Bound<Key>,
    end: Bound<Key>,
}

pub struct Scan {
    pub bounds: Bounds,
    pub dir: Dir,
}
```

`Scan::forward(bounds)` and `Scan::reverse(bounds)` are symmetric convenience constructors; `Scan::new(bounds, dir)` covers all combinations. `From<Option<&Key>>` for `Scan` builds `Scan { bounds: Bounds(Excluded(key) | Unbounded, Unbounded), dir: Dir::Forward }`, making existing call sites work via `impl Into<Scan>` with no changes (beyond the rename). The signature becomes:

```rust
pub async fn collect<'a, S, P, F, T>(
    &'a self, name: &'a Name, scan: S, pred: P, extract: F,
) -> Result<impl Stream<Item = Result<T>> + Send + 'a>
where
    S: Into<Scan>,
    // ...
```

No separate `collect_rev`, `collect_range`, etc. The `collect_prefix` family stays as a convenience (prefix termination is key-structure-aware, not just a range bound) and gains a `Dir` parameter for reverse prefix iteration (see decision 10).

**Alternatives considered**:
- Separate `collect_rev` / `collect_range` / `collect_range_vec` families: explodes the API surface (~10 new methods across `BTree`, `Database`, `Transaction`).
- Direction as a const generic: doubles monomorphization for negligible benefit.

**Rationale**: Rust enums exist; use them. One entry point, one `MergeState` impl, one cursor, configured by data.

### 3. Cursor holds `(NodeId, usize)` stack, not `Arc<Node>` references

**Choice**: The cursor stores a `SmallVec<[(NodeId, usize); 8]>` representing the path from root to the current position. Each entry is `(node_id, key_index)`. Nodes are loaded on demand via `load_node`.

**Alternatives considered**:
- Holding `Arc<Node>` in the stack: avoids re-loading but pins nodes in memory for the entire iteration, preventing cache eviction. Also complicates the borrow story since `Arc<Node>` is `'static` but we want the cursor to borrow `&BTree`.
- Leaf-only linked list (B+-tree style): would require restructuring the entire tree; too invasive.

**Rationale**: `NodeId` is `Copy` (it's a `u64` newtype), so the stack is cheap. The `DashMap` cache means re-loading a recently-visited node is an `O(1)` hash lookup, not a disk read. This also means the cursor is safe if a node is split or merged by a concurrent operation on a different global (it just loads the new version).

### 4. Cursor is `btree::Cursor`, a named struct

**Choice**: `Cursor<'a>` is a named struct in the `btree` module with `seek`, `seek_back`, `advance`, and `advance_back` async methods. The `collect_internal` stream uses `stream::unfold` over the cursor.

**Rationale**: A named struct is testable in isolation, reusable for future `$ORDER`/`$QUERY` optimizations, and clearer than the current nested closures in `collect_internal`. Naming it `Cursor` (not `BTreeCursor`) since the module path provides context.

### 5. B-tree semantics: internal nodes hold data

The existing B-tree stores key-value pairs in internal nodes (the median is promoted on split, carrying its data). The cursor must yield entries from internal nodes as well as leaves.

**Cursor traversal order** (in-order): for a node at stack position `(node_id, i)`:
- If the node is internal, yield `keys[i]` then descend into `children[i+1]` to find the leftmost entry
- If the node is a leaf, yield `keys[i]` then increment `i`
- When `i` exceeds the last key index, pop the stack

This naturally produces sorted order because B-tree in-order traversal visits: left subtree, key[0], second subtree, key[1], ..., key[n-1], rightmost subtree. Reverse traversal is the mirror.

### 6. `Bounds` and `Scan` live in `rumps-types`

**Choice**: Both types are defined in `rumps-types` alongside `Key`, `Value`, etc.

**Rationale**: Part of the public API (used by `Database::collect` and `Transaction::collect`). Putting them in `rumps-types` keeps them available to downstream crates (like `rumps-query`) without depending on `rumps-storage`.

### 7. `MergeState` takes `Dir` from `Scan`

**Choice**: `MergeState` stores a `Dir` value. In `advance_buffered`, forward mode uses `range((Excluded(...), Unbounded)).next()` and reverse mode uses `range((Unbounded, Excluded(...))).next_back()`. The snapshot stream is already a `BoxStream`, so the reverse cursor is just boxed in reverse mode. The merge comparison flips on `Dir`: forward yields the smaller key first, reverse yields the larger.

**Rationale**: One `MergeState` impl. The comparison flip is a single `match`. The overhead is negligible compared to I/O.

### 8. Reverse `find_predecessor_entry` mirrors `find_successor_entry`

**Choice**: Implement `find_predecessor_entry` as the exact mirror of `find_successor_entry`:
- Descend toward the target key
- At each internal node, track a fallback candidate to the left of the search position
- At a leaf, the predecessor is `keys[pos - 1]` (if `pos > 0`) or the tracked fallback

This is used for `seek_back` (seek to the last key <= start bound) and for `advance_back` when the cursor needs to cross a subtree boundary.

### 9. `buffer_unordered` / parallel processing deferred

**Choice**: Not included in this change.

**Rationale**: The `pred` and `extract` closures in `collect` are synchronous (`Fn`, not `AsyncFn`), so there is nothing to parallelize inside the iteration itself. Parallelism only helps when the *caller* does async work per record (e.g. `rumps-query` running a subquery per row). Adding it here would mean either making `extract` async (which changes every call site and complicates `MergeState`) or adding a `collect_par` variant that is effectively `stream.map(f).buffer_unordered(n)`, something the caller can already do in one line on the returned `Stream`. The cursor gives the foundation (a proper `Stream` with `O(1)` amortized steps); `buffer_unordered` is a one-liner on top. Better to design that API when the concrete consumer exists in `rumps-query`.

### 10. `collect_prefix` gains `Dir` via `Subscript::successor` and `Key::prefix_upper_bound`

**Choice**: `collect_prefix_internal`, `BTree::collect_prefix_at`, `Database::collect_prefix`, and `Transaction::collect_prefix` all gain a `Dir` parameter.

Forward prefix iteration is unchanged: `seek(root, Some(&prefix))`, advance forward, stop when `!key.starts_with(prefix)`.

Reverse prefix iteration uses `Key::prefix_upper_bound` to compute the seek target. `prefix_upper_bound` returns the smallest `Key` strictly greater than all keys sharing the prefix, or `None` if no such key exists (the prefix's last subscript is at the ceiling of the collation). `collect_prefix_internal` then calls `seek_back(root, upper_bound.as_ref())` and advances backward, stopping when `!key.starts_with(prefix)`. If `seek_back` lands on a key that does not start with the prefix (because the upper bound itself exists in the tree), the `starts_with` check in the iteration loop skips it; this is the same pattern as the `Excluded` bound adjustment in `collect_internal`.

`Key::prefix_upper_bound` is built on `Subscript::successor`:

```rust
impl Subscript {
    /// Returns the smallest `Subscript` strictly greater than `self`,
    /// or `None` if `self` is the maximum (`Json` has no successor
    /// because its value space is unbounded in representation).
    pub fn successor(&self) -> Option<Subscript> { ... }
}
```

The collation order is `Boolean(0) < Number(1) < Char(2) < String(3) < Json(4)`. Successor stays within the same type; at each type's ceiling, `successor()` returns `None` and `prefix_upper_bound` falls back to popping. Rules:
- `Boolean(false)` -> `Boolean(true)`
- `Boolean(true)` -> `None` (ceiling of `Boolean`)
- `Number(n)` -> `Number(next_float(n))` (where `next_float` is the next representable `f64`; `NaN` is handled per `OrderedFloat`'s ordering)
- `Number(f64::INFINITY)` -> `None` (ceiling of `Number`)
- `Char(c)` -> `Char(next_char(c))` (next Unicode scalar value, skipping the surrogate gap)
- `Char(char::MAX)` -> `None` (ceiling of `Char`)
- `String(s)` -> `String(s + '\0')` (append a null byte; this is the smallest string strictly greater than `s` in lexicographic order; `String` has no ceiling)
- `Json(_)` -> `None` (last type in collation; value space has no computable maximum)

`Key::prefix_upper_bound` replaces the last subscript with its successor. If the last subscript's successor is `None`, it pops the last subscript and tries the second-to-last, and so on. If all subscripts are exhausted (every subscript is at its ceiling), it returns `None`, meaning the prefix range extends to the end of the key space.

```rust
impl Key {
    /// Returns the smallest `Key` strictly greater than all keys that
    /// start with `self`, or `None` if the prefix range extends to
    /// the end of the key space.
    pub fn prefix_upper_bound(&self) -> Option<Key> { ... }
}
```

**Alternatives considered**:
- `Cursor::seek_prefix_end(prefix)`: a dedicated cursor method that descends rightward within the prefix range. Avoids computing successors but adds a new traversal path to the cursor, increasing complexity and test surface for no performance gain (both approaches are `O(log n)`).

**Rationale**: Reuses the existing `seek_back` without any new cursor methods. `Subscript::successor` is well-defined within each type; it does not cross type boundaries (at each type's ceiling it returns `None`, and `prefix_upper_bound` pops that subscript). This keeps the successor logic simple and avoids surprising cross-type jumps. The computation is cheap (no allocation for most variants; `String` appends one byte). `Subscript::successor` is also generally useful beyond prefix iteration (e.g. exclusive-to-inclusive bound conversion).

## Risks / Trade-offs

**[Stack depth for very deep trees]**: The cursor stack is `O(height)`. For a B-tree with `min_degree = t`, height is `O(log_t(n))`. With `t >= 50` (typical for disk-backed B-trees), a tree with `10^9` keys has height ~5. The `SmallVec<[_; 8]>` inline capacity is more than sufficient. No heap allocation in practice.

**[Node mutation between advances]**: If a node in the cursor's stack is modified (e.g. split/merge from a concurrent write to a *different* global), the cursor loads the mutated node. For the same global, snapshot isolation in `Transaction` prevents this. For `Database`-level iteration without a transaction (locals), concurrent modification is a user error (single-writer assumption). Mitigation: document that `Database::collect` on locals is not safe under concurrent mutation.

**[Reverse iteration perf]**: `advance_back` on the cursor has the same `O(1)` amortized cost as forward `advance`. The `MergeState` reverse mode uses `next_back()` on `BTreeMap` ranges, which has the same `O(log n)` cost as forward `next()`. No regression expected.

**[`From<Option<&Key>>` lifetime ergonomics]**: `From<Option<&Key>>` for `Scan` requires cloning the key into an owned `Bounds`. This matches the current behavior (the existing `collect_internal` already clones `start`). No additional allocation.

**[Type-ceiling successors are `None`]**: `successor()` returns `None` at each type's ceiling (`Boolean(true)`, `Number(INFINITY)`, `Char(char::MAX)`, `Json(_)`), causing `prefix_upper_bound` to pop that subscript and try the next one. A prefix whose subscripts are all at their type ceiling returns `None`, meaning `seek_back(root, None)` starts from the end of the tree. The `starts_with` check still terminates correctly; the only cost is that the cursor may visit a few keys past the prefix range before stopping. In practice, ceiling-value subscripts are rare.

**[Rename churn]**: `collects` -> `collect` touches every call site, test, doc comment, and benchmark that references the method. The `From<Option<&Key>>` conversion means the signature change (`start` -> `scan`) is source-compatible, so the rename is the only breaking change. A single find-and-replace pass handles it.

## Open Questions

- Should the cursor be exposed as `pub` for advanced use cases (e.g. `rumps-query` doing custom iteration), or kept `pub(crate)` for now?
