# B-Tree Cursor

## Purpose

Defines the `btree::Cursor` struct for efficient, stateful traversal of the B-tree in both forward and backward directions, replacing the per-entry `get_next_internal` approach in `collect`.

## Requirements

### Requirement: Cursor struct holds traversal state

`btree::Cursor<'a>` SHALL maintain a `SmallVec<[(NodeId, usize); 8]>` stack representing the path from root to the current position, where the `usize` is the key index within that node. The cursor SHALL be `Send` and borrow `&'a BTree` for node loading.

The cursor itself is bound-unaware: `seek` always lands on the first key >= target, and `seek_back` always lands on the last key <= target. The `Included`/`Excluded` distinction from `Bounds` is the caller's responsibility (i.e. `collect_internal`); see the "Bound adjustment" requirement in the range-bounded-queries spec.

#### Scenario: Cursor created via forward seek
- **WHEN** a cursor is created with `Cursor::seek(&btree, root, Some(&key))`
- **THEN** the stack represents the path from root to the first key >= `key`, and the index points to that key's position

#### Scenario: Cursor created from beginning
- **WHEN** a cursor is created with `Cursor::seek(&btree, root, None)`
- **THEN** the stack represents the path from root to the leftmost leaf at index `0`

#### Scenario: Cursor created via backward seek
- **WHEN** a cursor is created with `Cursor::seek_back(&btree, root, Some(&key))`
- **THEN** the stack represents the path from root to the last key <= `key`

#### Scenario: Cursor created from end
- **WHEN** a cursor is created with `Cursor::seek_back(&btree, root, None)`
- **THEN** the stack represents the path from root to the rightmost leaf at the last key index

#### Scenario: Cursor over empty tree
- **WHEN** a cursor is created for a tree with no keys
- **THEN** the cursor is immediately exhausted (first `advance` or `advance_back` yields `None`)

### Requirement: Forward advance is O(1) amortized

`advance` uses yield-then-step semantics: each call yields the entry at the current cursor position, then moves the cursor forward to the next position. The first call after `seek` yields the entry that `seek` landed on. The amortized cost SHALL be `O(1)` per advance after the initial `O(log n)` seek.

Stepping forward means: if the current node is a leaf, increment the key index. If the index exceeds the last key, pop the stack. If the current node is internal, descend into `children[i+1]` and find the leftmost entry in that subtree. When the stack is empty after a pop, the cursor is exhausted.

#### Scenario: First advance after seek
- **WHEN** `advance` is called immediately after `seek`
- **THEN** it yields the entry at the seek position (the first key >= the seek target, or the leftmost key if no target was given), then steps the cursor forward

#### Scenario: Advance within same leaf
- **WHEN** the cursor is in a leaf and there are more keys after the current index
- **THEN** `advance` yields the current entry and increments the index; no new nodes are loaded

#### Scenario: Advance across leaf boundary
- **WHEN** the cursor has just yielded the last key in a leaf
- **THEN** the cursor pops the stack to the parent; the next `advance` yields the parent's separator key (which holds data in this B-tree), then descends into the next child to find the leftmost entry

#### Scenario: Advance at end of tree
- **WHEN** the cursor has yielded the rightmost key in the tree and the stack is empty
- **THEN** `advance` yields `None` and the cursor is exhausted

### Requirement: Backward advance is O(1) amortized

`advance_back` uses the same yield-then-step semantics as `advance`, but in reverse: each call yields the entry at the current cursor position, then moves the cursor backward to the previous position. The first call after `seek_back` yields the entry that `seek_back` landed on. `advance_back` SHALL have `O(1)` amortized cost.

Stepping backward means: if the current node is a leaf, decrement the key index. If the index goes below `0`, pop the stack. If the current node is internal, descend into `children[i]` (the left child) and find the rightmost entry in that subtree. When the stack is empty after a pop, the cursor is exhausted.

#### Scenario: First advance_back after seek_back
- **WHEN** `advance_back` is called immediately after `seek_back`
- **THEN** it yields the entry at the seek_back position (the last key <= the seek target, or the rightmost key if no target was given), then steps the cursor backward

#### Scenario: Backward advance within same leaf
- **WHEN** the cursor is in a leaf at index `i > 0`
- **THEN** `advance_back` yields the current entry and decrements the index

#### Scenario: Backward advance across leaf boundary
- **WHEN** the cursor has just yielded the first key (index `0`) in a leaf
- **THEN** the cursor pops the stack to the parent; the next `advance_back` yields the parent's separator key to the left, then descends into the left child's rightmost entry

#### Scenario: Backward advance at start of tree
- **WHEN** the cursor has yielded the leftmost key in the tree and the stack is empty
- **THEN** `advance_back` yields `None`

### Requirement: Cursor replaces get_next_internal in collect

`collect_internal` SHALL use a `Cursor` instead of calling `get_next_internal` per entry. The cursor direction is determined by the `Dir` in the `Scan` parameter: `Dir::Forward` uses `seek` + `advance`, `Dir::Reverse` uses `seek_back` + `advance_back`. `collect_prefix_internal` SHALL likewise use the cursor with prefix-based early termination.

#### Scenario: collect produces identical results
- **WHEN** `collect` is called with a forward `Scan` and any predicate and extractor
- **THEN** the yielded entries are identical in order and content to the previous `get_next_internal`-based implementation

#### Scenario: collect in reverse yields descending order
- **WHEN** `collect` is called with `Scan { dir: Dir::Reverse, .. }`
- **THEN** entries are yielded from largest to smallest key, filtered by `pred`

#### Scenario: collect_prefix forward terminates at prefix boundary
- **WHEN** `collect_prefix` is called with a prefix and `Dir::Forward`
- **THEN** the cursor seeks to the prefix key via `seek(root, Some(&prefix))`, advances forward, and stops as soon as it encounters a key that does not start with the prefix

#### Scenario: collect_prefix reverse terminates at prefix boundary
- **WHEN** `collect_prefix` is called with a prefix and `Dir::Reverse`
- **THEN** the cursor computes `prefix.prefix_upper_bound()`, calls `seek_back(root, upper_bound.as_ref())`, advances backward, and stops as soon as it encounters a key that does not start with the prefix

### Requirement: Cursor handles concurrent modifications safely

The cursor SHALL hold `NodeId`s (not `Arc<Node>` references), loading nodes on demand via `BTree::load_node`. If a node has been modified between cursor advances (e.g. by a concurrent operation on a different global), the cursor SHALL load the current version. For transaction snapshot isolation, the cursor operates on the snapshot's view of the tree.

#### Scenario: Node evicted from cache between advances
- **WHEN** a node referenced by the cursor's stack is evicted from the in-memory cache between two `advance` calls
- **THEN** the cursor loads the node from disk storage and continues iteration correctly
