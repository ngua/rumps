# Reverse Iteration

## Purpose

Defines the primitives and integration points for reverse (descending-key) iteration: `find_predecessor_entry`, `Subscript::successor`, `Key::prefix_upper_bound`, `collect_prefix` `Dir` support, and `MergeState` `Dir` support.

## Requirements

### Requirement: find_predecessor_entry for general reverse lookup

`BTree` SHALL provide `find_predecessor_entry(&self, NodeId, &Key) -> Result<Option<(Key, Arc<NodeData>)>>` that returns the entry with the largest key strictly less than the target. This is the symmetric counterpart to `find_successor_entry`. The method SHALL descend from root, tracking fallback candidates in ancestors, mirroring `find_successor_entry`'s approach in reverse.

#### Scenario: Predecessor exists in same leaf
- **WHEN** `find_predecessor_entry(root, key)` is called and the key before `key` is in the same leaf
- **THEN** the method returns that key-value pair

#### Scenario: Predecessor exists in ancestor
- **WHEN** `find_predecessor_entry(root, key)` is called and the predecessor is a separator key in an ancestor node
- **THEN** the method returns the ancestor's separator key-value pair

#### Scenario: No predecessor exists
- **WHEN** `find_predecessor_entry(root, key)` is called and `key` is less than or equal to the smallest key in the tree
- **THEN** the method returns `None`

### Requirement: Subscript::successor for collation-aware upper bounds

`Subscript` SHALL provide `fn successor(&self) -> Option<Subscript>` returning the smallest `Subscript` strictly greater than `self` within the same type, or `None` if `self` is at the type's ceiling. Successors never cross type boundaries; at each ceiling, `prefix_upper_bound` pops the subscript and tries the next one.

Successor rules:
- `Boolean(false)` -> `Some(Boolean(true))`
- `Boolean(true)` -> `None` (ceiling of `Boolean`)
- `Number(n)` where `n < f64::INFINITY` -> `Some(Number(next_representable_f64(n)))` (`NaN` is handled per `OrderedFloat`'s ordering)
- `Number(f64::INFINITY)` -> `None` (ceiling of `Number`)
- `Char(c)` where `c < char::MAX` -> `Some(Char(next_char(c)))` (next Unicode scalar value, skipping the surrogate gap `0xD800..=0xDFFF`)
- `Char(char::MAX)` -> `None` (ceiling of `Char`)
- `String(s)` -> `Some(String(s + '\0'))` (append null byte; `String` has no ceiling)
- `Json(_)` -> `None` (value space has no computable maximum)

#### Scenario: Boolean successor within type
- **WHEN** `Subscript::Boolean(false).successor()` is called
- **THEN** it returns `Some(Subscript::Boolean(true))`

#### Scenario: Boolean ceiling returns None
- **WHEN** `Subscript::Boolean(true).successor()` is called
- **THEN** it returns `None`

#### Scenario: Number successor within type
- **WHEN** `Subscript::Number(OrderedFloat(1.0)).successor()` is called
- **THEN** it returns `Some(Subscript::Number(OrderedFloat(next_after(1.0))))` where `next_after(1.0)` is the next representable `f64`

#### Scenario: Number ceiling returns None
- **WHEN** `Subscript::Number(OrderedFloat(f64::INFINITY)).successor()` is called
- **THEN** it returns `None`

#### Scenario: Char ceiling returns None
- **WHEN** `Subscript::Char(char::MAX).successor()` is called
- **THEN** it returns `None`

#### Scenario: String successor
- **WHEN** `Subscript::String("NAME".into()).successor()` is called
- **THEN** it returns `Some(Subscript::String("NAME\0".into()))`

#### Scenario: Json has no successor
- **WHEN** `Subscript::Json(value).successor()` is called for any `value`
- **THEN** it returns `None`

### Requirement: Key::prefix_upper_bound for reverse prefix seeking

`Key` SHALL provide `fn prefix_upper_bound(&self) -> Option<Key>` returning the smallest `Key` strictly greater than all keys that start with `self` as a prefix, or `None` if the prefix range extends to the end of the key space.

The method works by replacing the last subscript with its successor. If the last subscript's successor is `None`, it pops the last subscript and tries the second-to-last, continuing until a successor is found or all subscripts are exhausted (returns `None`).

#### Scenario: Simple prefix upper bound
- **WHEN** `Key::from(vec![subscript!(1), subscript!("NAME")]).prefix_upper_bound()` is called
- **THEN** it returns `Some(Key::from(vec![subscript!(1), subscript!("NAME\0")]))`

#### Scenario: Last subscript at type ceiling
- **WHEN** a key's last subscript is `Json(...)` (no successor) and the second-to-last is `String("X")`
- **THEN** `prefix_upper_bound` returns a key with the second-to-last subscript replaced by `String("X\0")` and the `Json` subscript removed

#### Scenario: All subscripts at ceiling
- **WHEN** a key consists entirely of subscripts at their type ceiling (e.g. `[Boolean(true)]`, `[Number(INFINITY), Json(...)]`)
- **THEN** `prefix_upper_bound` returns `None` (prefix range extends to end of key space)

#### Scenario: Empty key
- **WHEN** `Key::new().prefix_upper_bound()` is called
- **THEN** it returns `None` (an empty prefix matches all keys)

### Requirement: collect_prefix supports Dir

`collect_prefix_internal`, `BTree::collect_prefix_at`, `Database::collect_prefix`, and `Transaction::collect_prefix` SHALL accept a `Dir` parameter.

For `Dir::Forward`: `seek(root, Some(&prefix))`, advance forward via `cursor.advance()`, stop when `!key.starts_with(prefix)`. This is identical to the current behavior.

For `Dir::Reverse`: compute `prefix.prefix_upper_bound()`. If `Some(ub)`, call `seek_back(root, Some(&ub))`; if `None`, call `seek_back(root, None)`. Advance backward via `cursor.advance_back()`, stop when `!key.starts_with(prefix)`. If `seek_back` lands on a key that does not start with the prefix (because `ub` exists in the tree as an actual key), the `starts_with` check skips it on the first iteration.

#### Scenario: Reverse prefix iteration
- **WHEN** `collect_prefix` is called with prefix `[1]`, `Dir::Reverse`, and the tree contains keys `[1], [1,1], [1,2], [1,3], [2]`
- **THEN** the stream yields `[1,3], [1,2], [1,1], [1]` (descending order, stopping at the prefix boundary)

#### Scenario: Reverse prefix with no matching keys
- **WHEN** `collect_prefix` is called with prefix `[5]`, `Dir::Reverse`, and the tree contains keys `[1], [2], [3]`
- **THEN** the stream yields zero entries

#### Scenario: Reverse prefix with upper bound in tree
- **WHEN** `collect_prefix` is called with prefix `[1, "NAME"]`, `Dir::Reverse`, the prefix upper bound is `[1, "NAME\0"]`, and the tree happens to contain key `[1, "NAME\0"]`
- **THEN** `seek_back` lands on `[1, "NAME\0"]`, which does not start with `[1, "NAME"]`, so it is skipped; iteration proceeds backward from the last key starting with `[1, "NAME"]`

### Requirement: MergeState supports Dir

`MergeState` SHALL store a `Dir` value from the `Scan`. In reverse mode, `advance_buffered` SHALL iterate the `BTreeMap` backward using `range(...).next_back()`, and the snapshot stream SHALL come from the reverse cursor. The merge comparison SHALL yield the larger key first when `Dir::Reverse`.

#### Scenario: Reverse merge with buffered writes
- **WHEN** `Transaction::collect` is called with `Dir::Reverse` and the transaction has buffered sets at keys `[2, 5]` and the snapshot has keys `[1, 3, 4]`
- **THEN** the merged stream yields `[5, 4, 3, 2, 1]`

#### Scenario: Reverse merge with buffered deletes
- **WHEN** `Transaction::collect` is called with `Dir::Reverse` and the transaction has a buffered delete at key `3` and the snapshot has keys `[1, 2, 3, 4]`
- **THEN** the merged stream yields `[4, 2, 1]`, skipping the deleted key
