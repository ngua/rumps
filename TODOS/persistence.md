# RUMPS Persistence TODOs

**NOTE**: For storage layer architecture and implementation details, see `docs/storage.org`.

---

## Future Considerations (Post-Goal 1)

These are the current next steps (post persistence and basic DB):

- **Networking**: Client-server protocol for remote access
- **Parallel Transactions**: Enable concurrent transaction execution to improve throughput (see [persistence/parallel-transactions.md](persistence/parallel-transactions.md))

These are the potential next steps (post networking):

- **Compression**: Compress nodes/pages to save disk space (see [persistence/compression.md](persistence/compression.md))
- **Encryption**: Optional encryption at rest (see [persistence/encryption.md](persistence/encryption.md))

These are future considerations not yet planned:
- **Advanced Concurrency**: Lock-free data structures, optimistic concurrency control (current plan uses Mutex for commit serialization)
- **Multi-Version Concurrency Control (MVCC)**: Full MVCC for better read concurrency (current plan uses snapshot isolation)
- **Savepoints**: Nested transactions with partial rollback
- **Replication**: Multi-node deployment with data replication
- **Distributed Transactions**: Two-phase commit for multi-node transactions
- **Configuration Changes**: Runtime overrides, persistent reconfiguration, and data rebuilds (see [persistence/reconfigure.md](persistence/reconfigure.md))
- **`COLLECT` Enhancements**:
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

## Fixes

### ~~1. Eliminate all `async_recursion`~~ DONE

All 27 `#[async_recursion]` sites converted to iterative loops with explicit stacks. `async_recursion` crate removed. Three subtree-walking methods (`delete_subtree`, `collect_keys_from_node`, `check_descendants_from_node`) use `FuturesUnordered` for concurrent child loads.

### ~~2. Replace `async_trait` with native async / remove trait objects~~ DONE

`AsyncStorageEngine` trait deleted; methods are now inherent on `FileStorageEngine`. `NodeAllocator` trait replaced with an enum (`InMemory` / `Disk`). `async-trait` crate removed. No more `dyn` dispatch or boxed futures on the storage hot path.

### ~~3. Fix `serialized_size()` and `would_fit()`~~ DELETED

Methods removed. Split decisions use key count (`keys.len() == max_keys`), not serialized size, so these were dead code. When page-size-based splitting is needed (e.g. for compression or variable-length pages), implement it with incremental size tracking from the start rather than serialize-to-measure.

### ~~4. Eliminate `NodeRaw` serde intermediary~~ DONE

`NodeRaw` deleted. `Node` now derives `Serialize`/`Deserialize` directly; `SmallVec` fields use their native serde support, and the `Arc<NodeData>` values field uses custom `ser_values`/`de_values` helpers that serialize through the `Arc` and wrap on deserialize. No intermediate `Vec` allocations.

### ~~5. Page cache: `get()` takes a write lock for reads~~ DONE

Replaced `RwLock<LruCache>` with `moka::sync::Cache` (lock-free reads, TinyLFU admission). Stats use `AtomicU64`. All cache operations are now sync (no `.await`).

### 6. Two-layer cache redundancy

There are two caches: `DashMap<NodeId, Arc<Node>>` (`NodeCache` in btree.rs, used for locals) and `PageCache` (LRU with `RwLock`, used by `FileStorageEngine` for globals). They serve different populations (locals vs globals), but the `BTree` also calls into the engine's `read()` which hits `PageCache`. The `BTree` then separately caches the result in `NodeCache`. This means globals get double-cached. Unify or clearly separate the responsibilities.

### 7. `kill_internal` collects all keys then deletes one-by-one

`kill_internal` calls `collects_internal` to gather ALL matching keys into a `Vec`, sorts them in reverse, then deletes each key individually (each deletion is a full tree traversal + rebalance). For a subtree with N keys, this is O(N * log(N)) tree operations.

Fix: implement a subtree-drop operation that removes an entire subtree in one pass, deallocating child pages directly without individually searching for each key.

### ~~8. `get_next_internal` does two tree traversals~~ DONE

Fused into a single traversal by calling `find_leftmost_entry`/`find_successor_entry` directly (which already return key + data). `collects` benchmarks improved ~33%.

### ~~9. File I/O: single `RwLock<File>` for all reads and writes (high priority)~~ DONE

`Arc<RwLock<tokio::fs::File>>` replaced with `Arc<std::fs::File>`. All runtime I/O uses positional reads/writes (`FileExt::read_exact_at`/`write_all_at`, i.e. `pread`/`pwrite`) via `tokio::task::spawn_blocking`. No lock contention; concurrent page reads and writes proceed independently. Startup I/O (superblock loading, initial page writes) still uses async tokio I/O on a temporary `tokio::fs::File`, converted to `std::fs::File` before construction.

### 10. Full-page reads and writes (always `PAGE_SIZE` bytes; high priority)

`read()` always reads `PAGE_SIZE` bytes and deserializes the whole buffer. `write_page_to_disk()` serializes then pads to `PAGE_SIZE`. A small leaf node that serializes to 200 bytes still reads/writes 4096 bytes.

When compression is added (per your TODO), this becomes critical: store the compressed length in a small header, read only the compressed bytes, decompress on demand. Without compression, at least consider variable-length records with a page directory for multi-record pages, or prefix compression for keys that share common prefixes (which is the norm in RUMPS's hierarchical key model).

### ~~11. `Node::clone()` is expensive and pervasive (high priority)~~ DONE

`FileStorageEngine::read()` now returns `Arc<Node>` directly; cache hits return the Arc without cloning. `BTree::load_node()` uses `insert_arc` to avoid cloning when loading from storage. The `Arc::try_unwrap` sites in split/merge code are correct copy-on-write and were left as-is.

### ~~12. WAL writer serializes with `bincode::serialize_into` but could use zero-copy~~ DELETED

Not a real issue. `WalRecord::Set` carries `NodeData` (one key's value), not a full serialized `Node`. The WAL logs the logical operation while the page write serializes the entire B-tree node; different data at different granularities. No double serialization.

### 13. Flush writes dirty pages sequentially then syncs

`flush()` uses `FuturesUnordered` to write dirty pages (good), but then does a single `sync_all()`. For many dirty pages, consider batched writes with `writev` (vectored I/O) to reduce syscall overhead. Now that #9 is resolved, the underlying `pwrite` calls no longer contend on a lock, so the `FuturesUnordered` writes are truly concurrent. Remaining opportunity: vectored I/O (`writev`) to batch multiple page writes into fewer syscalls.

### ~~14. Dirty set uses separate `RwLock<HashSet>`~~ DONE

Dirty tracking folded into cache entries as `Arc<AtomicBool>`. No separate lock; `mark_dirty`/`mark_flushed`/`mark_clean` flip the atomic without re-inserting. Dirty evictions captured by moka's eviction listener.
