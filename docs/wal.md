# RUMPS Write-Ahead Log (WAL)

This document describes the Write-Ahead Log implementation in RUMPS, which provides crash recovery and durability guarantees for transactional writes.

## Overview

The WAL ensures durability by logging all modifications before they are applied to the main data file. On crash recovery, uncommitted transactions can be rolled back and committed transactions can be replayed.

## Architecture

### Reader → Writer Lifecycle

The WAL enforces a single initialization path:

```
WalReader::open(dir)  →  iterate for recovery  →  reader.into_writer(cfg)
```

This design ensures:

1. **Single source of truth**: The reader is the authority on file state. There's no separate "open for writing" path that might disagree about sequence numbers or file position.

2. **No double-scanning**: The reader tracks position and sequence numbers as it iterates. Converting to a writer reuses this state without re-scanning.

3. **Forced acknowledgment**: Callers must explicitly handle existing WAL records (even if just iterating to EOF) before writing new ones. This prevents accidentally ignoring recovery.

4. **Clear lifecycle**: Read phase (recovery) → Write phase (runtime). No ambiguity about which operations are valid when.

`WalWriter` has no public constructor; it can only be created via `WalReader::into_writer`.

### Channel-Based Background Task

`WalWriter` uses a channel-based background task design:

- All operations (append, sync, rotate, checkpoint) are sent as commands via `mpsc` channel
- A background task owns the file exclusively and processes commands sequentially
- Callers receive results via `oneshot` channels
- `WalWriter` is trivially `Arc`-shareable with no mutex contention

This design enables **group commit** and **batched writes** (see Performance section).

## File Format

### File Structure

```
┌─────────────────────────────────────────────────────────────┐
│ File Header (16 bytes)                                      │
│   magic: [u8; 4]     = b"RWAL"                              │
│   version: u16       = 1                                    │
│   flags: u16         = 0 (reserved)                         │
│   first_seq: u64     = sequence number of first record      │
├─────────────────────────────────────────────────────────────┤
│ Record 0                                                    │
│   header: RecordHeader (20 bytes)                           │
│   payload: [u8; header.len]                                 │
├─────────────────────────────────────────────────────────────┤
│ Record 1                                                    │
│   header: RecordHeader (20 bytes)                           │
│   payload: [u8; header.len]                                 │
├─────────────────────────────────────────────────────────────┤
│ ...                                                         │
└─────────────────────────────────────────────────────────────┘
```

### Record Header

Each record is prefixed with a fixed-size header:

```
┌────────────────────────────────────────┐
│ RecordHeader (20 bytes)                │
│   checksum: u32   - CRC32 of payload   │
│   len: u32        - payload length     │
│   seq: u64        - sequence number    │
│   flags: u32      - reserved           │
└────────────────────────────────────────┘
```

The payload is the bincode-serialized `WalRecord`.

### Sequence Numbers

`WalSequence` is a monotonically increasing `u64` that uniquely identifies each record across all WAL files. Sequence numbers are used to:

- Order records for replay during recovery
- Detect gaps in the WAL (indicating corruption or missing files)
- Determine which archived files can be safely deleted after checkpointing

## Record Types

```rust
enum WalRecord {
    TxnBegin { txn_id },
    TxnCommit { txn_id },
    TxnAbort { txn_id },
    Set { txn_id, name, key, old, new },
    KillEntry { txn_id, name, key, data },
    Checkpoint { seq },
}
```

### Transaction Lifecycle

- `TxnBegin`: Marks transaction start
- `TxnCommit`: All operations from this transaction should be made durable
- `TxnAbort`: All operations from this transaction should be discarded

### Data Operations

- `Set`: Records a SET operation with both old (for undo) and new values
- `KillEntry`: Records a single key deletion within a KILL operation

### Incremental Kill Records

When a KILL operation removes a subtree (e.g., `KILL ^PATIENT(123)` which has children `NAME`, `DOB`, `ADDR`), we emit one `KillEntry` record per deleted key rather than storing the entire subtree in a single record.

This keeps each WAL record bounded in size, at the cost of a longer WAL for large subtree deletions.

### Checkpoints

`Checkpoint { seq }` indicates that all data up to sequence `seq` has been flushed to the main data file. WAL entries before this checkpoint can be discarded during recovery.

## File Rotation

When the current WAL file exceeds `max_file_size` (default: 64 MiB), it is rotated:

1. Current file is synced
2. File is renamed to `wal.{first_seq:016x}-{last_seq:016x}.log`
3. A new `wal.log` is created with the next sequence number

## Recovery Algorithm

1. Read all records from WAL sequentially (across all files in sequence order)
2. Track transaction states: `TxnBegin` → pending, `TxnCommit` → committed, `TxnAbort` → aborted
3. Collect `Set` and `KillEntry` operations grouped by transaction
4. If a `Checkpoint` record is found, filter out operations with `seq <= checkpoint_seq`
5. Return only operations from committed transactions (in sequence order)
6. Report uncommitted transactions (began but never committed/aborted)

### Partial Write Handling

Incomplete records at EOF (e.g., from a crash mid-write) are treated as if they never happened. The reader returns `Ok(None)` for these, allowing recovery to proceed with valid data.

## Sync Modes

```rust
enum SyncMode {
    Immediate,       // Sync on every write (slowest, safest)
    OnCommit,        // Sync only when sync() called (default)
    Periodic(Duration), // Sync at intervals
}
```

---

## Performance Analysis

Benchmark results from `cargo bench -p rumps-storage --features bench --bench wal_bench`.

### Append Throughput (no fsync)

| Record Size   | Time     | Throughput    |
|---------------|----------|---------------|
| Small (~12B)  | ~6.1 µs  | ~163K ops/sec |
| Medium (100B) | ~6.9 µs  | ~146K ops/sec |
| Large (1KB)   | ~7.7 µs  | ~129K ops/sec |

The ~6-8 µs overhead is dominated by:
- Channel send/receive overhead (mpsc + oneshot)
- Tokio task switching
- File write syscall (buffered, no sync)

### Serialization Overhead

| Size  | Time     |
|-------|----------|
| Small | ~17 ns   |
| 100B  | ~237 ns  |
| 1KB   | ~373 ns  |

Serialization is <5% of append time - bincode is not a bottleneck.

### Commit with Sync

A single-operation transaction (begin + set + commit + fsync) takes ~33 µs, yielding ~30K durable writes/sec for a single caller.

### Group Commit Effectiveness

The key benefit of the channel-based design is concurrent sync batching. Each task does append + sync; the background task batches concurrent `sync()` calls into a single `fsync()`:

| Concurrent Tasks | Total Time | Sequential Would Be | Speedup |
|------------------|------------|---------------------|---------|
|                1 |    161 µs  |             161 µs  |      1x |
|                4 |    204 µs  |             644 µs  |   ~3.2x |
|                8 |    210 µs  |           1,288 µs  |   ~6.1x |
|               16 |    264 µs  |           2,576 µs  |   ~9.8x |
|               32 |    323 µs  |           5,152 µs  |    ~16x |

**Why this matters**: Without group commit, each transaction's `sync()` blocks on `fsync()`. With N concurrent transactions, you'd do N separate fsyncs. With group commit, concurrent syncs are batched - the background task collects all pending `Sync` commands via `try_recv()` and issues one `fsync()` for the batch.

The speedup scales with concurrency because the fsync cost is amortized across all waiting tasks. At 32 concurrent tasks, we see ~16x speedup over sequential execution.

On slower storage the benefits are even more pronounced:
- **HDD (5-15ms fsync)**: 32 sequential syncs = 160-480ms; batched = 5-15ms → **10-30x speedup**
- **SSD without write cache (0.5-2ms fsync)**: 32 sequential = 16-64ms; batched = 0.5-2ms → **32x speedup**

### Batched Writes

In addition to group commit, the background task batches pending append commands via `try_recv()` and combines them into a single `write_all()` syscall, reducing per-record overhead.

### Comparison to Mutex-Based Approach

The current channel-based design was compared against an earlier `WalWriter` implementation that used a `Mutex` to serialize access. The Mutex values below were measured at the time of that implementation and may not reflect identical test conditions; they are presented for rough comparison only.

| Benchmark                    | Channel   | Mutex (old impl) | Improvement |
|------------------------------|-----------|------------------|-------------|
| Append (small, no sync)      | 6.1 µs    | ~10.7 µs         | ~1.7x       |
| Append (medium, no sync)     | 6.9 µs    | ~11.7 µs         | ~1.7x       |
| Append (large, no sync)      | 7.7 µs    | ~11.9 µs         | ~1.5x       |
| Commit with sync             | 33 µs     | ~41 µs           | ~1.2x       |

The channel approach wins because there's no lock contention on the hot path - appends go through an mpsc channel, and batching happens naturally in the background task.

*Note: Benchmarks run on ZFS with SSD. Real spinning disks take 5-15ms per fsync; SSDs typically 0.1-2ms.*

### Future Improvements

1. **Async Fsync Option** - For workloads that tolerate losing the last few milliseconds of writes in a crash, offer a "periodic sync" mode that fsyncs on a timer (e.g., every 100ms) rather than per-commit.

2. **Validate Sync Numbers on Real Storage** - Current benchmarks may be flattering due to aggressive disk caching. Production benchmarks should use:
   - Real SSDs with write cache disabled (`hdparm -W 0`)
   - Spinning disks for worst-case latency
   - Different filesystem configurations (ext4, XFS, ZFS)

3. **Write Coalescing** - For multiple writes to the same key within a transaction, only the final value needs to be logged. This reduces WAL size for update-heavy workloads.

4. **Compression** - Optional compression of WAL records for write-heavy workloads with compressible data. Trade CPU for I/O bandwidth.
