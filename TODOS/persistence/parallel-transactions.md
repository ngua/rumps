# Parallel Transactions

## Current State

- Sequential `Transaction::set` throughput: ~10 Kelem/s
- 1M records sequentially: ~100 seconds
- Group commits already implemented (batch fsync)

## Motivation

Parallel transactions are orthogonal and synergistic with group commits:

- **Group commits**: Reduce fsync overhead by batching multiple commits into one disk flush
- **Parallel transactions**: Increase concurrency; multiple transactions in-flight simultaneously

Together they compound: parallel transactions create more transactions in-flight, giving group commit more to batch. You get both concurrency AND amortized fsync cost.

Expected improvement: 10k/sec sequential -> 50-100k+/sec with parallelism (assuming efficient locking).

## Implementation Considerations

- Need efficient locking strategy (row-level vs page-level)
- Conflict detection for concurrent writes to same keys
- Consider optimistic concurrency control for read-heavy workloads
