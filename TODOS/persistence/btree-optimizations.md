# B-Tree Performance Optimizations

This document outlines actionable performance improvements for the B-tree implementation in `rumps-storage`. All optimizations preserve RUMPS collation order (Boolean < Number < Char < String < JSON).

## Overview

| Optimization                    | Expected Impact             | Complexity | Collation Safe |
|---------------------------------|-----------------------------|------------|----------------|
| Lock contention reduction       | High (concurrent workloads) | Medium     | Yes            |
| Subscript comparison fast-path  | High (all workloads)        | Low        | Yes            |
| Reduce allocations via SmallVec | Medium                      | Low        | Yes            |
| Lazy ancestor flag updates      | Medium (write-heavy)        | Medium     | Yes            |

---

## 1. Lock Contention Reduction ✅

**Goal:** Replace `RwLock<HashMap>` shards with lock-free concurrent map.

**Current:** `ShardedNodeCache` in `btree.rs:52-121` uses 64 `RwLock<HashMap<NodeId, Arc<Node>>>` shards. Every `load_node()` acquires a read lock via `.await`, and cache misses upgrade to write locks, causing contention and async overhead.

**Solution:** Use `dashmap` crate for concurrent access with internal fine-grained sharding.

**Why this helps:** `DashMap` operations are synchronous (no `.await` needed) because they use internal lock sharding rather than explicit async locks. The current implementation pays async scheduling overhead for what are essentially instant memory operations. With `DashMap`, concurrent readers never block each other, and the operations complete immediately without yielding to the async runtime.

### Implementation

**Step 1:** Add dependency to `crates/rumps-storage/Cargo.toml`:

```toml
[dependencies]
dashmap = "5"
```

**Step 2:** Replace `ShardedNodeCache` with `DashMap`:

```rust
use dashmap::DashMap;

struct NodeCache {
    inner: DashMap<NodeId, Arc<Node>>,
}

impl NodeCache {
    fn new() -> Self {
        Self {
            inner: DashMap::with_capacity(1024),
        }
    }

    fn get(&self, id: NodeId) -> Option<Arc<Node>> {
        self.inner.get(&id).map(|r| Arc::clone(&*r))
    }

    fn insert(&self, id: NodeId, node: Node) {
        self.inner.insert(id, Arc::new(node));
    }

    fn remove(&self, id: NodeId) -> Option<Arc<Node>> {
        self.inner.remove(&id).map(|(_, v)| v)
    }

    /// Modifies a node in-place using entry API.
    fn modify<F, T>(&self, id: NodeId, f: F) -> Result<T>
    where
        F: FnOnce(&mut Node) -> T,
    {
        self.inner
            .get_mut(&id)
            .map(|mut r| f(Arc::make_mut(&mut *r)))
            .ok_or_else(|| StorageError::NodeNotFound(id.into()))
    }

    fn len(&self) -> usize {
        self.inner.len()
    }
}
```

**Step 3:** Update `BTree` struct:

```rust
pub(crate) struct BTree {
    nodes: NodeCache,  // was: ShardedNodeCache
    // ... rest unchanged
}
```

**Step 4:** Remove all `.await` calls on node cache operations (they become sync).

### Verification

- Run existing tests; all should pass unchanged
- Benchmark concurrent access with `criterion`:
  ```rust
  // Spawn N tasks doing concurrent reads/writes to different globals
  // Measure throughput before/after
  ```

---

## 2. Subscript Comparison Fast-Path ✅

**Goal:** Short-circuit cross-type comparisons using discriminant, avoiding deep match.

**Current:** `Subscript::cmp()` in `key.rs:364-398` has a 25-arm match covering all type pairs. Cross-type comparisons (e.g., `Number` vs `String`) are common but don't need value inspection.

**Solution:** Use `#[repr(u8)]` with explicit discriminant values matching collation order, then read the tag byte directly via pointer cast.

### Implementation

**Step 1:** Add `#[repr(u8)]` to `Subscript` enum in `key.rs` with explicit discriminants:

```rust
/// Subscripts with explicit discriminants matching collation order.
/// SAFETY: `#[repr(u8)]` guarantees the first byte is the discriminant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Subscript {
    Boolean(bool) = 0,
    Number(OrderedFloat<f64>) = 1,
    Char(char) = 2,
    String(SmolStr) = 3,
    Json(serde_json::Value) = 4,
}
```

**Step 2:** Add `discriminant()` method to `Subscript`:

```rust
impl Subscript {
    /// Returns the discriminant byte directly.
    ///
    /// SAFETY: `#[repr(u8)]` guarantees the enum's memory layout has the
    /// discriminant as its first byte.
    #[inline]
    const fn discriminant(&self) -> u8 {
        // SAFETY: #[repr(u8)] ensures discriminant is first byte
        unsafe { *(self as *const Self as *const u8) }
    }
}
```

**Step 3:** Rewrite `Ord` implementation:

```rust
impl Ord for Subscript {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        let self_disc = self.discriminant();
        let other_disc = other.discriminant();

        // Fast path: different types resolve by discriminant alone
        match self_disc.cmp(&other_disc) {
            cmp::Ordering::Less => cmp::Ordering::Less,
            cmp::Ordering::Greater => cmp::Ordering::Greater,
            cmp::Ordering::Equal => {
                // Same type: compare values
                match (self, other) {
                    (Self::Boolean(a), Self::Boolean(b)) => a.cmp(b),
                    (Self::Number(a), Self::Number(b)) => a.cmp(b),
                    (Self::Char(a), Self::Char(b)) => a.cmp(b),
                    (Self::String(a), Self::String(b)) => a.cmp(b),
                    (Self::Json(a), Self::Json(b)) => {
                        a.to_string().cmp(&b.to_string())
                    }
                    // Unreachable: same discriminant means same variant
                    _ => cmp::Ordering::Equal,
                }
            }
        }
    }
}
```

**Step 4:** Add `#[inline]` to `PartialOrd::partial_cmp()`:

```rust
impl PartialOrd for Subscript {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}
```

### Verification

- Existing collation tests in `key.rs` (`test_subscript_cross_type_ordering`, `test_subscript_mumps_collation`) must pass unchanged
- Add benchmark:
  ```rust
  #[bench]
  fn bench_subscript_cmp_cross_type(b: &mut Bencher) {
      let num = subscript!(100);
      let str = subscript!("ABC");
      b.iter(|| num.cmp(&str));
  }

  #[bench]
  fn bench_subscript_cmp_same_type(b: &mut Bencher) {
      let n1 = subscript!(100);
      let n2 = subscript!(200);
      b.iter(|| n1.cmp(&n2));
  }
  ```

---

## 3. Reduce Allocations via SmallVec

**Goal:** Inline small vectors in `Node` to avoid heap allocation for typical leaf nodes.

**Current:** `Node` uses `Vec<Key>`, `Vec<Arc<NodeData>>`, `Vec<NodeId>`. For `min_degree=3`, nodes have 2-5 keys. Every node allocates 3 separate heap buffers.

**Solution:** Use `SmallVec` with inline capacity matching typical node size.

### Implementation

**Step 1:** Add dependency to `crates/rumps-storage/Cargo.toml`:

```toml
[dependencies]
smallvec = { version = "1", features = ["serde"] }
```

**Step 2:** Define type aliases in `node.rs`:

```rust
use smallvec::SmallVec;

/// Inline capacity for node vectors.
/// For `min_degree=3`, max keys = `2*3-1 = 5`.
/// Use 8 for headroom with larger degrees.
const NODE_INLINE_CAP: usize = 8;

type KeyVec = SmallVec<[Key; NODE_INLINE_CAP]>;
type ValueVec = SmallVec<[Arc<NodeData>; NODE_INLINE_CAP]>;
type ChildVec = SmallVec<[NodeId; NODE_INLINE_CAP + 1]>;  // children = keys + 1
```

**Step 3:** Update `Node` struct:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "NodeRaw", into = "NodeRaw")]
pub(crate) struct Node {
    pub(crate) keys: KeyVec,
    pub(crate) children: ChildVec,
    pub(crate) values: ValueVec,
    pub(crate) is_leaf: bool,
}
```

**Step 4:** Update `Node::new_leaf()` and `Node::new_internal()`:

```rust
impl Node {
    pub(crate) fn new_leaf() -> Self {
        Self {
            keys: SmallVec::new(),
            children: SmallVec::new(),
            values: SmallVec::new(),
            is_leaf: true,
        }
    }

    pub(crate) fn new_internal() -> Self {
        Self {
            keys: SmallVec::new(),
            children: SmallVec::new(),
            values: SmallVec::new(),
            is_leaf: false,
        }
    }
}
```

**Step 5:** Update `NodeRaw` similarly for serialization:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NodeRaw {
    keys: Vec<Key>,        // Keep as Vec for serde compatibility
    children: Vec<NodeId>,
    values: Vec<NodeData>,
    is_leaf: bool,
}

impl From<NodeRaw> for Node {
    fn from(raw: NodeRaw) -> Self {
        Self {
            keys: raw.keys.into_iter().collect(),
            children: raw.children.into_iter().collect(),
            values: raw.values.into_iter().map(Arc::new).collect(),
            is_leaf: raw.is_leaf,
        }
    }
}

impl From<Node> for NodeRaw {
    fn from(node: Node) -> Self {
        Self {
            keys: node.keys.into_vec(),
            children: node.children.into_vec(),
            values: node.values.into_iter().map(|a| {
                Arc::try_unwrap(a).unwrap_or_else(|a| (*a).clone())
            }).collect(),
            is_leaf: node.is_leaf,
        }
    }
}
```

**Step 6:** Update `split_node()` in `btree.rs` to use `SmallVec::split_off()`:

```rust
// SmallVec supports split_off, so existing code works.
// Just ensure the return type is SmallVec, not Vec.
let right_keys: KeyVec = keys.split_off(mid + 1);
```

### Verification

- All existing tests pass
- Memory benchmark:
  ```rust
  #[test]
  fn node_size_check() {
      // Verify Node struct stays reasonably sized
      assert!(std::mem::size_of::<Node>() < 512);
  }
  ```

---

## 4. Lazy Ancestor Flag Updates

**Goal:** Defer `has_descendants` flag maintenance until transaction commit, reducing repeated tree traversals.

**Current:** `set_internal()` calls `ensure_ancestors()` on every insert (`btree.rs:983`). For `key![1, 2, 3, 4]`, this creates/updates ancestors `[1]`, `[1,2]`, `[1,3]` individually, each traversing from root.

**Solution:** Collect ancestor keys in transaction write buffer; batch-update flags at commit.

### Snapshot Isolation Analysis

**This optimization is safe.** The transaction implementation in `transaction.rs` already provides snapshot isolation:

1. **Writes are buffered** until commit (`writes: BTreeMap<(Name, Key), WriteOp>`)
2. **Reads check buffer first**, then fall through to DB snapshot
3. **Ancestor flags are not updated** until `db.set_with_txn()` at commit time

Within a transaction, `data()` on ancestor keys sees:
- Buffered writes for that exact key (if any)
- Buffered descendants (Sets under the ancestor prefix)
- DB snapshot state (for non-buffered keys)

The `data()` method correctly merges buffered descendants with snapshot state, so callers see accurate `HasDescendants` / `Both` status even before commit. This means lazy ancestor updates don't change observable behavior; they only batch what currently happens sequentially at commit.

### Implementation

**Step 1:** Add ancestor tracking to `Transaction` in `transaction.rs`:

```rust
pub struct Transaction {
    // ... existing fields

    /// Keys that need `has_descendants=true` set at commit.
    /// Populated lazily during `set()` calls.
    pending_ancestors: Arc<RwLock<HashSet<(Name, Key)>>>,
}
```

**Step 2:** Modify `Transaction::set()` to collect ancestors instead of immediate update:

```rust
impl Transaction {
    pub async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()> {
        // Buffer the write (existing logic)
        self.writes.write().await.insert(
            (name.clone(), key.clone()),
            WriteOp::Set(NodeData::with_value(value)),
        );

        // Collect ancestors for deferred update
        let ancestors = key.ancestors();
        let mut pending = self.pending_ancestors.write().await;
        ancestors.into_iter().for_each(|anc| {
            pending.insert((name.clone(), anc));
        });

        Ok(())
    }
}
```

**Step 3:** Add batch ancestor update in `Database::apply_transaction()`:

```rust
impl Database {
    pub(crate) async fn apply_transaction(&self, txn: &Transaction) -> Result<()> {
        // 1. Apply buffered writes (existing logic)
        // ...

        // 2. Batch-update ancestor flags
        let ancestors = txn.pending_ancestors.read().await;

        // Group by name for efficiency
        let by_name = ancestors.iter().fold(
            HashMap::<Name, Vec<Key>>::new(),
            |mut acc, (name, key)| {
                acc.entry(name.clone()).or_default().push(key.clone());
                acc
            },
        );

        // Update each global's ancestors in one pass
        futures::stream::iter(by_name)
            .then(|(name, keys)| async move {
                let root = self.get_or_create_root(&name).await?;
                self.batch_set_descendants_flag(root, &keys).await
            })
            .try_collect::<()>()
            .await
    }
}
```

**Step 4:** Add `batch_set_descendants_flag()` to `BTree`:

```rust
impl BTree {
    /// Sets `has_descendants=true` for multiple keys in one traversal.
    /// Keys should be sorted shortest-first for efficiency.
    pub(crate) async fn batch_set_descendants_flag(
        &self,
        root: NodeId,
        keys: &[Key],
    ) -> Result<()> {
        // Sort keys by length (ancestors before descendants)
        let mut sorted: Vec<_> = keys.to_vec();
        sorted.sort_by_key(|k| k.len());

        // Process in order; skip keys whose ancestors already processed
        futures::stream::iter(sorted)
            .try_fold(HashSet::new(), |mut processed, key| async move {
                // Skip if a prefix was already processed
                let dominated = key
                    .ancestors()
                    .iter()
                    .any(|anc| processed.contains(anc));

                if !dominated {
                    self.ensure_ancestor_flag(root, &key).await?;
                    processed.insert(key);
                }

                Ok(processed)
            })
            .await?;

        Ok(())
    }

    /// Ensures a single key exists with `has_descendants=true`.
    /// Cheaper than `set_internal()` since it doesn't traverse ancestors.
    async fn ensure_ancestor_flag(&self, root: NodeId, key: &Key) -> Result<()> {
        let data = NodeData::with_descendants();
        self.set_at_node(root, key, data).await
    }
}
```

**Step 5:** Remove `ensure_ancestors()` call from `set_internal()`:

```rust
impl BTree {
    async fn set_internal(
        &self,
        root: NodeId,
        key: &Key,
        data: NodeData,
    ) -> Result<NodeId> {
        // REMOVED: let root = self.ensure_ancestors(root, key).await?;

        // Directly insert the key
        self.set_at_node(root, key, data).await
    }
}
```

### Verification

- All existing tests pass (ancestor semantics unchanged)
- Benchmark multi-level key insertion:
  ```rust
  #[bench]
  fn bench_deep_key_insert(b: &mut Bencher) {
      let db = Database::in_memory().unwrap();
      b.iter(|| {
          block_on(db.transaction(|txn| async move {
              // Insert 100 keys at depth 5
              futures::stream::iter(0..100)
                  .then(|i| async move {
                      txn.set(&global!("TEST"), &key![1, 2, 3, 4, i], value!(i)).await
                  })
                  .try_collect::<()>()
                  .await
          }))
      });
  }
  ```

---

# Tier 2: Medium-Impact Optimizations

These optimizations provide meaningful gains but require more invasive changes.

| Optimization              | Expected Impact         | Complexity | Collation Safe |
|---------------------------|-------------------------|------------|----------------|
| Incremental size tracking | Medium (disk-backed)    | Medium     | Yes            |
| Interleaved node layout   | Medium (cache locality) | High       | Yes            |

---

## 5. Serialization: Incremental Size Tracking

**Goal:** Avoid calling `bincode::serialize()` just to measure node size.

**Current:** `Node::serialized_size()` serializes the entire node to measure its byte length (`node.rs:257-267`). Called during `would_fit()` checks before insertion.

**Solution:** Track serialized size incrementally as keys/values are added/removed.

### Implementation

**Step 1:** Add `cached_size` field to `Node`:

```rust
pub(crate) struct Node {
    pub(crate) keys: KeyVec,
    pub(crate) children: ChildVec,
    pub(crate) values: ValueVec,
    pub(crate) is_leaf: bool,

    /// Cached serialized size in bytes. Updated on mutation.
    /// `None` means cache is invalidated and needs recomputation.
    cached_size: Option<usize>,
}
```

**Step 2:** Add size estimation helpers:

```rust
impl Node {
    /// Base overhead for an empty node (flags, vec length prefixes, etc.).
    const BASE_OVERHEAD: usize = 32; // Measure empirically

    /// Estimates serialized size of a key.
    fn key_size(key: &Key) -> usize {
        // Length prefix (8 bytes) + subscripts
        8 + key.iter().map(Self::subscript_size).sum::<usize>()
    }

    /// Estimates serialized size of a subscript.
    fn subscript_size(sub: &Subscript) -> usize {
        // Tag byte + payload
        1 + match sub {
            Subscript::Boolean(_) => 1,
            Subscript::Number(_) => 8,
            Subscript::Char(_) => 4,
            Subscript::String(s) => 8 + s.len(),
            Subscript::Json(j) => 8 + j.to_string().len(),
        }
    }

    /// Estimates serialized size of NodeData.
    fn nodedata_size(data: &NodeData) -> usize {
        // Tag byte + optional value
        match &data.value {
            None => 1,
            Some(v) => 1 + Self::value_size(v),
        }
    }

    fn value_size(val: &Value) -> usize {
        // Similar to subscript_size
        1 + match val {
            Value::Boolean(_) => 1,
            Value::Integer(_) => 8,
            Value::Float(_) => 8,
            Value::Char(_) => 4,
            Value::String(s) => 8 + s.len(),
            Value::Json(j) => 8 + j.to_string().len(),
        }
    }

    /// Recomputes cached size from scratch.
    fn recompute_size(&mut self) {
        let keys_size: usize = self.keys.iter().map(Self::key_size).sum();
        let values_size: usize = self.values.iter()
            .map(|v| Self::nodedata_size(v))
            .sum();
        let children_size = self.children.len() * 8; // NodeId = u64

        self.cached_size = Some(
            Self::BASE_OVERHEAD + keys_size + values_size + children_size
        );
    }

    /// Returns estimated serialized size.
    pub(crate) fn estimated_size(&mut self) -> usize {
        match self.cached_size {
            Some(s) => s,
            None => {
                self.recompute_size();
                self.cached_size.unwrap()
            }
        }
    }

    /// Invalidates size cache. Call after mutations.
    pub(crate) fn invalidate_size(&mut self) {
        self.cached_size = None;
    }
}
```

**Step 3:** Update mutation sites to invalidate cache:

```rust
// In btree.rs, after any node.keys.push(), node.values.remove(), etc.:
node.invalidate_size();
```

**Step 4:** Replace `would_fit()` with estimation:

```rust
impl Node {
    pub(crate) fn would_fit_estimated(
        &mut self,
        key: &Key,
        value: &NodeData,
        max_size: usize,
    ) -> bool {
        let current = self.estimated_size();
        let entry_size = Self::key_size(key) + Self::nodedata_size(value);
        current + entry_size <= max_size
    }
}
```

### Verification

- Compare `estimated_size()` vs `serialized_size()` for accuracy
- Allow ~10% margin of error (estimation is conservative)
- Benchmark insertion with size checks

---

## 6. Interleaved Node Layout

**Goal:** Improve cache locality by storing key-value pairs contiguously.

**Current:** `Node` stores `keys: Vec<Key>`, `values: Vec<Arc<NodeData>>` separately. Accessing a key-value pair requires two pointer chases to different memory regions.

**Solution:** Store entries as a single vector of `(Key, Arc<NodeData>)` tuples.

### Implementation

**Step 1:** Define entry type in `node.rs`:

```rust
/// A single entry in a B-tree node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) key: Key,
    pub(crate) data: Arc<NodeData>,
}

impl Entry {
    pub(crate) fn new(key: Key, data: Arc<NodeData>) -> Self {
        Self { key, data }
    }
}
```

**Step 2:** Update `Node` struct:

```rust
pub(crate) struct Node {
    /// Key-value entries, sorted by key.
    pub(crate) entries: SmallVec<[Entry; NODE_INLINE_CAP]>,
    /// Child node IDs (empty for leaves).
    /// For n entries, internal nodes have n+1 children.
    pub(crate) children: ChildVec,
    pub(crate) is_leaf: bool,
}
```

**Step 3:** Update accessors throughout `btree.rs`:

```rust
// Before:
let key = node.keys.get(pos)?;
let val = node.values.get(pos)?;

// After:
let entry = node.entries.get(pos)?;
let key = &entry.key;
let val = &entry.data;
```

**Step 4:** Update binary search:

```rust
// Before:
node.keys.binary_search(key)

// After:
node.entries.binary_search_by(|e| e.key.cmp(key))
```

**Step 5:** Update `split_node()`:

```rust
// Before:
let right_keys = keys.split_off(mid + 1);
let right_vals = values.split_off(mid + 1);
let med_key = keys.pop()?;
let med_val = values.pop()?;

// After:
let right_entries = entries.split_off(mid + 1);
let med_entry = entries.pop()?;
let (med_key, med_val) = (med_entry.key, med_entry.data);
```

**Step 6:** Update serialization (`NodeRaw`):

```rust
#[derive(Serialize, Deserialize)]
struct EntryRaw {
    key: Key,
    data: NodeData,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct NodeRaw {
    entries: Vec<EntryRaw>,
    children: Vec<NodeId>,
    is_leaf: bool,
}
```

### Verification

- All existing tests pass
- Benchmark traversal-heavy workloads (iteration, range queries)

---

## Execution Order

### Tier 1 (High Impact, Lower Risk)
1. **Subscript comparison fast-path** (lowest risk, immediate benefit)
2. **SmallVec for nodes** (low risk, reduces allocator pressure)
3. **Lock contention reduction** (medium risk, benefits concurrent workloads)
4. **Lazy ancestor updates** (higher risk, most invasive change)

### Tier 2 (Medium Impact, Higher Complexity)
5. **Incremental size tracking** (medium complexity, helps disk-backed)
6. **Interleaved node layout** (high complexity, improves cache locality)

Each optimization should be implemented, tested, and benchmarked independently before moving to the next.
