# Parallel Transactions

## Goal

Enable concurrent transaction execution and commit for workloads spanning multiple globals.

## Current State (After Phase 1-2 + ORM Optimizations)

| Workload                   | Throughput   |
|----------------------------|--------------|
| Single global (core `set`) | ~56 Kelem/s  |
| Single global (100k batch) | ~51 Kelem/s  |
| Multi-global (100 globals) | ~113 Kelem/s |
| ORM insert                 | ~12 Kelem/s  |

This is competitive with other MUMPS implementations that provide full durability (`fsync` on commit). The ~113 Kelem/s multi-global throughput demonstrates that sharded caching is working.

## Remaining Bottlenecks

1. **Page cache lock** (`page/cache.rs`): single `RwLock` for entire cache (Phase 3)
2. **Transaction commit serialization**: commits are sequential (Phase 4)
3. **ORM overhead**: `to_pairs()` key construction, value serialization, name cloning (~4.5x slower than core)

## Implementation Checklist

### Phase 1: Parallel Write Application

- [x] Replace `try_for_each` with `buffer_unordered(16)` in `Transaction::commit`
- [x] Group writes by `Name` to avoid concurrent B-tree modifications (race condition fix)
- [x] Verify all existing tests pass
- [x] Run benchmarks: all pass including `kill_subtree_100`

**Files**: `transaction.rs`

**Note**: Writes within a single global must remain sequential (B-tree load-modify-save is not atomic). Parallelism applies across different globals only.

```rust
// Group by Name, parallel across Names, sequential within each Name
let mut by_name: BTreeMap<&Name, Vec<(&Key, &WriteOp)>> = BTreeMap::new();
writes.iter().for_each(|((name, key), op)| {
    by_name.entry(name).or_default().push((key, op));
});

stream::iter(by_name.into_iter())
    .map(|(name, ops)| async move {
        // Sequential within this Name
        stream::iter(ops).try_for_each(|(key, op)| { ... }).await
    })
    .buffer_unordered(16)  // Parallel across Names
    .try_for_each(|()| future::ready(Ok(())))
    .await?;
```

### Phase 2: Sharded B-Tree Node Cache + Arc<Node>

- [x] Create `ShardedNodeCache` struct with 64 shards
- [x] Implement `get`, `insert`, `remove`, `modify`, `try_modify` methods
- [x] Replace `nodes: RwLock<HashMap<...>>` with `ShardedNodeCache`
- [x] Update `load_node` to return `Arc<Node>` (cheap clone)
- [x] Use `Arc::make_mut` in `modify`/`try_modify` for copy-on-write
- [x] Handle multi-node operations (splits/merges) with separate shard locks
- [x] Verify all existing tests pass
- [x] Run benchmarks
- [x] Add multi-global benchmark

**Files**: `btree.rs`, `benches/orm_bench.rs`

**Results**: Massive improvements, especially for read operations:
- `orm_one`: **-66.5%** (reads now clone Arc pointer, not entire node)
- `orm_exists`: **-65.1%**
- `orm_all_1000`: **-67.0%**
- `orm_all_10000`: **-65.1%**
- `orm_delete`: **-41.1%**
- `orm_query_prefix`: **-37.7%**
- `orm_insert_many/1000`: **-33.5%**
- Multi-global inserts: ~18 Kelem/s throughput

```rust
const NODE_CACHE_SHARDS: usize = 64;

struct ShardedNodeCache {
    shards: [RwLock<HashMap<NodeId, Arc<Node>>>; NODE_CACHE_SHARDS],
}

impl ShardedNodeCache {
    // Returns Arc<Node> instead of cloning
    async fn get(&self, id: NodeId) -> Option<Arc<Node>> { ... }

    // Uses Arc::make_mut for copy-on-write semantics
    async fn modify<F, T>(&self, id: NodeId, f: F) -> Result<T>
    where F: FnOnce(&mut Node) -> T { ... }
}
```

#### Phase 2b: Batched B-Tree Operations

**Status**: SKIPPED

Attempted simple batching (sort + dedupe ancestors + insert one-by-one) but it provided no meaningful improvement and added overhead. Removed the code entirely.

**Findings**:

1. **Current throughput is ~50 Kelem/s for pure B-tree ops**, not 10 Kelem/s as originally estimated
2. **ORM operations are ~10-12 Kelem/s**; a 5x gap that warrants investigation
3. Simple batching (sort + dedupe ancestors) doesn't beat N individual inserts
4. True batched insertion (single tree traversal) is complex and likely not worth the effort given decent base throughput

**TODO**: Investigate why ORM bench is 5x slower than pure B-tree ops. Likely candidates:
- ORM serialization overhead (`to_pairs`, `from_pairs`)
- WAL logging per operation
- Transaction overhead
- Key/Value cloning

#### Phase 2c: Bulk Loading

**Status**: SKIPPED (for now)

Only useful for specific scenarios (large initial imports into empty trees). Not worth implementing unless there's a concrete use case. Current ~50 Kelem/s is adequate for most purposes.

### Phase 3: Sharded Page Cache

- [ ] Create `ShardedPageCache` struct with `N` shards (start with 16)
- [ ] Implement sharded `get`, `put`, `mark_dirty`, `flush`
- [ ] Replace `PageCache` internals with sharded version
- [ ] Accept approximate LRU semantics (per-shard eviction)
- [ ] Verify all existing tests pass
- [ ] Run benchmarks

**Files**: `page/cache.rs`

```rust
const CACHE_SHARDS: usize = 16;

struct ShardedPageCache {
    shards: [RwLock<LruCache<PageId, CachedPage>>; CACHE_SHARDS],
    dirty_shards: [RwLock<HashSet<PageId>>; CACHE_SHARDS],
}
```

### Phase 4: Concurrent Transaction Commits

- [ ] Refactor commit to: apply writes -> brief serialization point -> flush
- [ ] Move conflict validation inside `committed_writes` write lock
- [ ] Ensure WAL group commit batches concurrent flushes
- [ ] Verify all existing tests pass
- [ ] Add concurrent commit tests (disjoint keys, conflicting keys)
- [ ] Run benchmarks

**Files**: `transaction.rs`, `database.rs`

```rust
impl Transaction {
    async fn commit(self) -> Result<()> {
        // Apply writes in parallel (sharded locks)
        self.apply_writes_parallel().await?;

        // Brief serialization for conflict recording
        {
            let mut committed = self.db.txn_manager.committed_writes.write().await;
            self.validate_no_conflicts_with(&committed)?;
            committed.push(CommittedWriteSet { ... });
        }

        // Flush (group commit batches)
        self.db.flush_with_txn(self.id).await
    }
}
```

### Phase 5: Lock-Free Read Path (Optional)

- [ ] Evaluate if phases 1-4 achieve target throughput
- [ ] If needed: implement copy-on-write for node modifications
- [ ] Readers see immutable snapshots; writers create new versions

**Files**: `btree.rs`

## Testing

- [ ] Add `test_concurrent_commits_no_conflict`: N transactions, disjoint keys, all succeed
- [ ] Add `test_concurrent_commits_with_conflict`: N transactions, same key, first-committer-wins
- [ ] Add stress test: many threads hammering random keys
- [ ] Benchmark before/after each phase

## Constraints

- **No atomics**: All synchronization via `RwLock`/`Mutex`
- **No API changes**: Internal optimization only
- **ACID preserved**: Snapshot isolation semantics unchanged
- **Incremental**: Each phase must pass all existing tests and benchmarks

## Notes
- ALL tests and benchmarks work on master; any failures would be introduced on this branch and require investigation

## ORM Throughput (Separate from Parallel Transactions)

The ORM is ~4.5x slower than core operations. This is expected since one ORM record (e.g., `User`) maps to multiple keys (e.g., `[id]`, `[id, "name"]`, `[id, "email"]`, `[id, "age"]`).

**Completed optimizations:**
- [x] Removed dead `ops_count` lock from Transaction
- [x] Added `Transaction::set_many` for batch inserts with single lock
- [x] Updated ORM `insert`/`insert_many` to use `set_many`

**Remaining overhead sources:**

1. **`to_pairs()` key construction**: Each field clones the prefix key and pushes a subscript
2. **Value serialization** (`to_val()`): Converting Rust types to `Value`
3. **Name cloning**: `name.clone()` for each entry in write buffer

**Potential optimizations (brainstorm):**

1. **Grouped write buffer**: Change `writes: BTreeMap<(Name, Key), WriteOp>` to `writes: BTreeMap<Name, BTreeMap<Key, WriteOp>>`. Eliminates name cloning per entry; one clone per global instead.

2. **Key builder pattern**: Instead of `prefix.clone()` + `push()` for each field, provide a key builder that reuses allocations:
   ```rust
   let mut builder = KeyBuilder::from(prefix);
   pairs.push((builder.with("name"), self.name.to_val()));
   pairs.push((builder.with("email"), self.email.to_val()));
   ```

3. **Derive macro improvements**: Generate more efficient `to_pairs()` that minimizes allocations.

4. **Accept overhead**: ORM convenience has inherent cost. Users needing maximum throughput can use core API directly.

**Fair comparison note**: Comparing ORM insert (~12 Kelem/s for 4-field records) to core `set` (~56 Kelem/s for single keys) is apples-to-oranges. Per-record, ORM is ~12k records/s vs core needing 4 sets/record = ~14k records/s equivalent. The gap is actually small.
