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

Following needs to be fixed in storage layer (went nuts with async nonsense):

No compression code yet; it's in the TODO. Now I have the complete picture. Here's the performance improvement list:

---

## Storage Engine Performance Improvements

### 3. Fix `serialized_size()` and `would_fit()` (serialize-to-measure; high priority)

`Node::serialized_size()` converts `SmallVec` -> `Vec` (allocating `NodeRaw`), then serializes the entire node to a `Vec<u8>` just to call `.len()`. This runs on every insert to check if a split is needed.

`would_fit()` calls `serialized_size()` AND serializes the new key+value into separate `Vec`s.

Fix: track size incrementally. Maintain a `cached_size: usize` field on `Node`, updated on insert/remove. For `would_fit`, compute the marginal cost of the new entry (key subscripts * avg subscript size + value tag byte + value payload) without serializing. Alternatively, use `bincode::serialized_size()` which computes size without allocating the output buffer, but incremental tracking is better.

### 4. Eliminate `NodeRaw` serde intermediary

`Node` uses `SmallVec<[_; 8]>` but bincode serde goes through `NodeRaw` (which uses `Vec` and non-`Arc` values). Every serialize does `SmallVec` -> `Vec` allocation; every deserialize does `Vec` -> `SmallVec` copy. Implement custom `Serialize`/`Deserialize` directly on `Node` (or on a wrapper) that reads/writes the `SmallVec` in place, skipping the intermediate allocation.

### 5. Page cache: `get()` takes a write lock for reads

`PageCache::get()` acquires `self.cache.write().await` just to update LRU order. This serializes all reads behind a single writer. Two options:
- Use a concurrent-friendly LRU (e.g. `moka` or `quick_cache`) that doesn't need exclusive access for reads
- Separate the stats tracking from the LRU access (stats can use `AtomicU64`)

Also: `get()` acquires two locks sequentially (cache + stats). The stats lock should use atomics instead.

### 6. Two-layer cache redundancy

There are two caches: `DashMap<NodeId, Arc<Node>>` (`NodeCache` in btree.rs, used for locals) and `PageCache` (LRU with `RwLock`, used by `FileStorageEngine` for globals). They serve different populations (locals vs globals), but the `BTree` also calls into the engine's `read()` which hits `PageCache`. The `BTree` then separately caches the result in `NodeCache`. This means globals get double-cached. Unify or clearly separate the responsibilities.

### 7. `kill_internal` collects all keys then deletes one-by-one

`kill_internal` calls `collects_internal` to gather ALL matching keys into a `Vec`, sorts them in reverse, then deletes each key individually (each deletion is a full tree traversal + rebalance). For a subtree with N keys, this is O(N * log(N)) tree operations.

Fix: implement a subtree-drop operation that removes an entire subtree in one pass, deallocating child pages directly without individually searching for each key.

### 8. `get_next_internal` does two tree traversals

`get_next_internal` (used by `@QUERY`) first calls `collects_find_next` to find the next key path, then calls `get` to retrieve its value. Both traverse the tree from root. These should be fused into a single traversal that finds the next key and reads its value in one pass.

### 9. File I/O: single `RwLock<File>` for all reads and writes (high priority)

`FileStorageEngine` wraps the data file in `Arc<RwLock<File>>`. Every page read acquires a write lock (because `seek` + `read_exact` must be atomic). This serializes all disk I/O.

Fix: use `pread`/`pwrite` (positional I/O) which don't need seeking and can run concurrently. On Linux, `pread64`/`pwrite64` are atomic at the page level. Tokio's `spawn_blocking` + `std::os::unix::fs::FileExt` is one path; `io_uring` via `tokio-uring` is the high-performance path.

### 10. Full-page reads and writes (always `PAGE_SIZE` bytes)

`read()` always reads `PAGE_SIZE` bytes and deserializes the whole buffer. `write_page_to_disk()` serializes then pads to `PAGE_SIZE`. A small leaf node that serializes to 200 bytes still reads/writes 4096 bytes.

When compression is added (per your TODO), this becomes critical: store the compressed length in a small header, read only the compressed bytes, decompress on demand. Without compression, at least consider variable-length records with a page directory for multi-record pages, or prefix compression for keys that share common prefixes (which is the norm in RUMPS's hierarchical key model).

### 11. `Node::clone()` is expensive and pervasive

`Node` contains `SmallVec<[Key; 8]>`, `SmallVec<[Arc<NodeData>; 8]>`, `SmallVec<[NodeId; 8]>`. Cloning a `Node` deep-copies all the `SmallVec`s (though `NodeData` is behind `Arc`). `btree.rs` clones nodes frequently: `Arc::try_unwrap(...).unwrap_or_else(|arc| (*arc).clone())` appears 4 times. The engine's `read()` also clones: it returns `Ok((*cached).clone())` from the cache hit path.

Fix: return `Arc<Node>` from reads instead of cloned `Node`. For mutations, use copy-on-write; only clone when `Arc::strong_count() > 1`.

### 12. WAL writer serializes with `bincode::serialize_into` but could use zero-copy

`WalWriter::write_record` serializes each WAL record into a buffer via bincode. For `Set` records that carry the full serialized node, this is a double serialization (node was already serialized for the page write). Consider writing the raw page bytes directly as the WAL payload.

### 13. Flush writes dirty pages sequentially then syncs

`flush()` uses `FuturesUnordered` to write dirty pages (good), but then does a single `sync_all()`. For many dirty pages, consider batched writes with `writev` (vectored I/O) to reduce syscall overhead. The comment in the code ("sequential for now; could parallelize") is already outdated since it uses `FuturesUnordered`, but the underlying writes still contend on the single `RwLock<File>` (see #9).

### 14. Dirty set uses separate `RwLock<HashSet>`

`PageCache` tracks dirty pages in a `RwLock<HashSet<PageId>>` separate from the LRU cache. This means every `put()` acquires three locks: cache, dirty_set, and stats. Fold the dirty bit into the `CachedPage` entry itself (it's already a struct; add a `dirty: bool` field) to eliminate the separate lock.
