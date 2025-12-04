# Architecture Options: Registry Integration

> **STATUS: IMPLEMENTED** (commit `3c3fb0d`)
>
> Option A was chosen and implemented. `BTree` now operates purely on `NodeId`s
> with a root-based API (`get_at`, `set_at`, etc.). The `Database` layer owns
> the `roots: BTreeMap<Name, NodeId>` mapping and handles registry integration.
> See `docs/btree.md` for updated architecture documentation.

---

## Problem (Historical)

The current plan (Phase 4.5) has `BTree` calling registry methods like
`registry_insert()` and `registry_remove()` directly. This is a leaky
abstraction—`BTree` is meant to be an in-memory data structure that shouldn't
know about persistence details like the registry.

## Current Architecture

```text
BTree
├── roots: BTreeMap<Name, NodeId>      ← name→root mapping
├── nodes: HashMap<NodeId, Node>       ← node storage
├── storage: Option<StorageEngine>     ← persistence
└── Methods: get(&Name, &Key), set(&Name, &Key, Value), ...

FileStorageEngine
├── registry_chain: Vec<(PageId, GlobalRegistry)>  ← persistent name→root
└── Methods: registry_get(), registry_insert(), registry_remove()
```

Problem: `BTree` must call `storage.registry_insert()` when creating globals,
coupling it to persistence implementation details.

---

## Option A: Database Owns the Namespace (Recommended)

Move `roots` out of `BTree`. `Database` (Phase 5) owns name→root mapping:

```text
Database
├── roots: BTreeMap<Name, NodeId>   ← single source of truth
├── btree: BTree                           ← operates on NodeIds only
└── storage: Option<FileStorageEngine>     ← persists name→root

BTree (simplified)
├── nodes: HashMap<NodeId, Node>           ← node storage only
├── allocator: Arc<dyn NodeAllocator>
└── NO roots map!
```

### API Changes

Current `BTree` API:
```rust
impl BTree {
    async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>;
    async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()>;
    async fn kill(&self, name: &Name, key: &Key) -> Result<()>;
}
```

New `BTree` API (root-based):
```rust
impl BTree {
    async fn get_at(&self, root: NodeId, key: &Key) -> Result<Option<Value>>;
    async fn set_at(&self, root: NodeId, key: &Key, val: Value) -> Result<()>;
    async fn kill_at(&self, root: NodeId, key: &Key) -> Result<()>;

    /// Create a new empty tree, returning its root.
    async fn create_tree(&self) -> Result<NodeId>;

    /// Delete an entire tree and deallocate all nodes.
    async fn delete_tree(&self, root: NodeId) -> Result<()>;
}
```

### Flow Examples

**SET ^PATIENT(123,"NAME") = "John"**
```
1. Database.set(Global("PATIENT"), key, val)
2. Database looks up "PATIENT" in roots
   ├─ Found: root_id = 42
   └─ Not found (new global):
      a. root_id = btree.create_tree()
      b. storage.registry_insert("PATIENT", root_id)
      c. roots.insert(Global("PATIENT"), root_id)
3. btree.set_at(root_id, key, val)
4. BTree operates purely on NodeIds
```

**GET ^PATIENT(123,"NAME")**
```
1. Database.get(Global("PATIENT"), key)
2. Database looks up "PATIENT" in roots
   ├─ Found: root_id = 42 → btree.get_at(root_id, key)
   └─ Not found: return None (global doesn't exist)
```

**KILL ^PATIENT (entire global)**
```
1. Database.kill(Global("PATIENT"), empty_key)
2. Database looks up "PATIENT" in roots → root_id = 42
3. btree.delete_tree(root_id)  // deallocates all nodes
4. storage.registry_remove("PATIENT")
5. roots.remove(Global("PATIENT"))
```

### Pros
- Clean separation: `Database` owns naming, `BTree` owns tree structure
- `BTree` is a simpler "tree of NodeIds" with no namespace awareness
- Registry calls happen at the layer that cares about names
- Matches Phase 5 architecture where `Database` is the public API
- Easier to test `BTree` in isolation (no mock registry needed)

### Cons
- Requires refactoring existing `BTree` methods
- `Database` must exist even for in-memory usage (or provide a thin wrapper)

## Recommendation

**Option A (Database owns namespace)** is the cleanest long-term architecture:

1. `BTree` becomes a pure tree data structure operating on `NodeId`s
2. `Database` is the natural place for namespace management
3. Registry integration is explicit and contained in one place
4. Aligns with the existing Phase 5 plan where `Database` is the public API

The refactor is larger but results in better separation of concerns.

### Future Benefit: Lazy-Loading Roots

Option A enables lazy-loading roots from the registry (instead of loading all
at startup). Since `Database` owns the namespace, it can load on-demand:

```rust
impl Database {
    async fn ensure_root_loaded(&self, name: &Name) -> Result<Option<NodeId>> {
        // Check in-memory cache first
        if let Some(&id) = self.roots.read().await.get(name) {
            Some(id)
        } else if let Name::Global(n) = name {
            // Lazy load from registry on first access
            self.storage.registry_get(n).await.map(|id| {
                self.roots.write().await.insert(name.clone(), id);
                id
            })
        } else {
            None
        }
    }
}
```

This is impossible with `BTree` owning roots—it would need registry awareness.

---

## Option A: Implementation Plan

### Phase 1: Refactor BTree API (remove roots)

**Files to modify:**
- `crates/rumps-storage/src/btree.rs`
- `crates/rumps-storage/src/btree/tests.rs`

**Step 1.1: Remove `roots` field from `BTree` struct**

```rust
// REMOVE this field:
roots: RwLock<BTreeMap<Name, NodeId>>,
```

**Step 1.2: Rename public methods (name-based → root-based)**

| Current Method | New Method | Notes |
|----------------|------------|-------|
| `get(&Name, &Key)` | `get_at(NodeId, &Key)` | Remove name lookup |
| `get_internal(...)` | `get_at_internal(...)` | Same logic, different entry |
| `set(&Name, &Key, Value, &Ctx)` | `set_at(NodeId, &Key, Value, &Ctx)` | Remove name lookup |
| `set_internal(...)` | `set_at_internal(...)` | Same logic |
| `kill(&Name, &Key, &Ctx)` | `kill_at(NodeId, &Key, &Ctx)` | Remove name lookup |
| `kill_internal(...)` | `kill_at_internal(...)` | Same logic |
| `data(&Name, &Key)` | `data_at(NodeId, &Key)` | Remove name lookup |
| `data_internal(...)` | `data_at_internal(...)` | Same logic |
| `order(&Name, Option<&Key>)` | `order_at(NodeId, Option<&Key>)` | Remove name lookup |
| `order_internal(...)` | `order_at_internal(...)` | Same logic |
| `collects(...)` | `collects_at(...)` | Remove name lookup |
| `collects_internal(...)` | `collects_at_internal(...)` | Same logic |

**Step 1.3: Add new tree lifecycle methods**

```rust
impl BTree {
    /// Create a new empty tree, returning its root NodeId.
    /// Allocates a single empty leaf node.
    pub async fn create_tree(&self) -> Result<NodeId> {
        let root_id = self.allocator.allocate().await?;
        let root = Node::new_leaf();
        self.nodes.write().await.insert(root_id, root);
        Ok(root_id)
    }

    /// Delete an entire tree rooted at `root`, deallocating all nodes.
    /// Returns the number of nodes deallocated.
    pub async fn delete_tree(&self, root: NodeId) -> Result<usize> {
        // Recursive deallocation of all nodes in the tree
        // (Similar to current kill_internal with empty key)
    }
}
```

**Step 1.4: Remove helper methods that reference `roots`**

- `get_or_create_root()` → DELETE (caller provides root)
- `find_root()` → DELETE (caller provides root)
- Any method that accesses `self.roots` → refactor or delete

### Phase 2: Rewrite All BTree Tests

**Test modules to rewrite:**
- `btree::tests::get_internal_tests` (~11 tests)
- `btree::tests::set_internal_tests` (~7 tests)
- `btree::tests::kill_internal_tests` (~35 tests)
- `btree::tests::data_internal_tests` (~12 tests)
- `btree::tests::order_internal_tests` (~15 tests)
- `btree::tests::collects_internal_tests` (~10+ tests)

**Test pattern change:**

```rust
// BEFORE: Tests create BTree, call methods with Name
#[tokio::test]
async fn test_get_existing_key() {
    let btree = BTree::new(3).unwrap();
    let name = Name::Global("TEST".into());
    let key = key![1, 2, 3];

    btree.set(&name, &key, Value::Integer(42), &ctx).await.unwrap();
    let val = btree.get(&name, &key).await.unwrap();
    assert_eq!(val, Some(Value::Integer(42)));
}

// AFTER: Tests create tree explicitly, call methods with root NodeId
#[tokio::test]
async fn test_get_existing_key() {
    let btree = BTree::new(3).unwrap();
    let root = btree.create_tree().await.unwrap();
    let key = key![1, 2, 3];

    btree.set_at(root, &key, Value::Integer(42), &ctx).await.unwrap();
    let val = btree.get_at(root, &key).await.unwrap();
    assert_eq!(val, Some(Value::Integer(42)));
}
```

**Tests that become simpler:**
- No need to test "Global vs Local namespaces are separate" at BTree level
  (that's now Database's responsibility)
- No need for `Name::Global` vs `Name::Local` variants in BTree tests

**Tests that move to Database layer:**
- Namespace separation tests
- "Global requires transaction" tests
- Registry persistence tests

### Phase 3: Create Database Layer (Phase 5 work, pulled forward)

**New file:** `crates/rumps-storage/src/database.rs`

```rust
pub struct Database {
    /// Name → root NodeId mapping (in-memory cache).
    roots: RwLock<BTreeMap<Name, NodeId>>,

    /// The underlying B-tree (operates on NodeIds only).
    btree: Arc<BTree>,

    /// Optional persistent storage (None = in-memory only).
    storage: Option<Arc<FileStorageEngine>>,
}

impl Database {
    /// Open an existing database from disk.
    pub async fn open(path: &Path) -> Result<Self>;

    /// Create a new database at the given path.
    pub async fn create(path: &Path) -> Result<Self>;

    /// Create an in-memory database (no persistence).
    pub fn in_memory() -> Result<Self>;

    // MUMPS operations (delegate to BTree after root lookup)
    pub async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>;
    pub async fn set(&self, name: &Name, key: &Key, val: Value) -> Result<()>;
    pub async fn kill(&self, name: &Name, key: &Key) -> Result<()>;
    pub async fn data(&self, name: &Name, key: &Key) -> Result<DataStatus>;
    pub async fn order(&self, name: &Name, after: Option<&Key>) -> Result<Option<Key>>;

    // Internal: ensure root exists, creating if necessary
    async fn ensure_root(&self, name: &Name) -> Result<NodeId>;
    async fn lookup_root(&self, name: &Name) -> Option<NodeId>;
}
```

### Phase 4: Update persistence.md

After implementing Option A, update `TODOS/persistence.md`:

1. Remove registry calls from Phase 4.5 BTree section
2. Move registry integration to Phase 5 Database section
3. Update the architecture diagrams
4. Mark Phase 4.5 tasks that are now simpler (no registry in BTree)

### Migration Order

1. **Create `database.rs` skeleton** with `in_memory()` constructor
2. **Refactor `BTree`**: rename methods, remove `roots`, add `create_tree()`
3. **Rewrite BTree tests** to use new API (one module at a time)
4. **Implement `Database` methods** that delegate to `BTree`
5. **Add Database tests** for namespace separation, transactions, etc.
6. **Wire up registry** in `Database::open()` / `Database::create()`

### Estimated Scope

| Component | Files | Estimated Changes |
|-----------|-------|-------------------|
| BTree refactor | 2 | ~500 lines changed |
| BTree tests | 1 | ~90 tests rewritten |
| Database impl | 1 (new) | ~300 lines new |
| Database tests | 1 (new) | ~50 tests new |
| persistence.md | 1 | ~50 lines updated |

**Total:** ~800 lines changed/new, ~140 tests affected
