## Why

The `collects` iteration system performs a full root-to-leaf B-tree traversal (`find_successor_entry`, `O(log n)`) for every entry yielded, making a scan of `m` entries cost `O(m * log n)`. A cursor-based approach reduces this to `O(log n)` initial seek plus `O(m)` amortized iteration. Additionally, `collects` currently lacks range bounds (relying on predicates for termination, which cannot early-terminate on the general path) and reverse iteration, both of which are needed for efficient range queries and reverse `$ORDER`/`$QUERY`.

## What Changes

- Rename `collects` -> `collect` throughout (the method name, internal helpers, docs, tests, benchmarks); remove `collects_vec` and `collects_prefix_vec` (callers use `.try_collect().await` on the returned stream)
- Replace per-entry `get_next_internal` calls in `collect_internal` and `collect_prefix_internal` with a stack-based `btree::Cursor` that seeks once and walks leaf-to-leaf
- Add `Bounds` type with `Bound<Key>` start/end bounds, `Dir` enum (`Forward`/`Reverse`), and `Scan` struct composing both
- Unify the `collect` signature: existing `start: Option<&Key>` becomes `impl Into<Scan>` via `From<Option<&Key>>`, so current call sites compile unchanged while new callers can pass ranges and directions
- Add `find_predecessor_entry` (largest key strictly less than target) and reverse cursor traversal (`seek_back`/`advance_back`)
- Add `Subscript::successor` and `Key::prefix_upper_bound` for collation-aware prefix upper bound computation
- Add `Dir` parameter to `collect_prefix` at all layers (`BTree`, `Database`, `Transaction`) for reverse prefix iteration, using `prefix_upper_bound` to compute the `seek_back` target
- Extend `Transaction::MergeState` to support reverse-mode iteration via `BTreeMap::range().next_back()`
- Stretch: batch node prefetching when the cursor exhausts a leaf (read-ahead the next sibling)

## Capabilities

### New Capabilities
- `btree-cursor`: Stack-based `btree::Cursor` with `seek`/`seek_back`/`advance`/`advance_back` for `O(1)` amortized bidirectional iteration
- `range-bounded-queries`: `Bounds`, `Dir`, and `Scan` types; unified `collect` signature via `impl Into<Scan>` for range-bounded and directional queries
- `reverse-iteration`: `find_predecessor_entry` for reverse lookup; `Subscript::successor` and `Key::prefix_upper_bound` for reverse prefix seeking; `MergeState` direction support; `collect_prefix` gains `Dir`

### Modified Capabilities

## Impact

- `crates/rumps-storage/src/btree.rs`: `Cursor` struct; `collect_internal` and `collect_prefix_internal` rewritten to use cursor; `find_predecessor_entry`; rename `collects_*` -> `collect_*`; remove `collects_vec_at`, `collects_prefix_vec_at`
- `crates/rumps-storage/src/database.rs`: rename `collects` -> `collect`; remove `collects_vec`, `collects_prefix_vec`; `collect` signature changes `start: Option<&Key>` to `scan: impl Into<Scan>`; `collect_prefix` gains `Dir`; update doc comments and examples
- `crates/rumps-storage/src/transaction.rs`: rename `collects` -> `collect`; remove `collects_vec`, `collects_prefix_vec`; `MergeState` gains `Dir`; `advance_buffered` branches on direction; `collect_prefix` gains `Dir`; update doc comments
- `crates/rumps-types/src/key.rs`: `Subscript::successor`, `Key::prefix_upper_bound`
- `crates/rumps-types/src/lib.rs` (or new module): `Bounds`, `Dir`, `Scan` type definitions
- `docs/`: update any references to `collects` -> `collect`
- Benchmarks: rename, update; existing benchmarks should show improvement; new benchmarks for range and reverse queries
