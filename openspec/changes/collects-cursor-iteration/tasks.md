## 1. Types in `rumps-types`

- [x] 1.1 Define `Bounds` wrapping `(Bound<Key>, Bound<Key>)` with `fn new(Bound<Key>, Bound<Key>) -> Self`, `fn start(&self) -> &Bound<Key>`, `fn end(&self) -> &Bound<Key>`, `fn full() -> Self`, `fn contains(&self, k: &Key) -> bool`
- [x] 1.2 Implement `From<RangeFull>`, `From<Range<Key>>`, `From<RangeFrom<Key>>`, `From<RangeTo<Key>>`, `From<RangeInclusive<Key>>`, `From<RangeToInclusive<Key>>` for `Bounds`
- [x] 1.3 Define `Dir` enum: `Forward`, `Reverse`
- [x] 1.4 Define `Scan { bounds: Bounds, dir: Dir }` with constructors: `fn forward(bounds: impl Into<Bounds>) -> Self`, `fn reverse(bounds: impl Into<Bounds>) -> Self`, `fn new(bounds: impl Into<Bounds>, dir: Dir) -> Self`
- [x] 1.5 Implement `From<Option<&Key>>` for `Scan`: `None` -> `Scan { bounds: Bounds::full(), dir: Dir::Forward }`, `Some(k)` -> `Scan { bounds: (Excluded(k.clone()), Unbounded), dir: Dir::Forward }`
- [x] 1.6 Write tests for `Bounds`: all six `From` conversions (`.., a..b, a..=b, a.., ..b, ..=b`), explicit `Bounds::new(Excluded, Excluded)`, `contains` with included/excluded/unbounded bounds, empty range `a..a`. Tests for `Scan::from(None)` and `Scan::from(Some(&key))` equivalence with old `start` behavior.

## 2. `Subscript::successor` and `Key::prefix_upper_bound`

- [x] 2.1 Implement `pub fn successor(&self) -> Option<Subscript>` on `Subscript` in `crates/rumps-types/src/key.rs`. Successors stay within the same type; at each type's ceiling, return `None` (and `prefix_upper_bound` pops that subscript). The collation order is `Boolean(0) < Number(1) < Char(2) < String(3) < Json(4)`. Successor rules:
  - `Boolean(false)` -> `Some(Boolean(true))`
  - `Boolean(true)` -> `None` (ceiling of `Boolean`)
  - `Number(n)` where `n < f64::INFINITY` -> `Some(Number(OrderedFloat(next_representable_f64(n))))` (use `f64::next_up` or manual bit manipulation for next representable float; `NaN` is handled per `OrderedFloat`'s ordering)
  - `Number(f64::INFINITY)` -> `None` (ceiling of `Number`)
  - `Char(c)` where `c < char::MAX` -> `Some(Char(next_char(c)))` (next Unicode scalar value via `char::from_u32(c as u32 + 1)`, skipping the surrogate gap `0xD800..=0xDFFF`)
  - `Char(char::MAX)` -> `None` (ceiling of `Char`)
  - `String(s)` -> `Some(String(s + "\0"))` (append a null byte; this is the smallest string strictly greater than `s` in lexicographic byte order; `String` has no ceiling)
  - `Json(_)` -> `None` (value space has no computable maximum)
- [x] 2.2 Implement `pub fn prefix_upper_bound(&self) -> Option<Key>` on `Key` in `crates/rumps-types/src/key.rs`. Clone `self`'s subscripts into a `Vec`. Starting from the last subscript, call `successor()`. If `Some(succ)`, replace that subscript with `succ`, truncate the vec to that length (drop any subscripts after it), and return `Some(Key(vec))`. If `None`, pop that subscript and try the next one toward the front. If all subscripts are exhausted, return `None`. An empty key returns `None` (empty prefix matches all keys).
- [x] 2.3 Write tests for `Subscript::successor`: within-type successor (`Boolean(false)` -> `Boolean(true)`, `Number(1.0)` -> next float, `Char('a')` -> `Char('b')`, `String("abc")` -> `String("abc\0")`), type ceilings return `None` (`Boolean(true)`, `Number(INFINITY)`, `Char(char::MAX)`, `Json(...)`), `NaN` handling per `OrderedFloat`'s ordering
- [x] 2.4 Write tests for `Key::prefix_upper_bound`: simple case (`[1, "NAME"]` -> `[1, "NAME\0"]`), last subscript at ceiling (`[1, Boolean(true)]` -> pops `Boolean(true)`, tries `1` -> `[next_float(1)]`), all subscripts at ceiling (`[Boolean(true)]` -> `None`), empty key -> `None`, single subscript (`["foo"]` -> `["foo\0"]`)

## 3. `btree::Cursor` Core

- [x] 3.1 Define `Cursor<'a>` in `btree.rs`: fields `stack: SmallVec<[(NodeId, usize); 8]>`, `tree: &'a BTree`, `exhausted: bool`
- [x] 3.2 Implement `async fn seek(tree: &'a BTree, root: NodeId, target: Option<&Key>) -> Result<Cursor<'a>>`: descend from root, push `(node_id, idx)` at each level, land on first key >= target (or leftmost for `None`)
- [x] 3.3 Implement `async fn seek_back(tree: &'a BTree, root: NodeId, target: Option<&Key>) -> Result<Cursor<'a>>`: descend from root, land on last key <= target (or rightmost for `None`). Uses `find_predecessor_entry` logic to track fallback candidates.
- [x] 3.4 Implement `async fn advance(&mut self) -> Result<Option<(Key, Arc<NodeData>)>>`: yield current entry, then step forward (increment leaf index; on exhaustion pop stack, yield parent separator, descend into next child's leftmost)
- [x] 3.5 Implement `async fn advance_back(&mut self) -> Result<Option<(Key, Arc<NodeData>)>>`: yield current entry, then step backward (decrement leaf index; at `0` pop stack, yield parent separator to the left, descend into left child's rightmost)
- [x] 3.6 Write cursor unit tests: empty tree, single-node tree, multi-level tree (3+ levels), `seek` to exact key, `seek` to missing key (lands on successor), `seek_back` to exact key, `seek_back` to missing key (lands on predecessor), full forward traversal, full backward traversal, forward traversal == backward traversal reversed

## 4. `find_predecessor_entry`

- [x] 4.1 Implement `async fn find_predecessor_entry(&self, node_id: NodeId, target: &Key) -> Result<Option<(Key, Arc<NodeData>)>>` on `BTree`: mirror of `find_successor_entry`; descend toward target, track fallback to the left of search position at each internal node, at leaf return `keys[pos - 1]` or fallback
- [x] 4.2 Write tests: predecessor in same leaf, predecessor in ancestor, no predecessor (target <= smallest key), predecessor at internal node separator

## 5. Rename and Remove

- [x] 5.1 Rename `collects` -> `collect` in `BTree`: `collects_at` -> `collect_at`, `collects_internal` -> `collect_internal`, `collects_prefix_at` -> `collect_prefix_at`, `collects_prefix_internal` -> `collect_prefix_internal`
- [x] 5.2 Remove from `BTree`: `collects_vec_at`, `collects_prefix_vec_at`. Rename `collects_find_next` -> `collect_find_next`, `collects_prefix_first` -> `collect_prefix_first`, `collects_prefix_next` -> `collect_prefix_next` (still called by `collect_internal`/`collect_prefix_internal`; deleted in task 6)
- [x] 5.3 Rename `collects` -> `collect` in `Database`: `collects` -> `collect`, `collects_prefix` -> `collect_prefix`
- [x] 5.4 Remove from `Database`: `collects_vec`, `collects_prefix_vec`
- [x] 5.5 Rename `collects` -> `collect` in `Transaction`: `collects` -> `collect`, `collects_prefix_vec` -> (removed)
- [x] 5.6 Remove from `Transaction`: `collects_vec`, `collects_prefix_vec`
- [x] 5.7 Update all call sites that used `collects_vec` / `collects_prefix_vec` to use `collect(...).await?.try_collect().await` (or `collect_prefix(...).await?.try_collect().await`)
- [x] 5.8 Rename references in test files: `btree/tests.rs`, `database_bench.rs`, any other test modules
- [x] 5.9 Update doc comments on `Database::collect`, `Transaction::collect`, `BTree::collect_at`, `Database::collect_prefix` to reflect new name and `Scan` parameter
- [x] 5.10 Update `CLAUDE.md` examples if they reference `collects`
- [x] 5.11 Update `docs/storage.org`, `docs/operations.org`, and any other docs that reference `collects`
- [x] 5.12 Verify: `cargo build` compiles clean, `cargo test` passes, `cargo doc` has no broken links

## 6. Replace `collect_internal` with Cursor

- [ ] 6.1 Rewrite `collect_internal` to accept a `Scan`, create a `Cursor` via `seek` or `seek_back` (based on `scan.dir`), and use `stream::unfold` calling `cursor.advance()` or `cursor.advance_back()` with pred/extract filtering and bound checking from `scan.bounds`. `collect_internal` is responsible for bound adjustment: translating `Included`/`Excluded`/`Unbounded` start (forward) or end (reverse) into the raw `Option<&Key>` target for `seek`/`seek_back`, and skipping the initial entry when the seek bound is `Excluded` and the cursor lands on the exact bound key
- [ ] 6.2 Rewrite `collect_prefix_internal` to accept a `Dir` parameter and use `Cursor` with prefix-based early termination. For `Dir::Forward`: `seek(root, Some(&prefix))`, advance forward, stop when `!key.starts_with(prefix)`. For `Dir::Reverse`: compute `prefix.prefix_upper_bound()`; if `Some(ub)`, call `seek_back(root, Some(&ub))`; if `None`, call `seek_back(root, None)`; advance backward, stop when `!key.starts_with(prefix)`. If `seek_back` lands on a key not matching the prefix, the `starts_with` check skips it on the first iteration
- [ ] 6.3 Update `BTree::collect_at` signature: `start: Option<&Key>` -> `scan: &Scan`
- [ ] 6.4 Update `BTree::collect_prefix_at` signature to accept `Dir`
- [ ] 6.5 Run existing `collect` and `collect_prefix` tests to verify identical behavior

## 7. Unified `collect` Signature at Database and Transaction

- [ ] 7.1 Change `Database::collect` signature: `start: Option<&'a Key>` -> `scan: impl Into<Scan>`. Internal: convert to `Scan`, pass to `BTree::collect_at`
- [ ] 7.2 Change `Transaction::collect` signature: `start: Option<&'a Key>` -> `scan: impl Into<Scan>`. Pass `Dir` to `MergeState`.
- [ ] 7.3 Update all call sites passing `None` / `Some(&key)` to compile via `From<Option<&Key>>` (should be zero-change)
- [ ] 7.4 Add `Dir` parameter to `Database::collect_prefix` and `Transaction::collect_prefix`. Update all call sites to pass `Dir::Forward` (preserving current behavior)
- [ ] 7.5 Write scan integration tests covering: full forward (`Scan::forward(..)`), full reverse (`Scan::reverse(..)`), half-open forward (`Scan::forward(a..b)`), half-open reverse (`Scan::reverse(a..b)`), closed forward (`Scan::forward(a..=b)`), closed reverse (`Scan::reverse(a..=b)`), from-range forward (`Scan::forward(a..)`), to-range forward (`Scan::forward(..b)`), to-inclusive reverse (`Scan::reverse(..=b)`), explicit excluded-start (`Scan::forward(Bounds::new(Excluded(a), Excluded(b)))`), empty range (`a..a` yields nothing), backward-compat (`None` and `Some(&key)` via `Into<Scan>`), bound adjustment: `Excluded` start skips exact match on seek, `Included` start yields exact match
- [ ] 7.6 Write reverse prefix integration tests: forward prefix (identical to current behavior), reverse prefix yields entries in descending order, reverse prefix with no matching keys yields empty, reverse prefix where `prefix_upper_bound` exists as an actual key in the tree (should be skipped), reverse prefix where last subscript is at its type ceiling (e.g. `Boolean(true)`, `Json(...)`) and `prefix_upper_bound` pops it

## 8. `MergeState` Direction Support

- [ ] 8.1 Add `dir: Dir` field to `MergeState`
- [ ] 8.2 Branch `advance_buffered` on `dir`: `Dir::Forward` uses `range((Excluded(...), Unbounded)).next()` (current behavior), `Dir::Reverse` uses `range((Unbounded, Excluded(...))).next_back()`
- [ ] 8.3 Branch merge comparison in `next_impl` on `dir`: `Dir::Forward` yields smaller key first, `Dir::Reverse` yields larger key first
- [ ] 8.4 Add bounded range to `advance_buffered`: skip buffered entries outside `scan.bounds`
- [ ] 8.5 Write merge tests: forward with range bounds (half-open, closed), reverse unbounded, reverse with range bounds, reverse with buffered sets/deletes/kills, forward excluded-start range with buffered writes straddling bounds

## 9. Integration and Benchmarks

- [ ] 9.1 Run full test suite (`cargo test`) to verify no regressions
- [ ] 9.2 Run existing benchmarks (renamed); compare cursor-based iteration vs old `get_next_internal`
- [ ] 9.3 Add benchmarks for range-bounded iteration (compare with predicate-only termination)
- [ ] 9.4 Add benchmarks for reverse iteration (including reverse prefix iteration)
- [ ] 9.5 Update `TODOS/persistence.md` to reflect completed `collects` enhancements
