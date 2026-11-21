## ADDED Requirements

### Requirement: Bounds type with start and end bounds

`Bounds` SHALL specify iteration bounds using `Bound<Key>` for both start and end. The bounds SHALL support `Included`, `Excluded`, and `Unbounded` variants (from `std::ops::Bound`). `Bounds` SHALL implement `From` conversions from `RangeFull`, `Range<Key>`, `RangeFrom<Key>`, `RangeTo<Key>`, `RangeInclusive<Key>`, and `RangeToInclusive<Key>`. A `Bounds::new(Bound<Key>, Bound<Key>)` constructor SHALL allow arbitrary bound combinations not expressible via range syntax.

#### Scenario: Unbounded range via RangeFull
- **WHEN** `Bounds::from(..)` is constructed
- **THEN** the start bound is `Unbounded` and the end bound is `Unbounded`

#### Scenario: Half-open range via Range
- **WHEN** `Bounds::from(key_a..key_b)` is constructed
- **THEN** the start bound is `Included(key_a)` and the end bound is `Excluded(key_b)`

#### Scenario: Closed range via RangeInclusive
- **WHEN** `Bounds::from(key_a..=key_b)` is constructed
- **THEN** the start bound is `Included(key_a)` and the end bound is `Included(key_b)`

#### Scenario: From-range via RangeFrom
- **WHEN** `Bounds::from(key_a..)` is constructed
- **THEN** the start bound is `Included(key_a)` and the end bound is `Unbounded`

#### Scenario: To-range via RangeTo
- **WHEN** `Bounds::from(..key_b)` is constructed
- **THEN** the start bound is `Unbounded` and the end bound is `Excluded(key_b)`

#### Scenario: To-inclusive range via RangeToInclusive
- **WHEN** `Bounds::from(..=key_b)` is constructed
- **THEN** the start bound is `Unbounded` and the end bound is `Included(key_b)`

#### Scenario: Explicit excluded-start bound
- **WHEN** `Bounds::new(Excluded(key_a), Excluded(key_b))` is constructed
- **THEN** the start bound is `Excluded(key_a)` and the end bound is `Excluded(key_b)`

### Requirement: Dir enum and Scan type compose range with direction

`Dir` SHALL be an enum with variants `Forward` and `Reverse`. `Scan` SHALL bundle a `Bounds` and a `Dir`. `Scan::forward(range)` and `Scan::reverse(range)` are symmetric convenience constructors; `Scan::new(range, dir)` covers all combinations. `From<Option<&Key>>` for `Scan` SHALL produce a forward scan compatible with the existing `collect` behavior: `None` maps to a full unbounded forward scan, `Some(k)` maps to `Excluded(k)..` forward.

#### Scenario: Scan from None (full forward)
- **WHEN** `Scan::from(None::<&Key>)` is constructed
- **THEN** `scan.bounds` is `(Unbounded, Unbounded)` and `scan.dir` is `Dir::Forward`

#### Scenario: Scan from Some(key) (after-key forward)
- **WHEN** `Scan::from(Some(&key))` is constructed
- **THEN** `scan.bounds` is `(Excluded(key), Unbounded)` and `scan.dir` is `Dir::Forward`

### Requirement: Bound adjustment at the collect_internal layer

The cursor's `seek` and `seek_back` are bound-unaware; they always use >= (forward) and <= (backward) semantics. `collect_internal` is responsible for translating `Bounds` bounds into the correct seek target and skipping the initial entry when the bound is `Excluded`. Specifically:

**Forward iteration** (seek position determined by start bound):
- `Unbounded`: `seek(tree, root, None)` (from the beginning)
- `Included(k)`: `seek(tree, root, Some(&k))`; the first entry (key == `k`) is yielded
- `Excluded(k)`: `seek(tree, root, Some(&k))`; if the first entry's key == `k`, skip it (advance once without yielding)

**Reverse iteration** (seek position determined by end bound):
- `Unbounded`: `seek_back(tree, root, None)` (from the end)
- `Included(k)`: `seek_back(tree, root, Some(&k))`; the first entry (key == `k`) is yielded
- `Excluded(k)`: `seek_back(tree, root, Some(&k))`; if the first entry's key == `k`, skip it (advance_back once without yielding)

#### Scenario: Forward, Included start
- **WHEN** forward iteration with range `key_a..key_b` seeks to `key_a`
- **THEN** `key_a` IS yielded (start bound is `Included`)

#### Scenario: Forward, Excluded start
- **WHEN** forward iteration with `Scan::from(Some(&key_a))` seeks to `key_a`
- **THEN** `key_a` is NOT yielded; the cursor advances past it because the start bound is `Excluded`

#### Scenario: Reverse, Included end
- **WHEN** reverse iteration with range `key_a..=key_b` seeks back to `key_b`
- **THEN** `key_b` IS yielded (end bound is `Included`)

#### Scenario: Reverse, Excluded end
- **WHEN** reverse iteration with `Bounds::new(Included(key_a), Excluded(key_b))` and `Dir::Reverse` seeks back to `key_b`
- **THEN** `key_b` is NOT yielded; the cursor advances back past it because the end bound is `Excluded`

### Requirement: Stream early-terminates on the opposing bound

After each advance, the stream checks the opposing bound (the one not used for seeking). For forward iteration, the *end* bound terminates the stream; for reverse iteration, the *start* bound terminates.

#### Scenario: Forward, Excluded end bound
- **WHEN** forward iteration with range `key_a..key_b` advances to `key_b`
- **THEN** `key_b` is NOT yielded and the stream terminates

#### Scenario: Forward, Included end bound
- **WHEN** forward iteration with range `key_a..=key_b` advances to `key_b`
- **THEN** `key_b` IS yielded, then the stream terminates on the next advance

#### Scenario: Reverse, Excluded start bound
- **WHEN** reverse iteration with `Bounds::new(Excluded(key_a), Included(key_b))` advances backward to `key_a`
- **THEN** `key_a` is NOT yielded and the stream terminates

#### Scenario: Reverse, Included start bound
- **WHEN** reverse iteration with range `key_a..=key_b` advances backward to `key_a`
- **THEN** `key_a` IS yielded, then the stream terminates on the next backward advance

#### Scenario: Empty range
- **WHEN** iteration uses range `key_a..key_a` (start == end, excluded end)
- **THEN** the stream yields zero entries

#### Scenario: No matching keys in range
- **WHEN** iteration uses range `key_a..key_b` and the tree has no keys in `[key_a, key_b)`
- **THEN** the stream yields zero entries

### Requirement: Unified collect signature at Database and Transaction layers

`Database::collect` and `Transaction::collect` SHALL accept `impl Into<Scan>` in place of the current `start: Option<&Key>`. Existing call sites passing `None` or `Some(&key)` SHALL continue to compile unchanged via `From<Option<&Key>>` (after the `collects` -> `collect` rename). The `collects_vec` and `collects_prefix_vec` methods SHALL be removed; callers use `.try_collect().await` on the returned stream.

#### Scenario: Existing forward call site (post-rename)
- **WHEN** `db.collect(name, None, pred, extract)` is called
- **THEN** it compiles and behaves identically to the pre-change `collects`

#### Scenario: Existing after-key call site (post-rename)
- **WHEN** `db.collect(name, Some(&key), pred, extract)` is called
- **THEN** it compiles and yields entries after `key` in forward order, same as before

#### Scenario: Full forward scan
- **WHEN** `db.collect(name, Scan::forward(..), pred, extract)` is called
- **THEN** all entries are yielded smallest to largest

#### Scenario: Full reverse scan
- **WHEN** `db.collect(name, Scan::reverse(..), pred, extract)` is called
- **THEN** all entries are yielded largest to smallest

#### Scenario: Half-open forward range
- **WHEN** `db.collect(name, Scan::forward(key_a..key_b), pred, extract)` is called
- **THEN** only keys in `[key_a, key_b)` satisfying `pred` are yielded, smallest to largest

#### Scenario: Half-open reverse range
- **WHEN** `db.collect(name, Scan::reverse(key_a..key_b), pred, extract)` is called
- **THEN** only keys in `[key_a, key_b)` satisfying `pred` are yielded, largest to smallest

#### Scenario: Closed forward range
- **WHEN** `db.collect(name, Scan::forward(key_a..=key_b), pred, extract)` is called
- **THEN** only keys in `[key_a, key_b]` satisfying `pred` are yielded, smallest to largest

#### Scenario: Closed reverse range
- **WHEN** `db.collect(name, Scan::reverse(key_a..=key_b), pred, extract)` is called
- **THEN** only keys in `[key_a, key_b]` satisfying `pred` are yielded, largest to smallest

#### Scenario: From-range forward
- **WHEN** `db.collect(name, Scan::forward(key_a..), pred, extract)` is called
- **THEN** only keys >= `key_a` satisfying `pred` are yielded, smallest to largest

#### Scenario: To-range forward
- **WHEN** `db.collect(name, Scan::forward(..key_b), pred, extract)` is called
- **THEN** only keys < `key_b` satisfying `pred` are yielded, smallest to largest

#### Scenario: To-inclusive reverse
- **WHEN** `db.collect(name, Scan::reverse(..=key_b), pred, extract)` is called
- **THEN** only keys <= `key_b` satisfying `pred` are yielded, largest to smallest

#### Scenario: Excluded-start forward (explicit bounds)
- **WHEN** `db.collect(name, Scan::forward(Bounds::new(Excluded(key_a), Excluded(key_b))), pred, extract)` is called
- **THEN** only keys in `(key_a, key_b)` satisfying `pred` are yielded, smallest to largest

#### Scenario: Transaction merges buffered writes within range
- **WHEN** `Transaction::collect` is called with a bounded `Scan` and the transaction has buffered writes within and outside that range
- **THEN** only buffered writes with keys inside the range are merged with the snapshot

#### Scenario: Transaction reverse merge
- **WHEN** `Transaction::collect` is called with `Dir::Reverse` and the transaction has buffered sets at keys `[2, 5]` and the snapshot has keys `[1, 3, 4]`
- **THEN** the merged stream yields `[5, 4, 3, 2, 1]`
