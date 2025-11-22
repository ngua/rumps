# Hierarchical Semantics: `has_descendants` Flag Implementation

## Problem Statement

The previous `SET` implementation in `crates/rumps-storage/src/btree.rs` did not maintain the `has_descendants` flag on ancestor nodes. This flag is **critical** for MUMPS hierarchical semantics and must be implemented before moving to other primitives.

### Why This Is Critical

The `has_descendants` flag enables three essential MUMPS operations:

1. **`$DATA(key)`** - Returns different values based on node state:
   - `0`: No data (doesn't exist)
   - `1`: Has value only (leaf node)
   - `10`: Has descendants only (intermediate node)
   - `11`: Has both value and descendants

2. **`$ORDER(key)`** - Navigates the hierarchy correctly:
   - Needs to know whether to descend into children or skip to next sibling
   - Without `has_descendants`, cannot distinguish leaves from branches

3. **`KILL(key)`** - Removes subtrees and updates parents:
   - When killing a node, must update parent's `has_descendants` if no siblings remain
   - Incorrect flags would leave orphaned metadata

---

## Design Decision: Arc<NodeData> for Efficient Hierarchy Navigation

**Critical Optimization**: `Node` stores `Vec<Arc<NodeData>>` instead of `Vec<NodeData>` directly.

**Rationale**:
- Hierarchy navigation (checking `has_descendants` flags) is extremely frequent in MUMPS operations
- `$DATA`, `$ORDER`, `ensure_ancestors()` all repeatedly access NodeData without needing ownership
- `Arc::clone()` (incrementing refcount) is much cheaper than cloning the entire `NodeData`
- When extracting values in the public `get()` API, we simply clone the `Option<Value>`

**Implementation Impact**:
1. **Node Type Change**: `pub values: Vec<Arc<NodeData>>` in `rumps-types/src/node.rs`
2. **Return Type**: `get_internal()` returns `Option<Arc<NodeData>>` instead of `Option<NodeData>`
3. **Node Creation**: All values wrapped with `Arc::new(NodeData { ... })`
4. **Serialization**: Custom serialize/deserialize unwraps/wraps Arc (already have custom impl)
5. **Public API**: `get()` extracts value by cloning:
   ```rust
   pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
       Ok(self.get_internal(name, key).await?.and_then(|arc_data| arc_data.value.clone()))
   }
   ```

**Performance Benefits**:
- Hierarchy checks: O(1) Arc clone instead of O(value_size) data clone
- Typical case: Most operations just check `has_descendants` flag, never clone the actual data
- Value extraction: Simple clone of the `Option<Value>` for GET operations

---

## Storage Model Recap

RUMPS uses a **flat B-tree** (not a true hierarchy) where keys are complete paths:

```text
Physical B-tree storage:
┌─────────────────────────────────────┐
│ Keys (sorted):                      │
│   Key([123])                        │  ← Ancestor (intermediate node)
│   Key([123, "ADDR"])                │  ← Descendant 1
│   Key([123, "DOB"])                 │  ← Descendant 2
│   Key([123, "NAME"])                │  ← Descendant 3
│   Key([124])                        │  ← Different subtree
│   Key([124, "NAME"])                │  ← Descendant
└─────────────────────────────────────┘

Logical hierarchy (user view):
^PATIENT
  ├─ 123
  │   ├─ "ADDR"
  │   ├─ "DOB"
  │   └─ "NAME"
  └─ 124
      └─ "NAME"
```

The hierarchy is **implicit** in the key structure. We must maintain `has_descendants` flags to enable hierarchical queries.

---

## Algorithm Overview

### Ancestor Key Generation

For a key `Key([a, b, c, d])`, the ancestors are:
- `Key([a])`
- `Key([a, b])`
- `Key([a, b, c])`

### SET Operation with Hierarchy Maintenance

When setting `Key([a, b, c])` with value `v`:

1. **Generate ancestors**: `[Key([a]), Key([a, b])]`
2. **For each ancestor** (in order from root to leaf):
   - Search for the key in the B-tree
   - If **not found**: Insert `NodeData::with_descendants()` (value = None, has_descendants = true)
   - If **found**: Update `has_descendants` to `true` if needed
3. **Insert target key**: Insert `Key([a, b, c])` with `NodeData::with_value(v)`
4. **Handle splits** as needed during insertion

### Performance

- **Worst Case**: O(d * log n) where d = key depth
- **Typical Case**: Most MUMPS data is 2-3 levels deep (1-2 ancestors)

---

## Implementation Steps

### Step 1: Update Node Type with Arc<NodeData> Wrapper ✅ COMPLETE

**File**: `crates/rumps-types/src/node.rs`

**Summary**: Updated the `Node` struct to use `Vec<Arc<NodeData>>` instead of `Vec<NodeData>` for efficient cloning during hierarchy navigation.

**Implementation Complete**:
- ✅ Changed `values: Vec<NodeData>` to `values: Vec<Arc<NodeData>>`
- ✅ Updated custom `Serialize` impl to unwrap Arc using `Arc::try_unwrap()` or clone
- ✅ Updated custom `Deserialize` impl to wrap deserialized NodeData in `Arc::new()`
- ✅ Updated all Node construction in `btree.rs` to wrap values in `Arc::new()`
- ✅ Updated value access patterns to use `Arc::clone()` where needed
- ✅ All 132 tests pass (103 in rumps-types + 29 in rumps-storage)
- ✅ Verified serialization round-trip works correctly
- ✅ Verified existing B-tree operations work with Arc-wrapped values

---

### Step 2: Add `Key::ancestors()` Method ✅ COMPLETE

**File**: `crates/rumps-types/src/key.rs`

**Summary**: Implemented `Key::ancestors()` method that returns all ancestor keys (prefixes) for a given key path.

**Implementation Complete**:
- ✅ Added `ancestors()` method to Key impl
- ✅ Returns all prefixes except the full key
- ✅ Empty keys and single-subscript keys return empty ancestors
- ✅ Method uses functional iterator style with `map` and `collect`
- ✅ Comprehensive documentation with examples
- ✅ All unit tests pass:
  - ✅ test_ancestors_empty_key
  - ✅ test_ancestors_single_subscript
  - ✅ test_ancestors_two_subscripts
  - ✅ test_ancestors_deep_nesting (5 levels)
- ✅ All 107 tests in rumps-types pass

---

### Step 3: Implement `get_internal()` - B-Tree Navigation ✅ COMPLETE

**File**: `crates/rumps-storage/src/btree.rs` (private helpers impl block)

**Summary**: Implemented B-tree navigation to retrieve full `Arc<NodeData>` for efficient hierarchy operations.

**Implementation Complete**:
- ✅ Added `get_internal()` method returning `Option<Arc<NodeData>>`
- ✅ Added `search_from_node()` recursive helper for tree traversal
- ✅ Uses binary search to find keys in nodes
- ✅ Correctly handles partial matches (returns None when key doesn't exist)
- ✅ Works for both leaf and internal nodes
- ✅ Returns `Arc<NodeData>` for cheap cloning during hierarchy checks
- ✅ Comprehensive test coverage (10 tests):
  - ✅ test_get_internal_nonexistent_variable
  - ✅ test_get_internal_exact_match_single_key
  - ✅ test_get_internal_exact_match_nested_key
  - ✅ test_get_internal_nonexistent_key (before/between/after existing keys)
  - ✅ test_get_internal_partial_match_no_such_path (critical edge case)
  - ✅ test_get_internal_multiple_keys_same_variable
  - ✅ test_get_internal_different_variables
  - ✅ test_get_internal_returns_nodedata_with_flags
  - ✅ test_get_internal_with_tree_splits
  - ✅ test_get_internal_deep_nesting
- ✅ All 39 tests in rumps-storage pass

---

### Step 4: Implement `set_internal()` - NodeData-Based Insertion ✅ COMPLETE

**File**: `crates/rumps-storage/src/btree.rs:930-1063` (private helpers impl block)

**Summary**: Implemented SET operation that accepts `NodeData` directly for creating intermediate nodes with idempotent merge semantics.

**Implementation Complete**:
- ✅ Added `set_internal()` method accepting `NodeData` parameter
- ✅ Implements idempotent merge: `has_descendants` uses OR, `value` prefers new over old
- ✅ Handles root creation, node splitting, and tree navigation
- ✅ Safe for concurrent ancestor creation
- ✅ Added `insert_non_full_with_data()` helper method (btree.rs:1153-1250)
- ✅ Merge semantics preserve `has_descendants` flag across updates
- ✅ All Arc<NodeData> properly wrapped during insertion

---

### Step 5: Implement `update_descendants_flag()` ✅ COMPLETE

**File**: `crates/rumps-storage/src/btree.rs:1252-1282` (private helpers impl block)

**Summary**: Implemented method to update `has_descendants` flag for existing keys while preserving values.

**Implementation Complete**:
- ✅ Added `update_descendants_flag()` method
- ✅ Retrieves existing NodeData via `get_internal()`
- ✅ Creates new NodeData with updated flag and preserved value
- ✅ Delegates to `set_internal()` for merge semantics
- ✅ Returns error if key doesn't exist

---

### Step 6: Implement `ensure_ancestors()` ✅ COMPLETE

**File**: `crates/rumps-storage/src/btree.rs:1284-1340` (private helpers impl block)

**Summary**: Implemented method to ensure all ancestor keys exist with `has_descendants = true` before inserting new keys.

**Implementation Complete**:
- ✅ Added `ensure_ancestors()` method
- ✅ Generates ancestors using `Key::ancestors()`
- ✅ Uses `futures::stream::iter` with `try_for_each` for sequential processing
- ✅ Checks each ancestor: creates if missing, updates flag if exists without it
- ✅ Delegates to `set_internal()` for idempotent ancestor creation
- ✅ Thread-safe for concurrent execution (idempotent merge semantics)
- ✅ Processes ancestors from root to leaf in order

---

### Step 7: Update `set_with_context()` and `insert_non_full()` ✅ COMPLETE

**Files**:
- `crates/rumps-storage/src/btree.rs:741-756` (set_with_context)
- `crates/rumps-storage/src/btree.rs:1094-1108` (insert_non_full)

**Summary**: Updated SET operation to maintain hierarchical semantics by calling `ensure_ancestors()` and preserving `has_descendants` flag on updates.

**Implementation Complete**:
- ✅ Updated `set_with_context()` to call `ensure_ancestors()` before insertion
- ✅ Updated rustdoc to mention ancestor creation in hierarchical semantics
- ✅ Modified `insert_non_full()` to preserve `has_descendants` flag when updating existing keys
- ✅ New keys inserted with `has_descendants=false` initially
- ✅ Existing keys preserve their `has_descendants` flag when value is updated
- ✅ Critical edge case handled: intermediate nodes can have both value and descendants

---

### Step 8: Verify Node Splitting Preserves Flags ✅ VERIFIED

**File**: `crates/rumps-storage/src/btree.rs`

**Summary**: Verified that node splitting correctly preserves `has_descendants` flags when promoting medians to parent nodes.

**Verification Complete**:
- ✅ Reviewed `split_node()` - returns `Arc<NodeData>` which includes `has_descendants` flag
- ✅ Reviewed `insert_non_full()` - correctly inserts median's NodeData into parent with `Arc::clone()`
- ✅ Reviewed `insert_non_full_with_data()` - correctly inserts median's NodeData into parent with `Arc::clone()`
- ✅ All flag information preserved during tree restructuring
- ✅ No additional changes needed - existing implementation correct

---

## Comprehensive Test Suite ✅ COMPLETE

### Unit Tests in `crates/rumps-types/src/key.rs`

All unit tests for `Key::ancestors()` implemented and passing:
- ✅ test_ancestors_empty_key
- ✅ test_ancestors_single_subscript
- ✅ test_ancestors_two_subscripts
- ✅ test_ancestors_deep_nesting

### Integration Tests in `crates/rumps-storage/src/btree.rs:2605-2860`

All 7 hierarchical semantics tests implemented and passing (46 total tests):
- ✅ test_set_creates_ancestors - Basic ancestor creation
- ✅ test_set_deep_nesting_creates_all_ancestors - Deep hierarchy (5 levels)
- ✅ test_set_intermediate_node_becomes_both - **Critical edge case**: node with both value and descendants
- ✅ test_set_preserves_has_descendants_on_update - Flag preservation on value updates
- ✅ test_set_multiple_children_same_parent - Multiple children scenario
- ✅ test_set_sibling_paths - Sibling path independence
- ✅ test_concurrent_ancestor_creation - Concurrent idempotent ancestor creation safety

---

## Comprehensive Implementation Checklist

### Step 1: Arc<NodeData> Wrapper (MUST BE DONE FIRST) ✅ COMPLETE
- [x] **Update Node type** in `crates/rumps-types/src/node.rs`:
  - [x] Change `values: Vec<NodeData>` to `values: Vec<Arc<NodeData>>`
  - [x] Update `Serialize` impl to unwrap Arc using `try_unwrap` or clone
  - [x] Update `Deserialize` impl to wrap deserialized NodeData in Arc::new()
  - [x] Test serialization round-trip (serialize then deserialize)
- [x] **Update all Node construction in btree.rs**:
  - [x] Find all places that create Node instances
  - [x] Wrap all `NodeData` values in `Arc::new()`
  - [x] Update value access patterns to use `Arc::clone()` where needed
- [x] **Run all existing tests** to verify no regressions from Arc change
- [x] **Verify existing B-tree operations work** with Arc-wrapped values

### Step 2: Core Implementation ✅ COMPLETE
- [x] Add `Key::ancestors()` method to `crates/rumps-types/src/key.rs`
- [x] Add unit tests for `Key::ancestors()` (empty, single, two, deep)
- [x] Implement `get_internal()` returning `Arc<NodeData>` with full B-tree navigation
- [x] Implement `search_from_node()` helper for recursive search
- [x] Test `get_internal()` with existing and non-existent keys (10 comprehensive tests)
- [x] Implement `set_internal()` with NodeData merge behavior
  - [x] OR operation on `has_descendants`
  - [x] Value replacement when new value is provided
  - [x] Handle concurrent ancestor creation gracefully
  - [x] Wrap all NodeData in Arc::new() when creating
- [x] Create `insert_non_full_with_data()` helper (wraps values in Arc)
- [x] Implement `update_descendants_flag()`
- [x] Implement `ensure_ancestors()` with sequential processing

### Step 3: Critical Fixes to Existing Code ✅ COMPLETE
- [x] **Fix 1**: Update `insert_non_full()` to preserve `has_descendants` on updates
  - [x] Check if key exists before updating
  - [x] Preserve existing `has_descendants` flag when updating value
- [x] **Fix 2**: Call `ensure_ancestors()` in `set_with_context()` before insertion
- [x] **Fix 3**: Verify median promotion during splits preserves `NodeData` flags

### Step 4: Testing - Unit Tests ✅ COMPLETE
- [x] Test: `Key::ancestors()` with various depths
- [x] Test: Empty key has no ancestors
- [x] Test: Single subscript has no ancestors
- [x] Test: Multiple levels return all prefixes
- [x] Test: `get_internal()` returns correct `NodeData` (10 tests)
- [x] Test: `set_internal()` merges `NodeData` correctly (via integration tests)

### Step 5: Testing - Integration Tests ✅ COMPLETE
- [x] Test: SET creates ancestor with `has_descendants=true`
- [x] Test: SET on deep nesting creates all ancestors
- [x] Test: Intermediate node becomes "both" (value + descendants)
- [x] Test: Updating value preserves `has_descendants` flag
- [x] Test: Multiple children of same parent
- [x] Test: Sibling paths create separate ancestors
- [x] Test: Concurrent ancestor creation is safe (no race conditions)

### Step 6: Quality Assurance ✅ COMPLETE
- [x] All new tests pass (46 total tests: 39 original + 7 new)
- [x] All existing tests still pass
- [x] No critical clippy warnings (only minor doc warnings)
- [x] Documented with rustdoc (all public and private methods)
- [x] Code review for race conditions (idempotent merge semantics)
- [x] Verify thread safety with concurrent operations (test_concurrent_ancestor_creation)

### Step 7: Documentation ✅ COMPLETE
- [x] Add rustdoc to `Key::ancestors()`
- [x] Document `get_internal()` behavior and purpose
- [x] Document `set_internal()` merge semantics explicitly
- [x] Document `ensure_ancestors()` thread safety
- [x] Update `set_with_context()` rustdoc to mention ancestor creation
- [x] Add examples to all new methods

### Step 8: Performance Validation ✅ VERIFIED
- [x] Verify O(d * log n) complexity acceptable for typical depths (2-3 levels)
- [x] Arc-based approach minimizes overhead for hierarchy checks
- [x] Future optimization: caching "known ancestors" (deferred to later phase)

---

## Integration with Future Operations

### GET (Phase 2.3)
- Will use `get_internal()` helper which returns `Arc<NodeData>`
- Public API returns `Option<Value>`, extracting value by cloning:
  ```rust
  pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
      Ok(self.get_internal(name, key).await?.and_then(|arc_data| arc_data.value.clone()))
  }
  ```
- Simple and clear: just clone the `Option<Value>` from the Arc

### DATA (Phase 2.5)
- Will use `get_internal()` to access full `Arc<NodeData>`
- Can cheaply read both `value` and `has_descendants` fields from the Arc
- Returns enum based on `value` and `has_descendants` flags:
  - `NoData` (0): Neither value nor descendants
  - `HasValue` (1): Value only
  - `HasDescendants` (10): Descendants only
  - `Both` (11): Both value and descendants

### KILL (Phase 2.4)
- Must **update** ancestors after deletion
- If all children of a node are killed, set `has_descendants = false`
- This is the inverse operation

### ORDER (Phase 2.6)
- Uses `has_descendants` to know whether to descend or skip subtrees
- Critical for efficient tree navigation

---

## Estimated Complexity

### Lines of Code
- `Key::ancestors()`: ~10 lines + tests (~40 lines)
- `get_internal()`: ~50 lines (tree navigation)
- `set_internal()`: ~60 lines (with merge logic)
- `insert_non_full_with_data()`: ~80 lines
- `update_descendants_flag()`: ~15 lines
- `ensure_ancestors()`: ~30 lines
- Update `set_with_context()`: ~5 lines added
- Update `insert_non_full()`: ~15 lines modified
- Integration tests: ~250 lines

**Total**: ~555 lines

### Time Estimate
- Implementation: 3-4 hours
- Testing and debugging: 2-3 hours
- Code review and verification: 1 hour
- **Total**: 6-8 hours

### Risk Assessment
- **High Risk**: Multiple critical edge cases
  - Flag preservation on updates
  - Concurrent ancestor creation
  - Complex tree navigation
- **High Reward**: Unblocks all other MUMPS primitives
- **No Breaking Changes**: Internal implementation detail
- **Mitigation**: Comprehensive test suite including concurrency tests

---

## Summary

**Status**: ✅ **IMPLEMENTATION COMPLETE**

All 8 implementation steps completed successfully:
- ✅ Step 1: Arc<NodeData> wrapper for efficient hierarchy navigation
- ✅ Step 2: Key::ancestors() method
- ✅ Step 3: get_internal() B-tree navigation
- ✅ Step 4: set_internal() with idempotent merge semantics
- ✅ Step 5: update_descendants_flag() helper
- ✅ Step 6: ensure_ancestors() sequential processing
- ✅ Step 7: Updated set_with_context() and insert_non_full()
- ✅ Step 8: Verified node splitting preserves flags

**Test Results**: All 46 tests passing (39 original + 7 new hierarchical semantics tests)

**Files Modified**:
- `crates/rumps-types/src/key.rs` - Added ancestors() method
- `crates/rumps-storage/src/btree.rs` - Added hierarchy maintenance logic
- `crates/rumps-storage/Cargo.toml` - Added futures dependency

**Ready for**: Implementation of $DATA, $ORDER, and KILL primitives

---

Last Updated: 2025-11-22 (Hierarchical semantics implementation complete - Steps 1-8)
