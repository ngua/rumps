# Hierarchical Semantics: `has_descendants` Flag Implementation

## Problem Statement

The current `SET` implementation in `crates/rumps-storage/src/btree.rs` does not maintain the `has_descendants` flag on ancestor nodes. This flag is **critical** for MUMPS hierarchical semantics and must be implemented before moving to other primitives.

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
- When extracting values in the public `get()` API, we can use `Arc::try_unwrap()` to avoid cloning if refcount is 1

**Implementation Impact**:
1. **Node Type Change**: `pub values: Vec<Arc<NodeData>>` in `rumps-types/src/node.rs`
2. **Return Type**: `get_internal()` returns `Option<Arc<NodeData>>` instead of `Option<NodeData>`
3. **Node Creation**: All values wrapped with `Arc::new(NodeData { ... })`
4. **Serialization**: Custom serialize/deserialize unwraps/wraps Arc (already have custom impl)
5. **Public API**: `get()` extracts value with smart unwrapping:
   ```rust
   pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
       match self.get_internal(name, key).await? {
           None => Ok(None),
           Some(arc_data) => {
               let node_data = Arc::try_unwrap(arc_data)
                   .unwrap_or_else(|arc| (*arc).clone());
               Ok(node_data.value)
           }
       }
   }
   ```

**Performance Benefits**:
- Hierarchy checks: O(1) Arc clone instead of O(value_size) data clone
- Typical case: Most operations just check `has_descendants` flag, never clone the actual data
- Smart extraction: When refcount is 1, `get()` consumes the Arc without any cloning

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

### Step 2: Add `Key::ancestors()` Method

**File**: `crates/rumps-types/src/key.rs`

**Implementation**:
```rust
impl Key {
    /// Returns all ancestor keys (all prefixes except the full key).
    ///
    /// For a key `Key([a, b, c])`, this returns `[Key([a]), Key([a, b])]`.
    /// Empty keys and single-subscript keys have no ancestors.
    ///
    /// # Examples
    ///
    /// ```
    /// use rumps_types::{Key, Subscript};
    ///
    /// let key = Key::from(vec![
    ///     Subscript::from(123),
    ///     Subscript::from("NAME"),
    /// ]);
    ///
    /// let ancestors = key.ancestors();
    /// assert_eq!(ancestors.len(), 1);
    /// assert_eq!(ancestors[0], Key::from(vec![Subscript::from(123)]));
    /// ```
    pub fn ancestors(&self) -> Vec<Key> {
        (1..self.len())
            .map(|i| Key::from(self.as_slice()[..i].to_vec()))
            .collect()
    }
}
```

**Tests Required**:
- Empty key → empty ancestors
- Single subscript → empty ancestors
- Two subscripts → one ancestor
- Deep nesting (5+ levels) → all prefixes

---

### Step 3: Implement `get_internal()` - B-Tree Navigation

**File**: `crates/rumps-storage/src/btree.rs` (private helpers impl block)

**Purpose**: Navigate the B-tree to retrieve full `NodeData` (not just `Value`).

**Implementation**:
```rust
/// Internal GET that returns Arc<NodeData> (not just Value).
///
/// Returns an Arc for efficient hierarchy navigation - checking `has_descendants`
/// flags is much cheaper with Arc::clone() than cloning the entire NodeData.
///
/// The public `get()` method will extract the value using Arc::try_unwrap()
/// when possible, avoiding clones when the refcount is 1.
async fn get_internal(&self, name: &Name, key: &Key) -> Result<Option<Arc<NodeData>>> {
    match self.roots.read().await.get(name).copied() {
        None => Ok(None),
        Some(root_id) => self.search_from_node(root_id, key).await,
    }
}

/// Recursively search for a key starting from the given node.
///
/// Returns Arc<NodeData> for cheap cloning during hierarchy navigation.
fn search_from_node<'a>(
    &'a self,
    node_id: NodeId,
    key: &'a Key,
) -> Pin<Box<dyn Future<Output = Result<Option<Arc<NodeData>>>> + Send + 'a>>
{
    Box::pin(async move {
        let node = self.find_node(node_id).await?;

        match node.keys.binary_search(key) {
            Ok(pos) => Ok(Some(Arc::clone(&node.values[pos]))),
            Err(pos) => if node.is_leaf {
                Ok(None)
              } else { 
                self.search_from_node(node.children[pos], key).await
            }
        }
    })
}
```

**Critical Considerations**:
- Must handle partial matches correctly (e.g., searching for `Key([1,2,3])` when tree only has `Key([1])` and `Key([1,2,5])`)
- Must work for both leaf and internal nodes
- Returns `Arc<NodeData>` for cheap cloning during hierarchy checks

**Note on Node Type**:
This requires updating `Node` in `rumps-types/src/node.rs`:
```rust
pub struct Node {
    pub keys: Vec<Key>,
    pub children: Vec<NodeId>,
    pub values: Vec<Arc<NodeData>>,  // ← Changed from Vec<NodeData>
    pub is_leaf: bool,
}
```

---

### Step 4: Implement `set_internal()` - NodeData-Based Insertion

**File**: `crates/rumps-storage/src/btree.rs` (private helpers impl block)

**Purpose**: SET operation that accepts `NodeData` directly (for creating intermediate nodes).

**Critical Behavior - Idempotent Merge**:

When a key already exists, this method MERGES the NodeData:
- `has_descendants`: Performs OR operation (if either old or new is true, result is true)
- `value`: Takes new value if provided, otherwise keeps old value

This ensures:
1. Setting `has_descendants=true` is permanent (can't be undone by another set)
2. Concurrent ancestor creation is safe (multiple threads can set same ancestor)
3. User can update values without losing `has_descendants` flag

**Implementation**:
```rust
/// Internal SET that accepts NodeData directly.
///
/// # Behavior for Existing Keys
///
/// If the key already exists, this method MERGES the NodeData:
/// - `has_descendants`: Performs OR operation (if either old or new is true, result is true)
/// - `value`: Takes new value if provided, otherwise keeps old value
///
/// This ensures that:
/// 1. Setting has_descendants=true is permanent (can't be undone by another set)
/// 2. Concurrent ancestor creation is safe (multiple threads can set same ancestor)
/// 3. User can update values without losing has_descendants flag
async fn set_internal(&self, name: &Name, key: &Key, data: NodeData) -> Result<()> {
    // Similar structure to set_with_context, but accepts NodeData
    // When inserting into a node, check if key exists and merge NodeData if needed
    // Implementation should delegate to insert_non_full_with_data()
    todo!("Implement set_internal with NodeData merging")
}
```

**Required Refactoring**:

The current `insert_non_full()` must be updated to handle NodeData merging. Create a version that accepts `NodeData`:

```rust
fn insert_non_full_with_data<'a>(
    &'a self,
    node_id: NodeId,
    key: &'a Key,
    data: NodeData,
) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>
{
    Box::pin(async move {
        let node = self.find_node(node_id).await?;
        let pos = node.keys.binary_search(key).unwrap_or_else(|insert_pos| insert_pos);

        if node.is_leaf {
            let mut updated_node = node;

            match updated_node.keys.get(pos) {
                Some(existing_key) if existing_key == key => {
                    // Key exists - MERGE NodeData
                    let existing_data = &updated_node.values[pos];
                    let merged_data = NodeData::new(
                        data.value.or_else(|| existing_data.value.clone()),
                        existing_data.has_descendants || data.has_descendants,
                    );
                    updated_node.values[pos] = Arc::new(merged_data);
                }
                _ => {
                    // Key doesn't exist - insert new
                    updated_node.keys.insert(pos, key.clone());
                    updated_node.values.insert(pos, Arc::new(data));
                }
            }

            let mut nodes = self.nodes.write().await;
            nodes.insert(node_id, updated_node);
            Ok(())
        } else {
            // Internal node logic with splitting
            // ...
        }
    })
}
```

---

### Step 5: Implement `update_descendants_flag()`

**File**: `crates/rumps-storage/src/btree.rs` (private helpers impl block)

**Purpose**: Update `has_descendants` flag for an existing key.

**Implementation**:
```rust
/// Updates the has_descendants flag for an existing key.
///
/// This is used when an ancestor already exists but needs its flag updated.
async fn update_descendants_flag(&self, name: &Name, key: &Key, value: bool) -> Result<()> {
    // Use set_internal with merged NodeData
    let existing_arc = self.get_internal(name, key).await?
        .ok_or_else(|| StorageError::NodeNotFound(/* key info */))?;

    // Clone the NodeData to update the flag
    let updated_data = NodeData::new(existing_arc.value.clone(), value);
    self.set_internal(name, key, updated_data).await
}
```

---

### Step 6: Implement `ensure_ancestors()`

**File**: `crates/rumps-storage/src/btree.rs` (private helpers impl block)

**Purpose**: Ensure all ancestor keys exist with `has_descendants = true`.

**Implementation**:
```rust
/// Ensures all ancestor keys exist with `has_descendants = true`.
///
/// This method is called before inserting a new key to maintain the
/// hierarchical structure. For each ancestor that doesn't exist, it
/// creates an intermediate node (no value, only descendants).
///
/// # Thread Safety
///
/// This method is safe for concurrent execution. If multiple threads
/// try to create the same ancestor, `set_internal()` will merge the
/// NodeData using OR semantics on `has_descendants`.
async fn ensure_ancestors(&self, name: &Name, key: &Key) -> Result<()> {
    let ancestors = key.ancestors();

    // Process each ancestor from root to leaf
    ancestors.into_iter()
        .try_for_each(|ancestor_key| async move {
            match self.get_internal(name, &ancestor_key).await? {
                Some(node_data) => {
                    // Ancestor exists - update has_descendants if needed
                    if !node_data.has_descendants {
                        self.update_descendants_flag(name, &ancestor_key, true).await?;
                    }
                    Ok(())
                }
                None => {
                    // Ancestor doesn't exist - create intermediate node
                    self.set_internal(name, &ancestor_key, NodeData::with_descendants()).await
                }
            }
        })
        .await
}
```

**Note**: This implementation requires converting to use `futures::stream::iter` and `try_for_each` to properly await each async operation in sequence.

---

### Step 7: Update Current `set_with_context()`

**File**: `crates/rumps-storage/src/btree.rs` (public impl block)

**Critical Fix**: Preserve `has_descendants` flag when updating existing keys.

**Changes Required**:

1. **Before insertion**, call `ensure_ancestors()`:
```rust
pub async fn set_with_context(
    &self,
    name: &Name,
    key: &Key,
    value: rumps_types::Value,
    _context: Option<()>,
) -> Result<()> {
    // NEW: Ensure all ancestors exist with has_descendants=true
    self.ensure_ancestors(name, key).await?;

    // EXISTING: Rest of implementation
    // ...
}
```

2. **In `insert_non_full()`**, preserve `has_descendants` when updating:
```rust
// In the leaf branch where we update existing keys:
match updated_node.keys.get(pos) {
    Some(existing_key) if existing_key == key => {
        // Key exists - CRITICAL: preserve has_descendants flag
        let existing_has_descendants = updated_node.values[pos].has_descendants;
        updated_node.values[pos] = Arc::new(NodeData::new(Some(value), existing_has_descendants));
    }
    _ => {
        // Key doesn't exist - insert with has_descendants=false initially
        updated_node.keys.insert(pos, key.clone());
        updated_node.values.insert(pos, Arc::new(NodeData::with_value(value)));
    }
}
```

**Why This Matters**:
- Scenario: User sets `^VAR(1) = "parent"`, then sets `^VAR(1,2) = "child"`
- After first SET: `^VAR(1)` has `value=Some("parent"), has_descendants=false`
- After second SET: `^VAR(1)` should become `value=Some("parent"), has_descendants=true`
- If we then update `^VAR(1) = "new parent"`, we MUST preserve `has_descendants=true`

---

### Step 8: Verify Node Splitting Preserves Flags

**File**: `crates/rumps-storage/src/btree.rs`

**Task**: Review `insert_non_full()` to ensure that when a median is promoted to a parent during a split, its `NodeData` is inserted correctly with the `has_descendants` flag preserved.

**Current behavior in `split_node()`**:
```rust
// In split_node()
let median_value = values.pop().ok_or_else(...)?;
// Returns (median_key, median_value, right_id)
```

This is already CORRECT - `split_node()` returns the median's `NodeData` which includes the `has_descendants` flag. Just verify that the parent receives this data intact.

---

## Comprehensive Test Suite

### Unit Tests in `crates/rumps-types/src/key.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ancestors_empty_key() {
        let key = Key::from(vec![]);
        assert_eq!(key.ancestors().len(), 0);
    }

    #[test]
    fn test_ancestors_single_subscript() {
        let key = Key::from(vec![123.into()]);
        assert_eq!(key.ancestors().len(), 0);
    }

    #[test]
    fn test_ancestors_two_subscripts() {
        let key = Key::from(vec![123.into(), "NAME".into()]);
        let ancestors = key.ancestors();
        assert_eq!(ancestors.len(), 1);
        assert_eq!(ancestors[0], Key::from(vec![123.into()]));
    }

    #[test]
    fn test_ancestors_deep_nesting() {
        let key = Key::from(vec![
            1.into(), 2.into(), 3.into(), 4.into(), 5.into()
        ]);
        let ancestors = key.ancestors();
        assert_eq!(ancestors.len(), 4);
        assert_eq!(ancestors[0], Key::from(vec![1.into()]));
        assert_eq!(ancestors[1], Key::from(vec![1.into(), 2.into()]));
        assert_eq!(ancestors[2], Key::from(vec![1.into(), 2.into(), 3.into()]));
        assert_eq!(ancestors[3], Key::from(vec![1.into(), 2.into(), 3.into(), 4.into()]));
    }
}
```

### Integration Tests in `crates/rumps-storage/src/btree.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use rumps_types::{Name, Key, Value};

    #[tokio::test]
    async fn test_set_creates_ancestors() {
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("PATIENT".into());

        // Set a nested key
        let key = Key::from(vec![123.into(), "NAME".into()]);
        btree.set(&name, &key, Value::String("John".into())).await.unwrap();

        // Verify ancestor was created
        let ancestor_key = Key::from(vec![123.into()]);
        let ancestor_arc = btree.get_internal(&name, &ancestor_key).await.unwrap();

        assert!(ancestor_arc.is_some());
        let data = ancestor_arc.unwrap();
        assert!(data.value.is_none()); // No value on ancestor
        assert!(data.has_descendants);  // But has descendants
    }

    #[tokio::test]
    async fn test_set_deep_nesting() {
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Set deeply nested key
        let key = Key::from(vec![1.into(), 2.into(), 3.into(), 4.into(), 5.into()]);
        btree.set(&name, &key, Value::Integer(42)).await.unwrap();

        // Verify all 4 ancestors have has_descendants=true
        ancestors.into_iter().for_each(|ancestor_key| async {
            let data = btree.get_internal(&name, &ancestor_key).await.unwrap().unwrap();
            assert!(data.has_descendants);
            assert!(data.value.is_none()); // Intermediate nodes have no value
        });
    }

    #[tokio::test]
    async fn test_set_intermediate_node_becomes_both() {
        // CRITICAL EDGE CASE
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // 1. Set ^VAR(1,"A") = "child1"
        //    → Creates ^VAR(1) with has_descendants=true, no value
        let key_child = Key::from(vec![1.into(), "A".into()]);
        btree.set(&name, &key_child, Value::String("child1".into())).await.unwrap();

        // Verify ancestor exists
        let key_parent = Key::from(vec![1.into()]);
        let arc = btree.get_internal(&name, &key_parent).await.unwrap().unwrap();
        assert!(arc.value.is_none());
        assert!(arc.has_descendants);

        // 2. Set ^VAR(1) = "parent_value"
        //    → Must preserve has_descendants=true AND add value
        btree.set(&name, &key_parent, Value::String("parent_value".into())).await.unwrap();

        // Verify ^VAR(1) has both value and has_descendants=true
        let arc = btree.get_internal(&name, &key_parent).await.unwrap().unwrap();
        assert_eq!(arc.value, Some(Value::String("parent_value".into())));
        assert!(arc.has_descendants);
    }

    #[tokio::test]
    async fn test_set_preserves_has_descendants_on_update() {
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        let key_parent = Key::from(vec![1.into()]);
        let key_child = Key::from(vec![1.into(), 2.into()]);

        // 1. Set ^VAR(1) = "first"
        btree.set(&name, &key_parent, Value::String("first".into())).await.unwrap();

        // 2. Set ^VAR(1,2) = "child" → ^VAR(1).has_descendants becomes true
        btree.set(&name, &key_child, Value::String("child".into())).await.unwrap();

        // Verify flag was set
        let arc = btree.get_internal(&name, &key_parent).await.unwrap().unwrap();
        assert!(arc.has_descendants);

        // 3. Set ^VAR(1) = "updated"
        btree.set(&name, &key_parent, Value::String("updated".into())).await.unwrap();

        // Verify ^VAR(1) still has has_descendants=true after update
        let arc = btree.get_internal(&name, &key_parent).await.unwrap().unwrap();
        assert_eq!(arc.value, Some(Value::String("updated".into())));
        assert!(arc.has_descendants); // MUST still be true
    }

    #[tokio::test]
    async fn test_set_multiple_children_same_parent() {
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Set ^VAR(1,"A"), ^VAR(1,"B"), ^VAR(1,"C")
        btree.set(&name, &Key::from(vec![1.into(), "A".into()]), Value::Integer(1)).await.unwrap();
        btree.set(&name, &Key::from(vec![1.into(), "B".into()]), Value::Integer(2)).await.unwrap();
        btree.set(&name, &Key::from(vec![1.into(), "C".into()]), Value::Integer(3)).await.unwrap();

        // Verify ^VAR(1) has has_descendants=true
        let arc = btree.get_internal(&name, &Key::from(vec![1.into()])).await.unwrap().unwrap();
        assert!(arc.has_descendants);
    }

    #[tokio::test]
    async fn test_set_sibling_paths() {
        let btree = BTree::new(3).unwrap();
        let name = Name::Global("VAR".into());

        // Set ^VAR(1,2), ^VAR(1,3), ^VAR(2,2)
        btree.set(&name, &Key::from(vec![1.into(), 2.into()]), Value::Integer(12)).await.unwrap();
        btree.set(&name, &Key::from(vec![1.into(), 3.into()]), Value::Integer(13)).await.unwrap();
        btree.set(&name, &Key::from(vec![2.into(), 2.into()]), Value::Integer(22)).await.unwrap();

        // Verify ^VAR(1) and ^VAR(2) both have has_descendants
        let arc1 = btree.get_internal(&name, &Key::from(vec![1.into()])).await.unwrap().unwrap();
        let arc2 = btree.get_internal(&name, &Key::from(vec![2.into()])).await.unwrap().unwrap();
        assert!(arc1.has_descendants);
        assert!(arc2.has_descendants);
    }

    #[tokio::test]
    async fn test_concurrent_ancestor_creation() {
        use futures::future::join_all;

        let btree = Arc::new(BTree::new(3).unwrap());
        let name = Name::Global("VAR".into());

        // Spawn multiple tasks creating children of same parent concurrently
        let tasks = (0..10).map(|i| {
            let btree = Arc::clone(&btree);
            let name = name.clone();
            tokio::spawn(async move {
                let key = Key::from(vec![1.into(), i.into()]);
                btree.set(&name, &key, Value::Integer(i)).await
            })
        });

        // Wait for all to complete
        let results: Vec<_> = join_all(tasks).await;
        results.into_iter().for_each(|result| {
            result.unwrap().unwrap();
        });

        // Verify parent was created exactly once with has_descendants=true
        let arc = btree.get_internal(&name, &Key::from(vec![1.into()])).await.unwrap().unwrap();
        assert!(arc.has_descendants);
        assert!(arc.value.is_none());
    }
}
```

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

### Step 2: Core Implementation
- [ ] Add `Key::ancestors()` method to `crates/rumps-types/src/key.rs`
- [ ] Add unit tests for `Key::ancestors()` (empty, single, two, deep)
- [ ] Implement `get_internal()` returning `Arc<NodeData>` with full B-tree navigation
- [ ] Implement `search_from_node()` helper for recursive search
- [ ] Test `get_internal()` with existing and non-existent keys
- [ ] Implement `set_internal()` with NodeData merge behavior
  - [ ] OR operation on `has_descendants`
  - [ ] Value replacement when new value is provided
  - [ ] Handle concurrent ancestor creation gracefully
  - [ ] Wrap all NodeData in Arc::new() when creating
- [ ] Create `insert_non_full_with_data()` helper (wraps values in Arc)
- [ ] Implement `update_descendants_flag()`
- [ ] Implement `ensure_ancestors()` with sequential processing
- [ ] Optional: Add `key_exists()` helper for clarity

### Step 3: Critical Fixes to Existing Code
- [ ] **Fix 1**: Update `insert_non_full()` to preserve `has_descendants` on updates
  - [ ] Check if key exists before updating
  - [ ] Preserve existing `has_descendants` flag when updating value
- [ ] **Fix 2**: Call `ensure_ancestors()` in `set_with_context()` before insertion
- [ ] **Fix 3**: Verify median promotion during splits preserves `NodeData` flags

### Step 4: Testing - Unit Tests
- [ ] Test: `Key::ancestors()` with various depths
- [ ] Test: Empty key has no ancestors
- [ ] Test: Single subscript has no ancestors
- [ ] Test: Multiple levels return all prefixes
- [ ] Test: `get_internal()` returns correct `NodeData`
- [ ] Test: `set_internal()` merges `NodeData` correctly

### Step 5: Testing - Integration Tests
- [ ] Test: SET creates ancestor with `has_descendants=true`
- [ ] Test: SET on deep nesting creates all ancestors
- [ ] Test: Intermediate node becomes "both" (value + descendants)
- [ ] Test: Updating value preserves `has_descendants` flag
- [ ] Test: Multiple children of same parent
- [ ] Test: Sibling paths create separate ancestors
- [ ] Test: Concurrent ancestor creation is safe (no race conditions)
- [ ] Test: SET then GET returns same value (with ancestors)
- [ ] Test: Verify ancestors have no values (only has_descendants)

### Step 6: Quality Assurance
- [ ] All new tests pass
- [ ] All existing tests still pass
- [ ] No clippy warnings
- [ ] Documented with rustdoc (all public and private methods)
- [ ] Remove TODO comment in btree.rs about missing hierarchy support
- [ ] Code review for race conditions
- [ ] Verify thread safety with concurrent operations

### Step 7: Documentation
- [ ] Add rustdoc to `Key::ancestors()`
- [ ] Document `get_internal()` behavior and purpose
- [ ] Document `set_internal()` merge semantics explicitly
- [ ] Document `ensure_ancestors()` thread safety
- [ ] Update `set_with_context()` rustdoc to mention ancestor creation
- [ ] Add examples to all new methods

### Step 8: Performance Validation
- [ ] Verify O(d * log n) complexity acceptable for typical depths (2-3 levels)
- [ ] Profile ancestor creation overhead
- [ ] Consider caching "known ancestors" for future optimization (not in initial implementation)

---

## Integration with Future Operations

### GET (Phase 2.3)
- Will use `get_internal()` helper which returns `Arc<NodeData>`
- Public API returns `Option<Value>`, extracting value with smart unwrapping:
  ```rust
  pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>> {
      match self.get_internal(name, key).await? {
          None => Ok(None),
          Some(arc_data) => {
              // Try to unwrap Arc if refcount is 1, otherwise clone
              let node_data = Arc::try_unwrap(arc_data)
                  .unwrap_or_else(|arc| (*arc).clone());
              Ok(node_data.value)
          }
      }
  }
  ```
- This approach optimizes for the common case where GET has exclusive access to the Arc

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

Last Updated: 2025-11-21 (Restructured with Arc<NodeData> optimization for efficient hierarchy navigation)
