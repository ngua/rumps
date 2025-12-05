# RUMPS DSL TODO

RUMPS is still a work in progress, but the core language is settled. RUMPS is statically typed. See the `rumps-query` crate, especially `crates/rumps-query/scripts`, for current language syntax and usage examples.

Only the remaining DSL work is tracked here: `collect` and JSON operators.

## `collect`

The `collect` primitive is the remaining planned iteration form for walking local and global tree data.

```rumps
collect ^data{}
  where (k, v) => { 
    ; cond. expr
  }
  select (k, v) => {
    ; mapping expr
  }
  execute (x) => {
    ; side-effect expr
  }
```

### Element Bindings

Before `select`, each element is passed explicitly as `(k, v)` to closure-based operations.

After `select`, the selected expression becomes the stream element. Later operations that inspect elements take explicit closure arguments.

| Stage           | Bindings                                |
|-----------------|-----------------------------------------|
| before `select` | explicit `(k, v)` closure arguments     |
| after `select`  | explicit selected-item closure argument |

### Operations

| Operation    | Purpose                                                               |
|--------------|-----------------------------------------------------------------------|
| `where`      | Keep elements where an explicit predicate closure is `true`           |
| `while`      | Keep elements until an explicit predicate closure is `false`          |
| `select`     | Transform each element with an explicit closure                       |
| `take`       | Keep at most `n` elements                                             |
| `skip`       | Drop the first `n` elements                                           |
| `take-while` | Keep selected elements until an explicit predicate closure is `false` |
| `skip-while` | Drop selected elements while an explicit predicate closure is `true`  |
| `count`      | Count elements                                                        |
| `aggregate`  | Run one or more aggregate operations                                  |
| `reduce`     | Fold elements with an explicit reducer                                |
| `group by`   | Group elements by key expression                                      |
| `reverse`    | Reverse stream order                                                  |
| `join`       | Join with another tree source                                         |
| `parallel`   | Evaluate stream work concurrently                                     |
| `execute`    | Run side effects for each element with an explicit closure            |

## JSON Operators

| Operator | Purpose                                                           | Example                      |
|----------|-------------------------------------------------------------------|------------------------------|
| `.`      | Get field or index as JSON                                        | `data.name`                  |
| `..`     | Get field or index as text or scalar                              | `data..name`                 |
| `->`     | Get field by dynamic key as JSON                                  | `data->key`                  |
| `->>`    | Get field by dynamic key as text or scalar                        | `data->>key`                 |
| `#>`     | Get value at path as JSON                                         | `data #> ["addr", "city"]`   |
| `#>>`    | Get value at path as text or scalar                               | `data #>> ["addr", "city"]`  |
| `@`      | Path expression                                                   | `data@.addr.city`            |
| `@>`     | Check whether the left JSON value contains the right value        | `full @> partial`            |
| `<@`     | Check whether the left JSON value is contained by the right value | `partial <@ full`            |
| `?`      | Check whether a key or index exists                               | `data ? "name"`              |
| `?\|`    | Check whether any listed key or index exists                      | `data ?\| ["name", "alias"]` |
| `?&`     | Check whether all listed keys or indexes exist                    | `data ?& ["name", "age"]`    |
| `\|\|`   | Concatenate arrays or merge objects                               | `base \|\| update`           |
| `-`      | Delete key, index, or value                                       | `data - "tmp"`               |
| `#-`     | Delete value at path                                              | `data #- ["addr", "zip"]`    |
