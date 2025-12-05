# Compression

## Overview

This document outlines the design considerations for adding page/node compression to RUMPS storage.

**Difficulty assessment**: Medium-High - harder than encryption due to variable output size and B-tree integration challenges.

---

## Key Challenge: Variable Output Size

Encryption is size-preserving (input ≈ output), so it slots in transparently. Compression produces **variable-size output** but pages are **fixed-size blocks** - this creates fundamental architectural tension.

### Options

| Approach                      | Pros                              | Cons                                        | Complexity |
|-------------------------------|-----------------------------------|---------------------------------------------|------------|
| Single-page compression       | Bounded output, simple alignment  | Can't compress if ratio > ~95%              | Low        |
| Multi-page spans              | Better compression for large data | Complex allocator, fragmentation, recovery  | High       |
| Compress within node (values) | Better ratio, per-value control   | More overhead, lock contention              | Medium     |

**Recommendation**: Single-page compression with fallback to uncompressed.

---

## Architecture

### What makes compression tractable

1. **Single-page approach works**: Compress within page boundaries, fall back to uncompressed if ratio exceeds threshold
2. **WAL already variable-size**: `wal/format.rs:47-64` uses length-prefixed records
3. **Good algorithm options**: `zstd` level 3 gives ~60% ratio at 500+ MB/s

### Complexity factors

1. **B-tree split decisions**: `Node::would_fit()` (`node.rs:298-317`) checks if a node needs splitting using raw serialized size - needs compressed size instead

2. **Serialization overhead**: `serialized_size()` (`node.rs:257-267`) does full bincode serialize on every call; adding compression would be expensive unless cached

3. **Cache strategy**: `PageCache` (`page/cache.rs:55-82`) stores `Arc<Node>` uncompressed - compress only at flush (simplest approach)

4. **No multi-page spans**: Allocator (`page/allocator.rs:206-275`) allocates single pages only

5. **Variable compression ratios**:
   - JSON/large strings: ~60% ratio
   - Small integers/booleans: ~20% ratio
   - Already-compressed data: may expand

---

## Algorithm Selection

| Algorithm | Crate    | Speed      | Ratio  | Recommendation         |
|-----------|----------|------------|--------|------------------------|
| LZ4       | `lz4`    | 500+ MB/s  | 40-50% | Good for pages         |
| zstd      | `zstd`   | 300+ MB/s  | 60-70% | Best balance           |
| snappy    | `snap`   | 500+ MB/s  | 30-40% | Too low ratio          |
| brotli    | `brotli` | 50 MB/s    | 70%+   | Too slow for hot path  |

**Recommendation**: `zstd` level 3 (default speed, good ratio).

---

## Implementation

### Page-level compression

```rust
// Write path
let serialized = bincode::serialize(&node)?;
let compressed = zstd::encode_all(&serialized, 3)?;

if compressed.len() <= PAGE_SIZE * 95 / 100 {
    write_page_with_flag(page_id, &compressed, COMPRESSED)
} else {
    write_page_with_flag(page_id, &serialized, UNCOMPRESSED)
}

// Read path
let (data, flags) = read_page(page_id)?;
let serialized = if flags.contains(COMPRESSED) {
    zstd::decode_all(&data)?
} else {
    data
};
let node: Node = bincode::deserialize(&serialized)?;
```

### Page header

Add compression flag to page structure:

```rust
// First byte(s) of each page
struct PageHeader {
    flags: u8,  // bit 0: compressed
    // Future: compression algorithm, original size for validation
}
```

### B-tree integration

Modify `would_fit()` to estimate compressed size:

```rust
fn would_fit(&self, entry_size: usize, page_size: usize) -> bool {
    let raw_size = self.serialized_size() + entry_size;
    // Estimate: assume 60% compression ratio for safety margin
    let estimated_compressed = raw_size * 60 / 100;
    estimated_compressed <= page_size * 95 / 100
}
```

This is imprecise but avoids compressing on every insert check.

### WAL compression (deferred)

WAL records are already variable-size. Compression is straightforward but lower priority:

```rust
// In WalWriter::append()
let payload = bincode::serialize(&record)?;
let compressed = zstd::encode_all(&payload, 3)?;
// Write with compression flag in RecordHeader
```

Defer until page compression is stable.

---

## What to compress

| Structure           | Compress? | Reason                                   |
|---------------------|-----------|------------------------------------------|
| Superblock (page 0) | No        | Fixed format, small, needs fast access   |
| MetadataPage        | No        | Fixed format, small                      |
| Registry            | Maybe     | Small, but could benefit                 |
| Data pages          | Yes       | Primary target - user data               |
| WAL records         | Deferred  | Lower priority, adds recovery complexity |

---

## Performance Considerations

1. **Hot path overhead**: `would_fit()` is called on every insert - use estimation, not actual compression
2. **Cache strategy**: Store uncompressed in `PageCache`, compress only at flush
3. **Async compatibility**: `zstd` is CPU-bound; consider `spawn_blocking` for large pages
4. **Metrics**: Track compression ratio per node type to tune thresholds

---

## Estimated Effort

| Task                                         | Effort   |
|----------------------------------------------|----------|
| Add `zstd` dependency, page header flags     | 1 day    |
| Modify `write_page_to_disk` / read path      | 2-3 days |
| Update `would_fit()` with size estimation    | 2-3 days |
| Handle edge cases (ratio > 95%, fallback)    | 2 days   |
| Metrics & benchmarking                       | 2 days   |
| WAL compression (optional, defer)            | 3-5 days |
| **Total (without WAL)**                      | ~2 weeks |
