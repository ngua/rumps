# RUMPS DSL Design Document

This document outlines the design of the RUMPS Domain-Specific Language (DSL), a declarative descendant of MUMPS that replaces imperative loops with functional stream operations.

## Core Philosophy

RUMPS is **not** a reimplementation of MUMPS, but rather a **declarative evolution** that:
- Replaces all imperative loops (`FOR`, `WHILE`) with stream-based operations
- Provides functional composition through pipeline operators
- Ensures memory efficiency through lazy evaluation
- Enables automatic parallelization and optimization
- Provides optional type annotations as runtime hints

## Naming Conventions

### Variables

Variable names can use **any case** — uppercase, lowercase, camelCase, PascalCase, or train-case — as long as they don't conflict with reserved keywords:

```rumps
; All valid variable names
SET COUNT = 0
SET count = 0
SET patientName = "John"
SET PatientName = "John"
SET patient-name = "John"
SET PATIENT-ID = 123
```

**Reserved keywords** (cannot be used as variable names):
`LET`, `SET`, `GET`, `KILL`, `COLLECT`, `FUN`, `TRANSACTION`, `IF`, `ELSE`, `WHERE`, `SELECT`, `INTO`, `OUTPUT`, `INTO` etc.

### Train-Case Support

RUMPS supports **train-case** (kebab-case) identifiers, which are common in Lisp-like languages:

```rumps
SET my-var = 123
SET last-visit-date = Time.now()
FUN compute-total (items) { ... }
```

**Important**: Because `-` can appear in identifiers, the subtraction operator **requires spaces**:

```rumps
; Subtraction - spaces required
SET result = total - tax        ; OK: subtraction
SET diff = end-time - start-time  ; OK: subtraction of two train-case vars

; Identifiers - no spaces around hyphen
SET my-var = 123                ; OK: train-case identifier

; Ambiguous - parse error
SET x = a-b                     ; ERROR: use spaces for subtraction
```

### Namespaces and Types

- **Namespaces**: PascalCase (`Json`, `String`, `Math`)
- **Types**: PascalCase (`Int`, `String`, `Array[T]`, `Option[T]`)
- **Namespace functions**: PascalCase.train-case (`Json.parse`, `String.split`, `Math.sqrt`)

## Variable Binding: `LET` vs `SET`

RUMPS distinguishes between two kinds of variable binding with different semantics and performance characteristics.

### `LET`: Simple Lexical Binding

`LET` creates a simple, non-subscriptable binding in lexical scope. These bindings:
- Are **synchronous** (no async DB machinery)
- Live on the **call stack** (not in the B-tree)
- **Cannot be subscripted** (`LET x(1) = ...` is a syntax error)
- Are **scoped** to the enclosing block

```rumps
LET x = 10
LET name = "John"
LET result = compute-something()

; x(1) = 2  ; ERROR: LET bindings cannot be subscripted
```

Use `LET` for:
- Temporaries and intermediate results
- Function-local computations
- Top-level script bindings
- Any binding that doesn't need subscripting

### `SET`: Tree-Structured Variables

`SET` creates or modifies a variable in the B-tree storage. These variables:
- Are **asynchronous** (backed by B-tree operations)
- Can be **subscripted** (`SET x(1, 2) = "value"`)
- Are either **local** (`x`) or **global** (`^X`)
- Globals require a **transaction**

```rumps
; Local B-tree variable (ephemeral, session-scoped)
SET x = 10
SET x(1) = "first"
SET x(1, "nested") = "deep"

; Global B-tree variable (persistent, requires transaction)
TRANSACTION {
  SET ^PATIENT(123, "NAME") = "John"
}
```

Use `SET` for:
- Data that needs hierarchical/subscripted access
- Persistent storage (globals)
- MUMPS-style sparse arrays

### Summary

| Construct    | Subscriptable        | Storage         | Async | Scope                     |
|--------------|----------------------|-----------------|-------|---------------------------|
| `LET x = 1`  | No                   | Call stack      | No    | Lexical block             |
| `SET x = 1`  | Yes (`SET x(1) = 2`) | B-tree (local)  | Yes   | Session                   |
| `SET ^X = 1` | Yes                  | B-tree (global) | Yes   | Persistent (requires txn) |

## Pragmas

RUMPS uses `PRAGMA` statements to configure database behavior at runtime. Pragmas affect the current session or database settings.

### Syntax

```rumps
PRAGMA <setting> = <value>
PRAGMA <setting>          ; Query current value
```

### Available Pragmas

#### `cache-size`

Controls the maximum number of pages cached in memory.

```rumps
PRAGMA cache-size = 2048   ; Cache up to 2048 pages (~8 MiB at 4KB page size)
PRAGMA cache-size          ; Query current cache size
```

Larger values improve read performance at the cost of memory. Default: `1024` pages.

#### `sync-mode`

Controls when WAL data is synced to disk. Affects durability vs performance trade-off.

```rumps
PRAGMA sync-mode = IMMEDIATE    ; Sync every write (safest, slowest)
PRAGMA sync-mode = ON-COMMIT    ; Sync on transaction commit (default)
PRAGMA sync-mode = PERIODIC(50) ; Sync every 50ms
PRAGMA sync-mode = RELAXED      ; Rely on OS page cache (fastest, least durable)
```

| Mode           | Durability       | Performance | Use Case                          |
|----------------|------------------|-------------|-----------------------------------|
| `IMMEDIATE`    | Maximum          | Lowest      | Critical financial data           |
| `ON-COMMIT`    | Per-transaction  | Balanced    | General use (default)             |
| `PERIODIC(ms)` | Interval-bounded | High        | High-throughput with bounded loss |
| `RELAXED`      | OS-dependent     | Maximum     | Batch imports, analytics, dev     |

#### `wal-max-file-size`

Maximum WAL file size in bytes before rotation to a new file.

```rumps
PRAGMA wal-max-file-size = 134217728  ; 128 MiB
PRAGMA wal-max-file-size = 64M        ; Shorthand: 64 MiB
PRAGMA wal-max-file-size = 1G         ; Shorthand: 1 GiB
```

Default: `64` MiB. Larger files reduce rotation overhead but increase recovery time.

#### `max-pages` (future)

Optional limit on total database pages.

```rumps
PRAGMA max-pages = 1000000  ; Limit to ~4 GiB at 4KB page size
PRAGMA max-pages = NONE     ; Unlimited (default)
```

Useful for resource-constrained environments or testing.

#### `read-only` (future)

Open database in read-only mode.

```rumps
PRAGMA read-only = true
```

Prevents all writes; useful for analytics or replication targets.

### Pragma Scope

- **Session pragmas**: Affect only the current connection (e.g., `cache-size`)
- **Database pragmas**: Persist across sessions, stored in metadata (e.g., `max-pages`)

```rumps
; Session-scoped (reverts on disconnect)
PRAGMA cache-size = 4096

; Database-scoped (persists)
PRAGMA max-pages = 500000
```

### Querying Pragmas

Query current values by omitting the assignment:

```rumps
PRAGMA cache-size
; Returns: 1024

PRAGMA sync-mode
; Returns: ON-COMMIT
```

### Example: High-Throughput Configuration

```rumps
; Optimize for batch import
PRAGMA cache-size = 8192
PRAGMA sync-mode = RELAXED
PRAGMA wal-max-file-size = 256M

TRANSACTION {
  ; ... bulk insert operations ...
}

; Restore safe defaults
PRAGMA sync-mode = ON-COMMIT
```

## Transaction Blocks

RUMPS requires **explicit transactions** for all writes to persistent globals. Transactions provide ACID guarantees and support various conflict resolution strategies.

### Basic Transaction Syntax

```rumps
TRANSACTION {
  SET ^PATIENT(123,"NAME") = "J. DOE"
  SET ^PATIENT(123,"DOB") = "1980-05-15"
  KILL ^PATIENT(122)
}
```

### Transaction with Options

```rumps
TRANSACTION {
  SET ^INVENTORY(item-id,"COUNT") = ^INVENTORY(item-id,"COUNT") - 1
  SET ^ORDER(order-id,"STATUS") = "PROCESSED"
} ON CONFLICT RETRY 3

TRANSACTION {
  ; Critical section that must succeed or fail entirely
  SET ^ACCOUNT(from,"BALANCE") = ^ACCOUNT(from,"BALANCE") - amount
  SET ^ACCOUNT(to,"BALANCE") = ^ACCOUNT(to,"BALANCE") + amount
} ON CONFLICT ABORT

TRANSACTION {
  ; Optimistic update - skip if already modified
  SET ^CACHE(key) = computed-value
} ON CONFLICT SKIP

TRANSACTION {
  ; Last-write-wins semantics
  SET ^CONFIG(setting) = new-value
} ON CONFLICT OVERWRITE
```

### Transaction Options

#### Conflict Resolution
- `ON CONFLICT ABORT` - (default) Fail the transaction if conflict detected
- `ON CONFLICT RETRY n` - Retry up to n times on conflict
- `ON CONFLICT SKIP` - Skip transaction if conflict, continue execution
- `ON CONFLICT OVERWRITE` - Force write, last-write-wins
- `ON CONFLICT DO block` - Custom conflict handler

#### Isolation Levels
```rumps
TRANSACTION WITH ISOLATION SNAPSHOT {
  ; Default - snapshot isolation
}

TRANSACTION WITH ISOLATION SERIALIZABLE {
  ; Stricter isolation for critical operations
}

TRANSACTION WITH ISOLATION READ-COMMITTED {
  ; Lower isolation for better concurrency
}
```

#### Timeout and Priority
```rumps
TRANSACTION WITH TIMEOUT 5000 {  ; 5 second timeout
  ; Long-running operations
}

TRANSACTION WITH PRIORITY HIGH {
  ; Critical operations get preference
}
```

### Nested Transactions (Savepoints)
```rumps
TRANSACTION {
  SET ^ORDER(id,"STATUS") = "PROCESSING"

  SAVEPOINT process-items

  COLLECT ^ORDER(id,"ITEMS")
    EXECUTE {
      TRANSACTION {  ; Nested transaction
        SET ^INVENTORY(key[0],"COUNT") = ^INVENTORY(key[0],"COUNT") - value..qty
      } ON CONFLICT ROLLBACK TO process-items
    }

  SET ^ORDER(id,"STATUS") = "COMPLETED"
}
```

### Transaction Context Variables
```rumps
TRANSACTION {
  ; Built-in transaction context
  ; SET ^AUDIT(Txn.id, "USER") = Session.user       For if/when user support
  SET ^AUDIT(Txn.id, "TIME") = Txn.start-time
  SET ^AUDIT(Txn.id, "OPERATIONS") = Txn.op-count
}
```

### Error Handling in Transactions
```rumps
TRANSACTION {
  SET ^CRITICAL(id) = value
} CATCH e => {
  ; Error handling block
  SET dir = Io.env("LOG_DIR") ?? "./logs"
  OUTPUT TO FILE
    "{dir}/err.log"
    "Transaction failed: {e.message}"
  ; ALERT admin-user                                    For if/when multi-user support
} FINALLY {
  ; Cleanup code that always runs
  ; RELEASE locks                                       What would this do?
}
```

## Fundamental Primitive: COLLECT

The `COLLECT` primitive is the **foundation for ALL iteration over B-tree data** in RUMPS. It creates a lazy stream from a B-tree variable that can be transformed, filtered, and consumed.

### Syntax

```rumps
COLLECT ^DATA
  WHERE condition
  SELECT transformation
  TERMINAL
```

### Element Type Tracking

The interpreter tracks the **element type** as data flows through the pipeline. This determines what implicit bindings are available at each stage.

#### Initial State (before SELECT)

Each element is a `(Key, Option<Value>)` tuple from the B-tree:
- `key`: The current key (subscript path)
- `value`: The value at that key (may be `Option.None` if node has descendants but no value)

Operations like `WHERE`, `WHILE`, and `SELECT` have access to these implicit bindings.

#### After SELECT

`SELECT` transforms elements into a new type. The interpreter tracks this and updates available bindings:

| SELECT Expression               | Element Becomes  | Available Bindings                  |
|---------------------------------|------------------|-------------------------------------|
| `SELECT { id: ..., name: ... }` | Object           | `item` + field names (`id`, `name`) |
| `SELECT value.amount`           | Scalar           | `item`                              |
| `SELECT ^G(key[0], "SUB")`      | Global reference | `item` (can be nested-iterated)     |

**Examples:**

```rumps
; After SELECT { fields }, field names are directly available
COLLECT ^PATIENT
  WHERE key[0] > 100                        ; key/value (pre-SELECT)
  SELECT { id: key[0], name: value.name }
  SORT BY name ASC                          ; 'name' from selected object
  TAKE-WHILE id < 1000                      ; 'id' from selected object
  EXECUTE { log(item) }                     ; 'item' is the selected object

; After SELECT scalar, use 'item'
COLLECT ^SALES
  SELECT value.amount                       ; stream element becomes the _amount_
  SKIP-WHILE item < 100                     ; 'item' is the scalar
  TAKE-WHILE item < 10000
  AGGREGATE SUM INTO total

; SELECT can produce global references
COLLECT ^PATIENT
  SELECT ^VISITS(key[0])              ; element becomes a Global ref
  ; 'item' is now a Global that could be further iterated in a nested COLLECT
```

#### Binding Summary

| Operation                  | When Used         | Implicit Bindings                                        |
|----------------------------|-------------------|----------------------------------------------------------|
| `WHERE`, `WHILE`           | Pre-SELECT        | `key`, `value`                                           |
| `SELECT`                   | Transforms stream | `key`, `value` → produces new type                       |
| `TAKE-WHILE`, `SKIP-WHILE` | Post-SELECT       | `item` + fields (if object)                              |
| `SORT BY`                  | Post-SELECT       | Field names (if object) or expression                    |
| `AGGREGATE`, `COUNT`       | Any               | Operates on stream values                                |
| `TAKE n`, `SKIP n`         | Any               | No element access needed                                 |
| `EXECUTE`                  | Terminal          | Same as current stage (`key`/`value`, `item`, or fields) |
| `INTO`, `OUTPUT`           | Terminal          | No element access needed                                 |

## COLLECT Operations

Operations within a `COLLECT` block are composable and lazy; they don't execute until a terminal operation is reached. The available implicit bindings depend on whether you're before or after a `SELECT` (see [Element Type Tracking](#element-type-tracking) above).

### Filtering Operations

#### WHERE - Filter by predicate (COLLECT-specific)

`WHERE` filters based on `key` and/or `value`. Multiple `WHERE` clauses are ANDed together.

```rumps
COLLECT ^PATIENT
  WHERE key[0] > 100 AND key[0] < 200
  WHERE has-value  ; Multiple WHERE clauses are ANDed together
```

**Note**: For filtering general collections (arrays, etc.), use `FILTER` with an explicit closure. See [General Collection Operations](#general-collection-operations).

#### WHILE - Take while condition is true (early termination)
```rumps
COLLECT ^LOG
  WHILE key[0] <= "2025-01-01"  ; Stops at first false condition
```

### Transformation Operations

#### SELECT - Transform each element (COLLECT-specific)

`SELECT` transforms each element using the implicit `key` and `value` bindings.

```rumps
; Simple selection
COLLECT ^DATA
  SELECT value

; Field extraction
COLLECT ^PATIENT
  SELECT GET(^PATIENT(key[0],"NAME"))

; Object construction
COLLECT ^PATIENT
  SELECT {
    id: key[0],
    name: GET(^PATIENT(key[0],"NAME")),
    dob: GET(^PATIENT(key[0],"DOB"))
  }
```

**Note**: For transforming general collections (arrays, etc.), use `MAP` with an explicit closure. See [General Collection Operations](#general-collection-operations).

### Limiting Operations

#### TAKE / SKIP - Count-based limiting
These operations don't need element access:
```rumps
COLLECT ^LOG
  TAKE 100

COLLECT ^LOG
  SKIP 100
  TAKE 50  ; Get items 101-150
```

#### TAKE-WHILE / SKIP-WHILE - Conditional limiting
These operations access the current element. Bindings depend on position relative to SELECT:
```rumps
; Pre-SELECT: uses key/value
COLLECT ^DATA
  SKIP-WHILE value < 0
  TAKE-WHILE key[0] < 1000

; Post-SELECT with scalar: uses 'item'
COLLECT ^DATA
  SELECT value.score
  TAKE-WHILE item >= 0

; Post-SELECT with object: uses field names
COLLECT ^DATA
  SELECT { id: key[0], score: value.score }
  TAKE-WHILE score >= 0
```

### Aggregation Operations

#### AGGREGATE - Multiple aggregations at once
```rumps
COLLECT ^SALES
  SELECT GET(^SALES(key[0],"AMOUNT"))
  AGGREGATE
    COUNT INTO total-sales
    SUM INTO total-revenue
    AVG INTO avg-sale
    MIN INTO min-sale
    MAX INTO max-sale
```

#### COUNT - Count elements
```rumps
COLLECT ^PATIENT
  COUNT INTO patient-count
```

#### REDUCE - Custom reduction
```rumps
COLLECT ^DATA
  REDUCE WITH custom-reducer INITIAL 0 INTO result
```

### Grouping Operations

#### GROUP BY - Group elements by key
```rumps
COLLECT ^VISITS
  WHERE key[1] == "2025"
  GROUP BY key[0]  ; Group by patient ID
  AGGREGATE COUNT INTO visit-counts
```

### Ordering Operations

#### SORT BY - Sort stream
`SORT BY` uses the current element bindings. Typically used after SELECT:
```rumps
; Sort by object field
COLLECT ^PATIENT
  SELECT { id: key[0], name: GET(^PATIENT(key[0],"NAME")) }
  SORT BY name ASC

; Sort by scalar (use 'item')
COLLECT ^SCORES
  SELECT value.score
  SORT BY item DESC

; Pre-SELECT: sort by key or value
COLLECT ^DATA
  SORT BY key[0] ASC
```

#### REVERSE - Reverse stream order
```rumps
COLLECT ^DATA
  REVERSE
```

### Join Operations

#### JOIN - Join with another variable
```rumps
COLLECT ^ORDER
  JOIN ^CUSTOMER ON key[0] == ^CUSTOMER.key[0]
  SELECT { order: value, customer: ^CUSTOMER.value }
```

### Parallel Processing

#### PARALLEL - Process in parallel
```rumps
COLLECT ^RECORDS
  PARALLEL 10  ; Process up to 10 records concurrently
  SELECT expensive-op(key, value)
```

## Terminal Operations

Terminal operations consume the stream and produce a result.

### INTO - Collect into variable
```rumps
COLLECT ^DATA
  SELECT value
  INTO results  ; Local variable

COLLECT ^DATA
  SELECT value
  INTO ^PROCESSED  ; Global variable (requires transaction)
```

### OUTPUT - Write to console (with formatting options)

The `OUTPUT` operation in RUMPS supports multiple formatting options for flexible console output.

#### Simple Output
```rumps
COLLECT ^DATA
  OUTPUT  ; Each item on new line
```

#### Template Output
```rumps
COLLECT ^PATIENT
  SELECT { id: key[0], name: GET(^PATIENT(key[0],"NAME")) }
  OUTPUT "Patient #{id}: {name}"
```

#### Formatted Output
```rumps
; JSON format
COLLECT ^CONFIG
  OUTPUT AS JSON

; Table format
COLLECT ^STATS
  OUTPUT AS TABLE HEADERS ["Date", "Count", "Average"]

; CSV format
COLLECT ^DATA
  OUTPUT WITH SEPARATOR ","

; XML format (future)
COLLECT ^DATA
  OUTPUT AS XML ROOT "records" ELEMENT "record"
```

#### Output Targets
```rumps
; Standard error
COLLECT ^ERRORS
  OUTPUT TO ERROR

; File output (future)
COLLECT ^DATA
  OUTPUT TO FILE "/tmp/output.txt"

; Network output (future)
COLLECT ^METRICS
  OUTPUT TO HTTP "https://metrics.example.com/api"
```

#### Extended Output Examples
```rumps
; Simple output - each item on a new line
COLLECT ^DATA
  OUTPUT

; Template-based formatting with field interpolation
COLLECT ^PATIENT
  SELECT { id: key[0], name: GET(^PATIENT(key[0],"NAME")) }
  OUTPUT "ID: {id} - Name: {name}"

; JSON output for structured data
COLLECT ^CONFIG
  SELECT { key: key, value: value }
  OUTPUT AS JSON

; Table formatting for reports
COLLECT ^STATS
  SELECT { date: key[0], total: value.sum, avg: value.avg }
  OUTPUT AS TABLE HEADERS ["Date", "Total", "Average"]

; Custom separators and formatting
COLLECT ^LIST
  OUTPUT WITH SEPARATOR ", "  ; Output as comma-separated values

; Conditional output
COLLECT ^ERRORS
  WHERE value.severity == "HIGH"
  OUTPUT TO ERROR  ; Write to stderr instead of stdout
```

This declarative output approach eliminates the need for manual formatting loops and provides consistent, reusable output patterns.

### EXECUTE - Side effects (COLLECT terminal)

`EXECUTE` is an operation that runs a block for each element in the stream. Like other COLLECT operations, it uses implicit bindings based on element type (see [Element Type Tracking](#element-type-tracking)).

```rumps
; Pre-SELECT: key/value available
COLLECT ^TASKS
  WHERE value.status == "pending"
  EXECUTE {
    process(key[0])
    log("Processed task", key)
  }

; Post-SELECT with object: field names available
COLLECT ^PATIENT
  WHERE key[0] > 100
  SELECT { id: key[0], name: value.name }
  EXECUTE {
    log("Processing: " ++ name)
    notify(id)
  }

; Post-SELECT with scalar: item available
COLLECT ^DATA
  SELECT value.score
  EXECUTE {
    log("Score: " ++ item)
  }
```

**Note**: `EXECUTE` is the COLLECT-specific primitive for side effects. For general collections (arrays, etc.), use `FOREACH` with an explicit closure.

## General Collection Operations

These operations work on **any collection** (arrays, ranges, streams, etc.) and require **explicit closures**. They are distinct from `COLLECT`-specific operations which have implicit `key`/`value` bindings.

### MAP - Transform collection elements

`MAP` applies a closure to each element and returns a new collection.

```rumps
; Prefix form: parens around closure (needed to delimit from array arg)
MAP (x => x * 2) [1, 2, 3]              ; => [2, 4, 6]
MAP (n => n * n) (1..5)                 ; => [1, 4, 9, 16, 25]

; Pipeline form: no parens needed (closure is last)
[1, 2, 3] |> MAP x => x * 2             ; => [2, 4, 6]

; With named function
FUN double (x) { x * 2 }
MAP double [1, 2, 3]                    ; => [2, 4, 6]
```

### FILTER - Filter collection elements

`FILTER` keeps elements that satisfy a predicate.

```rumps
; Prefix form: parens around closure
FILTER (x => x > 2) [1, 2, 3, 4, 5]     ; => [3, 4, 5]

; Pipeline form: no parens needed
[1, 2, 3, 4, 5] |> FILTER x => x > 2    ; => [3, 4, 5]

; Chain with pipeline
[1, 2, 3, 4, 5]
  |> FILTER x => x % 2 == 0
  |> MAP x => x * 10                    ; => [20, 40]
```

### FOREACH - Execute side effects

`FOREACH` executes a closure for each element. It does not produce a result; use for side effects only.

```rumps
; Execute for each element (one-liner, no braces needed)
FOREACH (x => OUTPUT x) [1, 2, 3]

; With pipeline operator (multi-line needs braces)
[1, 2, 3] |> FOREACH x => {
  OUTPUT "Processing: {x}" 
  log(x)
}

; Process a range
FOREACH n => {
  SET ^COUNTER(n) = n * n
} (1..100)
```

### REDUCE - Fold collection to single value

`REDUCE` combines all elements into a single value using an accumulator.

```rumps
; Sum an array
REDUCE (acc, x => acc + x) 0 [1, 2, 3, 4, 5]  ; => 15

; With pipeline
[1, 2, 3, 4, 5] |> REDUCE (acc, x => acc + x) 0

; Build a string
["a", "b", "c"] |> REDUCE (acc, s => acc ++ s) ""  ; => "abc"
```

### Pipeline Operator (`|>`)

The pipeline operator threads a value through a series of transformations:

```rumps
; General collection pipeline
[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
  |> FILTER x => x % 2 == 0      ; [2, 4, 6, 8, 10]
  |> MAP x => x * x              ; [4, 16, 36, 64, 100]
  |> REDUCE (a, b => a + b) 0    ; 220 (parens needed: closure not last)

; Mixed with function calls
"hello world"
  |> String.upper
  |> String.split " "
  |> MAP String.reverse
  |> String.join "-"             ; "OLLEH-DLROW"
```

**Note**: The pipeline operator works with general values and functions. For B-tree iteration, use `COLLECT` with its block syntax instead.

### Comparison: COLLECT Operations vs General Operations

| COLLECT-specific | General   | Binding Model                                                 |
|------------------|-----------|---------------------------------------------------------------|
| `WHERE`          | `FILTER`  | Implicit `key`/`value` vs explicit closure                    |
| `SELECT`         | `MAP`     | Implicit `key`/`value` vs explicit closure                    |
| `TAKE-WHILE`     | (same)    | Implicit (`key`/`value` or `item`/fields) vs explicit closure |
| `SORT BY`        | (same)    | Implicit field access vs explicit closure                     |
| `EXECUTE`        | `FOREACH` | Implicit bindings vs explicit closure                         |

```rumps
; COLLECT: implicit bindings track element type through pipeline
COLLECT ^DATA
  WHERE key[0] > 100                   ; implicit key/value
  SELECT { id: key[0], val: value }    ; implicit key/value → object
  SORT BY val DESC                     ; implicit field access
  TAKE-WHILE id < 500                  ; implicit field access
  EXECUTE { log(item) }                ; implicit (item is the object)

; General: all operations use explicit closures
[1, 2, 3]
  |> FILTER x => x > 1
  |> MAP x => { id: x, val: x * 2 }
  |> FOREACH obj => log(obj)
```

## Complete Examples

### Example 1: Find patients with recent visits
```rumps
; Traditional MUMPS approach (NOT supported in RUMPS)
; SET COUNT=0
; FOR  SET PID=$ORDER(^PATIENT(PID)) QUIT:PID=""  DO
; . SET LASTVISIT=$GET(^PATIENT(PID,"LASTVISIT"))
; . IF LASTVISIT>20250101 DO
; . . SET COUNT=COUNT+1
; . . WRITE "Patient ",PID," last visited on ",LASTVISIT,!

; RUMPS declarative approach
COLLECT ^PATIENT
  WHERE has-descendants
  WHERE GET(^PATIENT(key[0],"LASTVISIT")) > 20250101
  SELECT {
    id: key[0],
    last-visit: GET(^PATIENT(key[0],"LASTVISIT"))
  }
  OUTPUT "Patient {id} last visited on {last-visit}"

; Get count
COLLECT ^PATIENT
  WHERE has-descendants
  WHERE GET(^PATIENT(key[0],"LASTVISIT")) > 20250101
  COUNT INTO recent-count
```

### Example 2: Top 10 customers by order value
```rumps
COLLECT ^ORDERS
  GROUP BY GET(^ORDERS(key[0],"CUSTOMER-ID"))
  AGGREGATE SUM GET(^ORDERS(key[0],"AMOUNT")) INTO total
  SORT BY total DESC
  TAKE 10
  JOIN ^CUSTOMER ON group-key
  SELECT {
    customer-name: GET(^CUSTOMER(group-key,"NAME")),
    total-orders: total
  }
  OUTPUT AS TABLE HEADERS ["Customer", "Total Orders"]
```

### Example 3: ETL Pipeline
```rumps
; Extract, transform, and load data
TRANSACTION {
  COLLECT ^RAW-DATA
    WHERE key[0] >= last-processed-id
    PARALLEL 5
    WHERE validate-record(key, value)
    WHERE is-valid(value)
    SELECT {
      id: generate-id(),
      data: transform-record(value),
      processed-at: Time.now()
    }
    INTO ^PROCESSED-DATA

  SET last-processed-id = LAST(^RAW-DATA)
}
```

## Comparison with Traditional MUMPS

### Summary Table

| Pattern             | Traditional MUMPS                | RUMPS DSL                 | Benefits               |
|---------------------|----------------------------------|---------------------------|------------------------|
| Simple iteration    | `FOR SET I=$O(^D(I)) Q:I="" DO`  | `COLLECT ^D`              | Cleaner syntax         |
| Filtering           | `IF` statements in loop body     | `WHERE`                   | Declarative intent     |
| Counting            | Manual counter variable          | `COUNT INTO`              | No state management    |
| First N items       | Counter with `QUIT`              | `TAKE n`                  | Clear intent           |
| Aggregation         | Manual accumulator variables     | `AGGREGATE` operations    | Built-in operations    |
| Output              | Multiple `WRITE` statements      | `OUTPUT` with templates   | Flexible formatting    |
| Parallel processing | Not available                    | `PARALLEL n`              | Automatic optimization |
| Error handling      | Manual checks                    | Stream error propagation  | Consistent handling    |

### Detailed Pattern Comparison

The following table shows common MUMPS iteration patterns and their conceptual RUMPS equivalents:

| Traditional MUMPS (Imperative) | Future RUMPS DSL (Declarative) | Description |
|--------------------------------|--------------------------------|-------------|
| ```mumps```<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. WRITE ID,!` | ```rumps```<br/>`COLLECT ^DATA`<br/>`  SELECT key[0]`<br/>`  OUTPUT` | Iterate through all top-level keys |
| ```mumps```<br/>`SET CNT=0`<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET CNT=CNT+1`<br/>`WRITE "Total: ",CNT,!` | ```rumps```<br/>`COLLECT ^PAT`<br/>`  COUNT INTO tot`<br/>`WRITE "Total: ",tot,!` | Count entries |
| ```mumps```<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. IF ID>100 QUIT`<br/>`. ; Process ID` | ```rumps```<br/>`COLLECT ^DATA`<br/>`  WHILE key[0] <= 100`<br/>`  ; Process automatically` | Early termination with condition |
| ```mumps```<br/>`SET I=0`<br/>`FOR  SET ID=$ORDER(^LOG(ID)) QUIT:ID=""  DO`<br/>`. SET I=I+1`<br/>`. IF I>10 QUIT`<br/>`. ; Process first 10` | ```rumps```<br/>`COLLECT ^LOG`<br/>`  TAKE 10`<br/>`  ; Process automatically` | Take first N entries |
| ```mumps```<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET NAME=$GET(^PAT(ID,"NAME"))`<br/>`. IF NAME["Smith" DO`<br/>`. . ; Process Smith patients` | ```rumps```<br/>`COLLECT ^PAT`<br/>`  WHERE has-descendants`<br/>`  WHERE GET(^PAT(key[0],"NAME")) contains "Smith"`<br/>`  ; Process automatically` | Filter with condition |
| ```mumps```<br/>`KILL RESULTS`<br/>`SET CNT=0`<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. SET CNT=CNT+1`<br/>`. SET RESULTS(CNT)=$GET(^DATA(ID,"VAL"))` | ```rumps```<br/>`COLLECT ^DATA`<br/>`  SELECT GET(^DATA(key[0],"VAL"))`<br/>`  INTO RESULTS` | Collect into array |
| ```mumps```<br/>`FOR  SET D=$ORDER(^LOG(2025,D)) QUIT:D=""  DO`<br/>`. FOR  SET T=$ORDER(^LOG(2025,D,T)) QUIT:T=""  DO`<br/>`. . ; Process each timestamp` | ```rumps```<br/>`COLLECT ^LOG`<br/>`  WHERE key[0] == 2025 AND key.len == 3`<br/>`  ; All 2025 timestamps, flat` | Nested iteration (flattened) |
| ```mumps```<br/>`; Complex aggregation`<br/>`SET TOT=0,CNT=0`<br/>`FOR  SET ID=$ORDER(^SALE(ID)) QUIT:ID=""  DO`<br/>`. SET AMT=$GET(^SALE(ID,"AMOUNT"))`<br/>`. SET TOT=TOT+AMT,CNT=CNT+1`<br/>`SET AVG=TOT/CNT` | ```rumps```<br/>`COLLECT ^SALE`<br/>`  SELECT GET(^SALE(key[0],"AMOUNT"))`<br/>`  AGGREGATE`<br/>`    SUM INTO total`<br/>`    COUNT INTO count`<br/>`    AVG INTO average` | Aggregation operations |
| ```mumps```<br/>`; Display all patient info`<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET NAME=$GET(^PAT(ID,"NAME"))`<br/>`. SET DOB=$GET(^PAT(ID,"DOB"))`<br/>`. WRITE "Patient ",ID,": ",NAME`<br/>`. WRITE " (DOB: ",DOB,")",!` | ```rumps```<br/>`COLLECT ^PAT`<br/>`  SELECT {`<br/>`    id: key[0],`<br/>`    name: GET(^PAT(key[0],"NAME")),`<br/>`    dob: GET(^PAT(key[0],"DOB"))`<br/>`  }`<br/>`  OUTPUT "Patient {id}: {name} (DOB: {dob})"` | Console output with formatting |

### Key Advantages of RUMPS `COLLECT` Approach

1. **No Manual State**: No need for iteration variables, counters, or manual loop control
2. **Declarative Intent**: The code expresses *what* you want, not *how* to iterate
3. **Automatic Optimization**: The runtime can optimize streaming, batching, and parallelization
4. **Composable Operations**: Stream operations naturally chain together
5. **Memory Efficient**: Lazy evaluation means data isn't loaded until needed
6. **Type Hints**: Optional annotations validated at runtime for documentation and error catching


## Operators

RUMPS modernizes MUMPS operators, making them more readable and consistent with modern programming languages.

### Operator Comparison Table

| Category         | MUMPS         | RUMPS              | Description        | Notes             |
|------------------|---------------|--------------------|--------------------|-------------------|
| **Arithmetic**   |               |                    |                    |                   |
| Addition         | `+`           | `+`                | Add two numbers    | Same              |
| Subtraction      | `-`           | `-`                | Subtract           | Same              |
| Multiplication   | `*`           | `*`                | Multiply           | Same              |
| Division         | `/`           | `/`                | Divide (float)     | Same              |
| Integer Division | `\`           | `//`               | Integer division   | More intuitive    |
| Modulo           | `#`           | `%`                | Remainder          | Standard notation |
| Exponentiation   | `**`          | `^` or `**`        | Power              | Both supported    |
| **String**       |               |                    |                    |                   |
| Concatenation    | `_`           | `++`               | String concat      | More intuitive    |
| Contains         | `[`           | `contains`         | Substring check    | Clearer           |
| Not Contains     | `']`          | `!contains`        | Not substring      | Clearer           |
| Follows          | `]`           | `>`                | String comparison  | Context-aware     |
| Pattern Match    | `?`           | `matches` or `~`   | Regex match        | Modern regex      |
| **Comparison**   |               |                    |                    |                   |
| Equals           | `=`           | `==`               | Equality           | Consistent        |
| Not Equals       | `'=`          | `!=` or `≠`        | Inequality         | Standard          |
| Less Than        | `<`           | `<`                | Less than          | Same              |
| Greater Than     | `>`           | `>`                | Greater than       | Same              |
| Less or Equal    | `<=` or `'>`  | `<=` or `≤`        | Less or equal      | Standard          |
| Greater or Equal | `>=` or `'<`  | `>=` or `≥`        | Greater or equal   | Standard          |
| **Logical**      |               |                    |                    |                   |
| And              | `&` or `&&`   | `AND` or `&&`      | Logical AND        | Clearer           |
| Or               | `!` or `!!`   | `OR` or `\|\|`     | Logical OR         | Standard          |
| Not              | `'`           | `NOT` or `!`       | Logical NOT        | Standard          |
| **Special**      |               |                    |                    |                   |
| Indirection      | `@`           |                    | Dynamic evaluation | Removed           |
| Global Prefix    | `^`           | `^`                | Global variable    | Same              |
| Function Prefix  | `$`           |                    | Built-in function  | Removed           |
| **Assignment**   |               |                    |                    |                   |
| Set              | `SET` or `S`  | `SET` or `=`       | Assignment         | Flexible          |
| Kill             | `KILL` or `K` | `KILL` or `DELETE` | Delete variable    | Options           |
| **New in RUMPS** |               |                    |                    |                   |
| Null Coalesce    | N/A           | `??`               | Default if `None`  | `a ?? b`          |
| Optional Chain   | N/A           | `?.`               | Safe navigation    | `obj?.field`      |
| Pipe             | N/A           | `\|>`              | Pipeline operator  | Functional        |
| Range            | N/A           | `..`               | Range operator     | `1..10`           |
| Spread           | N/A           | `...`              | Spread operator    | `...array`        |
| Type Check       | N/A           | `is`               | Type checking      | `x is Number`     |

### Operator Usage Examples

#### Arithmetic
```rumps
; MUMPS style (not supported)
SET result = 10 + 20 * 3
SET quotient = 100 \ 3  ; Integer division
SET remainder = 100 # 3  ; Modulo

; RUMPS style
SET result = 10 + 20 * 3
SET quotient = 100 // 3  ; More intuitive integer division
SET remainder = 100 % 3   ; Standard modulo notation
SET power = 2^8 ; or 2**8
```

#### String Operations
```rumps
; MUMPS style
SET fullname = first _ " " _ last
IF name["Smith" WRITE "Found Smith"
IF text?1N.N WRITE "All numbers"

; RUMPS style (clearer)
SET fullname = first + " " ++ last
IF name contains "Smith" { OUTPUT "Found Smith" }
IF text matches /^\d+$/ { OUTPUT "All numbers" }
```

#### Comparisons
```rumps
; RUMPS uses standard comparison operators
IF age >= 18 AND age <= 65 {
  SET category = "Working Age"
}
```

#### Logical Operations
```rumps
; Clear, readable boolean logic
IF (status == "ACTIVE" OR override) AND NOT suspended {
  PROCESS record
}

; Short-circuit evaluation
SET valid = exists(user) && user.active && user.age >= 18
```

#### New RUMPS Operators
```rumps
; Null coalescing - use default if null/undefined
SET name = GET(^PATIENT(id, "NAME")) ?? "Unknown"

; Optional chaining - safe navigation
SET city = patient?.address?.city ?? "N/A"

; Pipeline operator for functional composition
[1, 2, 3, 4, 5]
  |> FILTER x => x > 2
  |> MAP x => x * 10
  |> FOREACH x => OUTPUT x

; Range operator (`..`)
FOREACH (n => OUTPUT n) (1..100)

; Spread operator in collections
SET combined = [...array1, ...array2]

; Type checking
IF value is Type.Number {
  SET res = value * 2
} ELSE IF value is Type.String {
  SET res = "Value: " + value
}
```

### Pattern Matching (Enhanced)

RUMPS enhances MUMPS pattern matching with modern regex support:

```rumps
; MUMPS patterns (not supported)
IF ssn?3N1"-"2N1"-"4N { ; Social Security Number format }

; RUMPS regex patterns
; Social Security Number format
IF email matches /^[^@]+@[^@]+\.[^@]+$/ {
  SET valid-email = true
}

; Named capture groups
IF date matches /(?<year>\d{4})-(?<month>\d{2})-(?<day>\d{2})/ {
  SET year = Match.year
  SET month = Match.month
  SET day = Match.day
}
```

## JSON Operators

RUMPS provides comprehensive JSON operators for working with structured data, inspired by PostgreSQL's JSON support but with clearer, more readable syntax.

### Field Access Operators

| Operator | Description                     | Example         | Result             |
|----------|---------------------------------|-----------------|--------------------|
| `.`      | Get field (returns JSON)        | `data.name`     | `"John"` (as JSON) |
| `..`     | Get field (returns text/scalar) | `data..name`    | `John` (as string) |
| `->`     | Get field by key (returns JSON) | `data->"name"`  | `"John"` (as JSON) |
| `->>`    | Get field by key (returns text) | `data->>"name"` | `John` (as string) |

#### Usage Examples
```rumps
SET patient = { "name": "John", "age": 42, "active": true }

; Dot notation (preferred for known fields)
SET name = patient.name           ; JSON string "John"
SET name = patient..name          ; Plain string John

; Arrow notation (for dynamic keys or special characters)
SET field = "name"
SET name = patient->field         ; JSON string "John"
SET name = patient->>field        ; Plain string John

; Works with arrays too
SET items = ["a", "b", "c"]
SET first = items.0               ; "a" as JSON
SET first = items..0              ; a as string
```

### Path Navigation

| Operator | Description                      | Example                      | Result            |
|----------|----------------------------------|------------------------------|-------------------|
| `#>`     | Get value at path (returns JSON) | `data #> ["addr", "city"]`   | `"NYC"` (as JSON) |
| `#>>`    | Get value at path (returns text) | `data #>> ["addr", "city"]`  | `NYC` (as string) |
| `@`      | Path expression                  | `data@.addr.city`            | `"NYC"`           |

#### Usage Examples
```rumps
SET record = {
  "patient": {
    "name": "John",
    "addresses": [
      { "type": "home", "city": "NYC" },
      { "type": "work", "city": "Boston" }
    ]
  }
}

; Path array notation
SET city = record #> ["patient", "addresses", 0, "city"]   ; "NYC" as JSON
SET city = record #>> ["patient", "addresses", 0, "city"]  ; NYC as string

; Path expression notation (cleaner for literals)
SET city = record@.patient.addresses[0].city               ; "NYC"

; Wildcard paths (future)
SET cities = record@.patient.addresses[*].city             ; ["NYC", "Boston"]
```

### Containment Operators

| Operator | Description  | Example                     | Result |
|----------|--------------|-----------------------------|--------|
| `@>`     | Contains     | `{"a":1, "b":2} @> {"a":1}` | `true` |
| `<@`     | Contained by | `{"a":1} <@ {"a":1, "b":2}` | `true` |

#### Usage Examples
```rumps
SET full = { "name": "John", "age": 42, "active": true }
SET partial = { "name": "John" }

; Check if full contains partial
IF full @> partial {
  OUTPUT "Match found"
}

; Check if partial is contained by full
IF partial <@ full {
  OUTPUT "Is subset"
}

; Works with arrays
SET arr = [1, 2, 3, 4, 5]
IF arr @> [2, 3] {
  OUTPUT "Contains 2 and 3"
}
```

### Key/Element Existence

| Operator | Description    | Example                      | Result |
|----------|----------------|------------------------------|--------|
| `?`      | Key exists     | `data ? "name"`              | `true` |
| `?\|`    | Any key exists | `data ?\| ["name", "alias"]` | `true` |
| `?&`     | All keys exist | `data ?& ["name", "age"]`    | `true` |

#### Usage Examples
```rumps
SET data = { "name": "John", "age": 42 }

; Single key existence
IF data ? "name" {
  OUTPUT "Has name"
}

; Any of multiple keys
IF data ?| ["email", "phone", "name"] {
  OUTPUT "Has at least one contact method"
}

; All keys required
IF data ?& ["name", "age", "active"] {
  OUTPUT "Record is complete"
} ELSE {
  OUTPUT "Missing required fields"
}

; Works with arrays (checks index exists)
SET arr = ["a", "b", "c"]
IF arr ? 0 {
  OUTPUT "Has first element"
}
```

### Modification Operators

| Operator | Description       | Example                   | Result               |
|----------|-------------------|---------------------------|----------------------|
| `\|\|`   | Concatenate/merge | `{"a":1} \|\| {"b":2}`    | `{"a":1, "b":2}`     |
| `-`      | Delete key        | `{"a":1, "b":2} - "a"`    | `{"b":2}`            |
| `#-`     | Delete at path    | `data #- ["addr", "zip"]` | (removes nested key) |

#### Usage Examples
```rumps
; Merge objects (right overwrites left on conflict)
SET base = { "name": "John", "role": "user" }
SET update = { "role": "admin", "active": true }
SET merged = base || update
; Result: { "name": "John", "role": "admin", "active": true }

; Delete single key
SET data = { "name": "John", "temp": "delete me" }
SET clean = data - "temp"
; Result: { "name": "John" }

; Delete multiple keys
SET clean = data - ["temp", "internal"]

; Delete at nested path
SET record = { "user": { "name": "John", "password": "secret" } }
SET safe = record #- ["user", "password"]
; Result: { "user": { "name": "John" } }

; Array operations
SET arr = ["a", "b", "c"]
SET shorter = arr - 1              ; Remove by index: ["a", "c"]
SET shorter = arr - "b"            ; Remove by value: ["a", "c"]
```

### JSON Namespace

The `Json` namespace provides functions for JSON manipulation:

| Function                   | Description            | Example                          |
|----------------------------|------------------------|----------------------------------|
| `Json.type(val)`           | Get JSON type          | `Json.type(42)` → `Json.Number`  |
| `Json.keys(obj)`           | Get object keys        | `Json.keys({"a":1})` → `["a"]`   |
| `Json.values(obj)`         | Get object values      | `Json.values({"a":1})` → `[1]`   |
| `Json.length(val)`         | Get length             | `Json.length([1,2,3])` → `3`     |
| `Json.parse(str)`          | Parse JSON string      | `Json.parse("{\"a\":1}")`        |
| `Json.stringify(val)`      | Convert to JSON string | `Json.stringify({"a":1})`        |
| `Json.set(obj, path, val)` | Set value at path      | `Json.set(data, ["a"], 1)`       |
| `Json.merge-deep(a, b)`    | Deep merge objects     | `Json.merge-deep(base, overlay)` |
| `Json.flatten(obj)`        | Flatten nested object  | `Json.flatten({"a":{"b":1}})`    |
| `Json.unflatten(obj)`      | Unflatten object       | `Json.unflatten({"a.b":1})`      |

#### Usage Examples
```rumps
SET data = { "users": [{"name": "John"}, {"name": "Jane"}] }

; Type checking
IF Json.type(data.users) == Json.Array {
  OUTPUT "Users is an array"
}

; Get keys and values
SET keys = Json.keys(data)                    ; ["users"]
SET user-list = Json.values(data.users.0)     ; ["John"]

; Length operations
SET count = Json.length(data.users)           ; 2

; Parse and stringify
SET json-str = "{\"temp\": 72}"
SET parsed = Json.parse(json-str)
SET back = Json.stringify(parsed)

; Immutable update at path
SET updated = Json.set(data, ["users", 0, "age"], 42)
; Result: { "users": [{"name": "John", "age": 42}, {"name": "Jane"}] }

; Deep merge (recursive)
SET base = { "config": { "a": 1, "b": 2 } }
SET overlay = { "config": { "b": 3, "c": 4 } }
SET merged = Json.merge-deep(base, overlay)
; Result: { "config": { "a": 1, "b": 3, "c": 4 } }
```

### JSON in Stream Operations

JSON operators integrate naturally with `COLLECT` streams:

```rumps
; Filter by JSON field
COLLECT ^PATIENTS
  WHERE value ? "active"
  WHERE value..active == true
  SELECT {
    id: key[0],
    name: value..name,
    email: value..contact..email ?? "N/A"
  }
  OUTPUT AS JSON

; Aggregate JSON data
COLLECT ^ORDERS
  WHERE value @> {"status": "completed"}
  SELECT value..amount
  AGGREGATE SUM INTO total-revenue

; Transform nested structures
COLLECT ^RECORDS
  SELECT value #>> ["metadata", "tags"]
  FILTER value != Option.None
  INTO tag-list
```

### Operator Precedence

JSON operators have the following precedence (highest to lowest):

1. `.` `..` (field access)
2. `->` `->>` (arrow access)
3. `#>` `#>>` (path access)
4. `?` `?|` `?&` (existence)
5. `@>` `<@` (containment)
6. `-` `#-` (deletion)
7. `||` (concatenation)

Use parentheses to override precedence when needed:
```rumps
SET result = (data || defaults)..name    ; Merge first, then access
SET result = data || (defaults..name)    ; Access first, then merge (different!)
```

## Control Flow

### IF Expression

`IF` is an **expression** that evaluates to a value, not just a statement. This enables functional-style conditional logic where the result of a branch can be assigned or used directly.

#### Basic Syntax

```rumps
IF <condition> { <then-expr> } ELSE { <else-expr> }
```

Both branches are block expressions. The `ELSE` clause is optional.

#### As an Expression

```rumps
; Assign result of IF directly
LET status = IF active { "enabled" } ELSE { "disabled" }

; Use in function return
FUN abs (n) {
  IF n < 0 { -n } ELSE { n }
}

; Nested IF expressions
LET grade = IF score >= 90 { "A" }
            ELSE IF score >= 80 { "B" }
            ELSE IF score >= 70 { "C" }
            ELSE { "F" }
```

#### Without ELSE

When `IF` has no `ELSE` clause and the condition is false, it evaluates to `Option.None`:

```rumps
LET result = IF condition { value }
; result is Option.None if condition is false

; Useful with null coalescing
LET name = IF has-name { get-name() } ?? "Unknown"
```

#### As a Statement

When used as a statement (for side effects), the value is discarded:

```rumps
IF count > 0 {
  OUTPUT "Processing..."
  process-items()
}
```

### Block Expressions

Braces `{ }` create a **block expression**. A block executes statements for side effects, then evaluates to its trailing expression (the last expression in the block).

```rumps
; Block with trailing expression
LET result = {
  LET x = compute()
  LET y = transform(x)
  x + y    ; This is the value of the block
}

; Block without trailing expression evaluates to Option.None
{
  OUTPUT "Side effect only"
}
```

Variables bound with `LET` inside a block are scoped to that block:

```rumps
LET x = 1
LET result = {
  LET x = 10   ; shadows outer x
  x + 1        ; 11
}
OUTPUT x       ; 1 (outer x unchanged)
OUTPUT result  ; 11
```

## Functions

Functions are named, reusable blocks of code. They can accept arguments, perform computations, and yield a result.

### Basic Syntax

```rumps
FUN <name> (<args>) {
  <body>
}
```

### Simple Functions

```rumps
FUN greet (name) {
  OUTPUT "Hello, " ++ name ++ "!"
}

FUN add (a, b) {
  a + b
}

FUN square (x) {
  x * x
}
```

The last expression in a function body is its result (no explicit `RETURN`).

### Calling Functions

```rumps
; Direct call
greet("World")

; Capture result
SET sum = add(10, 20)

; In expressions
SET area = square(side) * 4

; With COLLECT (implicit key/value)
COLLECT ^NUMBERS
  SELECT square(value)
  OUTPUT

; With general collections
[1, 2, 3, 4] |> MAP square
```

### Multi-Statement Functions

Use `;` or newlines to separate statements. The final expression is the result:

```rumps
FUN process-patient (id) {
  SET name = GET(^PATIENT(id, "NAME"))
  SET age = GET(^PATIENT(id, "AGE"))
  SET visits = COLLECT ^VISITS
    WHERE key[0] == id
    COUNT

  {
    name: name,
    age: age,
    visit-count: visits
  }
}
```

### Functions with Side Effects

Functions that perform side effects but don't need to yield a value:

```rumps
FUN log-access (user, resource) {
  TRANSACTION {
    SET ^AUDIT(Time.now(), user) = resource
  }
}

FUN notify-all (msg) {
  COLLECT ^USERS
    WHERE value..active == true
    EXECUTE {
      send-notification(value..id, msg)
    }
}
```

### Functions in Stream Operations

Functions integrate naturally with `COLLECT` streams:

```rumps
FUN is-adult (age) {
  age >= 18
}

FUN format-name (last, first) {
  last ++ ", " ++ first
}

; Use in COLLECT with implicit key/value
COLLECT ^PERSONS
  WHERE is-adult(value..age)
  SELECT format-name(value..last, value..first)
  OUTPUT
```

### Recursive Functions

```rumps
FUN factorial (n) {
  IF n <= 1 { 1 }
  ELSE { n * factorial(n - 1) }
}

FUN tree-sum (node-key) {
  SET val = GET(^TREE(node-key, "VALUE")) ?? 0
  SET children-sum = COLLECT ^TREE(node-key, "CHILDREN")
    SELECT tree-sum(key[0])
    AGGREGATE SUM

  val + children-sum
}
```

### Closures / Anonymous Functions

Closures use arrow syntax and are used with general collection operations:

```rumps
; Arrow syntax with general collections (pipeline form, no parens needed)
[1, 2, 3] |> MAP x => x * 2
[1, 2, 3, 4, 5] |> FILTER x => x > 2

; Prefix form requires parens to delimit closure from collection arg
MAP (x => x * 2) [1, 2, 3]
```

More complex expressions use braces in the closure body:

```rumps
[1, 2, 3] |> MAP x => {
  LET doubled = x * 2
  LET squared = doubled * doubled
  squared
}
```

**Note**: Within `COLLECT` blocks, all operations (`WHERE`, `SELECT`, `EXECUTE`, etc.) have implicit access to bindings based on element type. General collection operations (`MAP`, `FILTER`, `FOREACH`) use explicit closures.

## Namespaces

RUMPS uses **PascalCase namespaces** with dot notation for organizing functions and types.

### Defining Namespaces

```rumps
NAMESPACE MyUtils {
  FUN double (x: Int) -> Int {
    x * 2
  }

  FUN triple (x: Int) -> Int {
    x * 3
  }
}

; Usage
SET result = MyUtils.double(21)  ; 42
```

### Standard Library Namespaces

#### `String` — String operations

| Function                      | Description        | Example                                     |
|-------------------------------|--------------------|---------------------------------------------|
| `String.length(s)`            | Get length         | `String.length("hello")` → `5`              |
| `String.upper(s)`             | Uppercase          | `String.upper("hi")` → `"HI"`               |
| `String.lower(s)`             | Lowercase          | `String.lower("HI")` → `"hi"`               |
| `String.trim(s)`              | Trim whitespace    | `String.trim("  x  ")` → `"x"`              |
| `String.split(s, d)`          | Split by delimiter | `String.split("a,b", ",")` → `["a", "b"]`   |
| `String.join(arr, d)`         | Join with delim    | `String.join(["a", "b"], ",")` → `"a,b"`    |
| `String.slice(s, i, j)`       | Substring          | `String.slice("hello", 1, 3)` → `"el"`      |
| `String.contains(s, sub)`     | Check substring    | `String.contains("hello", "ell")` → `true`  |
| `String.replace(s, old, new)` | Replace occurs     | `String.replace("foo", "o", "a")` → `"faa"` |

#### `Array` — Array operations

| Function                 | Description      | Example                                  |
|--------------------------|------------------|------------------------------------------|
| `Array.length(arr)`      | Get length       | `Array.length([1,2,3])` → `3`            |
| `Array.push(arr, val)`   | Append element   | `Array.push([1,2], 3)` → `[1,2,3]`       |
| `Array.pop(arr)`         | Remove last      | `Array.pop([1,2,3])` → `[1,2]`           |
| `Array.head(arr)`        | First element    | `Array.head([1,2,3])` → `1`              |
| `Array.tail(arr)`        | All but first    | `Array.tail([1,2,3])` → `[2,3]`          |
| `Array.reverse(arr)`     | Reverse order    | `Array.reverse([1,2,3])` → `[3,2,1]`     |
| `Array.sort(arr)`        | Sort ascending   | `Array.sort([3,1,2])` → `[1,2,3]`        |
| `Array.concat(a, b)`     | Concatenate      | `Array.concat([1], [2])` → `[1,2]`       |
| `Array.slice(arr, i, j)` | Subarray         | `Array.slice([1,2,3,4], 1, 3)` → `[2,3]` |
| `Array.contains(arr, v)` | Check membership | `Array.contains([1,2,3], 2)` → `true`    |

#### `Map` — Map operations

| Function           | Description      | Example                                  |
|--------------------|------------------|------------------------------------------|
| `Map.keys(m)`      | Get all keys     | `Map.keys({a: 1})` → `["a"]`             |
| `Map.values(m)`    | Get all values   | `Map.values({a: 1})` → `[1]`             |
| `Map.has(m, k)`    | Check key exists | `Map.has({a: 1}, "a")` → `true`          |
| `Map.get(m, k)`    | Get value        | `Map.get({a: 1}, "a")` → `Some(1)`       |
| `Map.set(m, k, v)` | Set key-value    | `Map.set({}, "a", 1)` → `{a: 1}`         |
| `Map.remove(m, k)` | Remove key       | `Map.remove({a: 1}, "a")` → `{}`         |
| `Map.merge(a, b)`  | Merge maps       | `Map.merge({a: 1}, {b: 2})` → `{a:1,b:2}`|

#### `Math` — Mathematical operations

| Function        | Description    | Example                    |
|-----------------|----------------|----------------------------|
| `Math.abs(x)`   | Absolute value | `Math.abs(-5)` → `5`       |
| `Math.min(a,b)` | Minimum        | `Math.min(3, 7)` → `3`     |
| `Math.max(a,b)` | Maximum        | `Math.max(3, 7)` → `7`     |
| `Math.floor(x)` | Floor          | `Math.floor(3.7)` → `3`    |
| `Math.ceil(x)`  | Ceiling        | `Math.ceil(3.2)` → `4`     |
| `Math.round(x)` | Round          | `Math.round(3.5)` → `4`    |
| `Math.sqrt(x)`  | Square root    | `Math.sqrt(16)` → `4.0`    |
| `Math.pow(x,y)` | Power          | `Math.pow(2, 3)` → `8`     |
| `Math.log(x)`   | Natural log    | `Math.log(2.718)` → `~1.0` |
| `Math.sin(x)`   | Sine           | `Math.sin(0)` → `0.0`      |
| `Math.cos(x)`   | Cosine         | `Math.cos(0)` → `1.0`      |
| `Math.random()` | Random 0-1     | `Math.random()` → `0.xxx`  |

#### `Option` — Option operations

| Function                 | Description          | Example                                   |
|--------------------------|----------------------|-------------------------------------------|
| `Option.Some(v)`         | Wrap value           | `Option.Some(42)` → `Some(42)`            |
| `Option.None`            | Empty option         | `Option.None` → `None`                    |
| `Option.is-some(o)`      | Check if Some        | `Option.is-some(Some(1))` → `true`        |
| `Option.is-none(o)`      | Check if None        | `Option.is-none(None)` → `true`           |
| `Option.unwrap(o)`       | Get value or panic   | `Option.unwrap(Some(1))` → `1`            |
| `Option.unwrap-or(o, d)` | Get value or default | `Option.unwrap-or(None, 0)` → `0`         |
| `Option.map(o, f)`       | Transform if Some    | `Option.map(Some(1), double)` → `Some(2)` |

#### `Result` — Result operations

| Function                 | Description          | Example                                   |
|--------------------------|----------------------|-------------------------------------------|
| `Result.Ok(v)`           | Success value        | `Result.Ok(42)` → `Ok(42)`                |
| `Result.Err(e)`          | Error value          | `Result.Err("fail")` → `Err("fail")`      |
| `Result.is-ok(r)`        | Check if Ok          | `Result.is-ok(Ok(1))` → `true`            |
| `Result.is-err(r)`       | Check if Err         | `Result.is-err(Err("x"))` → `true`        |
| `Result.unwrap(r)`       | Get value or panic   | `Result.unwrap(Ok(1))` → `1`              |
| `Result.unwrap-or(r, d)` | Get value or default | `Result.unwrap-or(Err("x"), 0)` → `0`     |
| `Result.map(r, f)`       | Transform if Ok      | `Result.map(Ok(1), double)` → `Ok(2)`     |
| `Result.map-err(r, f)`   | Transform if Err     | `Result.map-err(Err("x"), upper)` → `...` |

#### `Io` — Input/Output (Future)

| Function                 | Description                                   |
|--------------------------|-----------------------------------------------|
| `Io.read-file(path)`     | Read file contents                            |
| `Io.write-file(path, s)` | Write to file                                 |
| `Io.stdin()`             | Read from stdin                               |
| `Io.print(s)`            | Print to stdout  (alias for `OUTPUT`)         |
| `Io.eprint(s)`           | Print to stderr (alias for `OUTPUT TO ERROR`) |
| `Io.env(x)`              | Try to get `x` as env var                     |
|                          |                                               |

### Namespace Imports

```rumps
; Import specific functions
IMPORT String.{length, upper, lower}

; Use without prefix
SET len = length("hello")

; Import entire namespace with alias
IMPORT Math AS M

SET x = M.sqrt(16)

; Import everything (use sparingly)
IMPORT Array.*

SET arr = reverse([1, 2, 3])
```

## Types

RUMPS uses **PascalCase** for type names. Types are reserved keywords.

RUMPS uses **square brackets** for type parameters: `Array[Int]`, `Map[String, Int]`.

### Native Types

Native types are the core RUMPS types with full type tracking.

#### Primitives

| Type     | Description           | Examples                        |
|----------|-----------------------|---------------------------------|
| `Option` | Nullable value        | `Option.Some(_)`, `Option.None` |
| `Bool`   | Boolean               | `true`, `false`                 |
| `Int`    | Integer               | `42`, `-7`, `0`                 |
| `Float`  | Floating-point number | `3.14`, `-0.5`, `1e10`          |
| `String` | Text string           | `"hello"`, `""`                 |

#### Collections

| Type         | Description                    | Examples                      |
|--------------|--------------------------------|-------------------------------|
| `Array[T]`   | Typed array                    | `Array[Int]`, `Array[String]` |
| `Map[K, V]`  | Key-value map                  | `Map[String, Int]`            |
| `Set[T]`     | Unique value set               | `Set[String]`                 |
| `Tuple[...]` | Fixed heterogeneous collection | `Tuple[Int, String, Bool]`    |

#### Wrappers

| Type           | Description             | Examples              |
|----------------|-------------------------|-----------------------|
| `Option[T]`    | Nullable/optional value | `Option[String]`      |
| `Result[T, E]` | Success or error        | `Result[Int, String]` |

#### Special

| Type         | Description                 | Examples         |
|--------------|-----------------------------|------------------|
| `Stream[T]`  | Lazy stream of `T`          | `Stream[Key]`    |
| `Proc[A, R]` | Procedure `A -> R`          | `Proc[Int, Int]` |
| `Key`        | Subscript key               | `Key`            |
| `Global`     | Global variable reference   | `Global`         |
| `Local`      | Local variable reference    | `Local`          |
| `Type`       | Type value (reflection)     | `Type`           |

#### Abstract

| Type     | Description                |
|----------|----------------------------|
| `Number` | `Int` or `Float`           |
| `Any`    | Any native type            |
| `Never`  | Bottom type (no values)    |

### JSON Types

JSON types represent dynamic data from parsing or external sources. They mirror JSON's structure but are distinct from native types.

| Type          | Description                            | Native Equivalent   |
|---------------|----------------------------------------|---------------------|
| `Json`        | Any JSON value                         | `Any`               |
| `Json.Null`   | JSON null                              | `Option.None`       |
| `Json.Bool`   | JSON boolean                           | `Bool`              |
| `Json.Number` | JSON number (no int/float distinction) | `Number`            |
| `Json.String` | JSON string                            | `String`            |
| `Json.Array`  | JSON array (heterogeneous)             | `Array[Json]`       |
| `Json.Object` | JSON object (string keys)              | `Map[String, Json]` |

### Native vs JSON: Literal Syntax

RUMPS distinguishes native types from JSON types **syntactically** in literals:

#### Records vs JSON Objects

- **Unquoted keys** → native record (structural type)
- **Quoted keys** → JSON object

```rumps
; Native record: typed, efficient, dot access
SET patient = { id: 123, name: "John", active: true }
OUTPUT patient.name       ; "John"

; JSON object: dynamic, for external data
SET json = { "id": 123, "name": "John", "active": true }
OUTPUT json..name         ; John (text extraction with ..)
```

#### Arrays

Array literals are **native by default**. JSON arrays require explicit annotation or coercion.

- **Homogeneous** → native `Array[T]` unless annotated/coerced
- **Heterogeneous** → `Json.Array` (only valid interpretation)
- **Explicit `as Json` or `: Json.Array`** → JSON regardless of contents

```rumps
; Native arrays (default for homogeneous)
[1, 2, 3]                        ; Array[Int]
["a", "b", "c"]                  ; Array[String]

; JSON arrays (explicit)
[1, 2, 3] as Json                ; Json.Array
SET x: Json.Array = [1, 2, 3]    ; Json.Array via annotation
SET x: Json = [1, 2, 3]          ; Json (which is a Json.Array)

; Heterogeneous: MUST be Json.Array (no other valid interpretation)
[1, "hello", true]               ; Json.Array (inferred)
```

#### Conversion and Coercion

```rumps
; Native types from RUMPS operations
SET keys: Array[Key] = COLLECT ^DATA SELECT key INTO
SET counts: Map[String, Int] = compute-histogram(data)

; JSON from parsing external data
SET json: Json = Json.parse(input-str)
SET arr: Json.Array = Json.parse("[1, 2, 3]")

; Convert JSON to native (validated at runtime)
SET nums: Array[Int] = arr as Array[Int]

; Convert native to JSON
SET out: Json = patient as Json
```

#### Key Differences Summary

| Aspect         | Native                    | JSON                                |
|----------------|---------------------------|-------------------------------------|
| Object keys    | Unquoted: `{ name: "x" }` | Quoted: `{ "name": "x" }`           |
| Array elements | Homogeneous, typed        | Heterogeneous allowed               |
| Field access   | `.field`                  | `..field` (text) or `.field` (JSON) |
| Type guarantee | Compile-time structure    | Runtime dynamic                     |
| Use case       | Internal data, typed APIs | External data, parsing/serializing  |

### Type Annotations

Type annotations are **optional hints** that the interpreter validates at runtime. RUMPS is an interpreted query language, not a compiled programming language—there is no static type checking. Annotations serve as:

1. **Documentation** for procedure signatures
2. **Runtime guards** that produce interpreter errors if violated
3. **Self-describing contracts** for API boundaries

```rumps
; Untyped (no runtime validation)
FUN add (a, b) {
  a + b
}

; Typed arguments (runtime error if wrong type passed)
FUN add (a: Int, b: Int) {
  a + b
}

; Typed arguments and return (validates both input and output)
FUN add (a: Int, b: Int) -> Int {
  a + b
}

; Complex types
FUN process (data: Json.Object) -> Json.Array {
  Json.values(data)
}
```

When a type annotation is violated, the interpreter raises a runtime error with a clear message indicating the expected vs actual type.

### Type Checking

Runtime type checking with `is`:

```rumps
IF value is Type.String {
  OUTPUT "It's a string: " + value
} ELSE IF value is Type.Number {
  OUTPUT "It's a number: " + String.from(value)
} ELSE IF value is Option.None {
  OUTPUT "It's null"
}
```

Get type as a value with `Type.of`:

```rumps
SET t = Type.of(value)

IF t == Type.String { ... }
IF t == Type.Int OR t == Type.Float { ... }
```

### Type Coercion

RUMPS performs **automatic type coercion** wherever sensible, following these rules:

#### Automatic Coercions

RUMPS takes a conservative approach to coercion: values can be coerced *into* strings, but not *out of* them. This avoids JavaScript-style surprises while remaining flexible.

| Context                             | Coercion            | Example                       | Notes                    |
|-------------------------------------|---------------------|-------------------------------|--------------------------|
| Numeric operations (`+`, `-`, etc.) | Int ↔ Float         | `42 + 3.14` → `45.14`         | Promotes to Float        |
| String concatenation (`++`)         | Any → String        | `"ID: " ++ 42` → `"ID: 42"`   | Always valid             |
| Boolean context (`IF`, `AND`, `OR`) | Any → Truthy/falsy  | `IF count { ... }`            | See table below          |

**What is NOT coerced:**

| Expression     | Result        | Rationale                              |
|----------------|---------------|----------------------------------------|
| `"10" + 2`     | Runtime error | String not coerced to number           |
| `"10" > 5`     | Runtime error | Use `Int.from("10") > 5` if intended   |
| `"3.14" * 2`   | Runtime error | Explicit conversion required           |

This is a deliberate departure from traditional MUMPS (where everything was stringly-typed) in favor of catching likely bugs.

#### Truthy/Falsy Values

| Falsy                                                | Truthy          |
|------------------------------------------------------|-----------------|
| `Option.None`, `false`, `0`, `0.0`, `""`, `[]`, `{}` | Everything else |

#### Explicit Conversion

When automatic coercion isn't appropriate or for clarity, use type namespaces:

| Function           | Description      | Example                       |
|--------------------|------------------|-------------------------------|
| `Int.from(val)`    | Convert to Int   | `Int.from("42")` → `42`       |
| `Float.from(val)`  | Convert to Float | `Float.from("3.14")` → `3.14` |
| `String.from(val)` | Convert to String| `String.from(42)` → `"42"`    |
| `Bool.from(val)`   | Convert to Bool  | `Bool.from(1)` → `true`       |
| `Array.from(val)`  | Wrap in Array    | `Array.from(1)` → `[1]`       |

Or use the `as` keyword for casting:

```rumps
SET n = "42" as Int
SET s = 3.14 as String
```

**Note**: Failed coercions (e.g., `"hello" as Int`) produce runtime errors.

### Parameterized Type Examples

```rumps
; Array of specific type
FUN sum (nums: Array[Int]) -> Int {
  nums |> AGGREGATE SUM
}

; Optional/nullable return
FUN find (id: Int) -> Option[String] {
  GET(^DATA(id, "NAME"))
}

; Result type for fallible operations
FUN parse-int (s: String) -> Result[Int, String] {
  ; returns Ok[Int] or Err[String]
}

; Stream processing with known element type
FUN get-names () -> Stream[String] {
  COLLECT ^PATIENTS
    SELECT value..name
}

; Function as first-class value
FUN apply-twice (f: Proc[Int, Int], x: Int) -> Int {
  f(f(x))
}

; Map type
FUN word-count (words: Array[String]) -> Map[String, Int] {
  ; ...
}
```

### Structural Object Types (Future)

For objects with known shape:

```rumps
; Inline structural type
FUN process (patient: {name: String, age: Int}) {
  OUTPUT patient..name
}

; Type alias
TYPE Patient = {
  name: String,
  age: Int,
  active: Bool
}

FUN admit (p: Patient) {
  ; ...
}
```

**Resolved**: All type checking is **runtime only**. RUMPS is an interpreted query language—type annotations are validated when code executes, not at parse time.

### Sum Types

RUMPS supports **sum types** (tagged unions / algebraic data types) for modeling values that can be one of several variants.

#### Definition Syntax

```rumps
; Simple enumeration (no payloads)
TYPE Status =
  | Pending
  | Active
  | Completed
  | Failed

; With payloads
TYPE DataStatus =
  | NoValue
  | HasValue(value)
  | HasDescendants
  | HasBoth(value)

; Parameterized sum types
TYPE Result[T, E] =
  | Ok(T)
  | Err(E)

TYPE Option[T] =
  | Some(T)
  | None
```

#### Constructor Syntax

Constructors use `Type.Variant` notation:

```rumps
SET status = Status.Pending
SET result = Result.Ok(42)
SET data = DataStatus.HasValue("hello")
SET opt = Option.None
```

#### Built-in Sum Types

These are predefined in the standard library:

| Type           | Variants                  | Description             |
|----------------|---------------------------|-------------------------|
| `Option[T]`    | `Some(T)`, `None`         | Nullable/optional value |
| `Result[T, E]` | `Ok(T)`, `Err(E)`         | Success or error        |
| `DataStatus`   | `NoValue`, `HasValue(v)`, `HasDescendants`, `HasBoth(v)` | Node existence status |

#### Serializing Custom Types (Future)

Currently, users must manually convert custom sum types to/from records for persistence:

```rumps
TYPE Status =
  | Active
  | Discharged(date)

; Manual conversion
FUN status-to-record (s: Status) {
  MATCH s {
    Status.Active => { { tag: "Active" } }
    Status.Discharged(d) => { { tag: "Discharged", date: d } }
  }
}

FUN record-to-status (r) -> Status {
  MATCH r.tag {
    "Active" => { Status.Active }
    "Discharged" => { Status.Discharged(r.date) }
    _ => { THROW "Unknown tag: " ++ r.tag }
  }
}
```

This is explicit and requires no new language features. Future iterations may add:
- Derive-style: `TYPE Status DERIVE Serialize = | ...`
- Associated functions: `TYPE Status WITH { to-record: ..., from-record: ... }`
- Convention-based: interpreter auto-discovers `TypeName.to-record` / `TypeName.from-record`

For now, manual `MATCH`-based conversion is the idiomatic approach.

### Pattern Matching

The `MATCH` expression enables exhaustive, type-safe branching on values. It is the primary way to destructure sum types.

#### Basic Syntax

```rumps
MATCH <scrutinee> {
  <pattern> => <expr>
  <pattern> => <expr>
  ...
}
```

#### Matching Sum Types

```rumps
SET status = get-data-status(^PATIENT(id))

MATCH status {
  DataStatus.NoValue => {
    OUTPUT "No data at this key"
  }
  DataStatus.HasValue(v) => {
    OUTPUT "Value: " ++ v
  }
  DataStatus.HasDescendants => {
    OUTPUT "Has children but no value"
  }
  DataStatus.HasBoth(v) => {
    OUTPUT "Value: " ++ v ++ " (and has children)"
  }
}
```

#### Matching Option and Result

```rumps
SET name = GET(^PATIENT(id, "NAME"))

MATCH name {
  Option.Some(n) => { OUTPUT "Patient: " ++ n }
  Option.None => { OUTPUT "Unknown patient" }
}

SET result = try-parse-int(input)

MATCH result {
  Result.Ok(n) => { n * 2 }
  Result.Err(e) => {
    OUTPUT TO ERROR "Parse failed: " ++ e
    0  ; default value
  }
}
```

#### Matching Literals and Wildcards

```rumps
; Match on integers
MATCH count {
  0 => { "none" }
  1 => { "one" }
  n => { "many: " ++ n }  ; bind to variable
}

; Match on booleans
MATCH active {
  true => { "enabled" }
  false => { "disabled" }
}

; Match on strings
MATCH cmd {
  "quit" => { exit() }
  "help" => { show-help() }
  _ => { OUTPUT "Unknown command" }  ; wildcard
}
```

#### Pattern Guards

Use `IF` to add conditions to patterns:

```rumps
MATCH n {
  x IF x > 100 => { "large" }
  x IF x > 0 => { "positive" }
  0 => { "zero" }
  _ => { "negative" }
}

MATCH user {
  Option.Some(u) IF u.admin => { "Admin: " ++ u.name }
  Option.Some(u) => { "User: " ++ u.name }
  Option.None => { "Anonymous" }
}
```

#### Nested Patterns

```rumps
MATCH nested-opt {
  Option.Some(Option.Some(v)) => { "doubly wrapped: " ++ v }
  Option.Some(Option.None) => { "outer Some, inner None" }
  Option.None => { "outer None" }
}

MATCH result {
  Result.Ok(Option.Some(v)) => { use(v) }
  Result.Ok(Option.None) => { OUTPUT "Success but empty" }
  Result.Err(e) => { OUTPUT "Error: " ++ e }
}
```

#### Tuple and Array Patterns

```rumps
; Tuple destructuring
SET pair = (1, "hello")
MATCH pair {
  (0, s) => { "zero with " ++ s }
  (n, "hello") => { "greeting from " ++ n }
  (n, s) => { "other: " ++ n ++ ", " ++ s }
}

; Array head/tail (future)
MATCH items {
  [] => { "empty" }
  [x] => { "single: " ++ x }
  [x, y] => { "pair: " ++ x ++ ", " ++ y }
  [h, ...tail] => { "head: " ++ h ++ ", rest has " ++ Array.length(tail) }
}
```

#### Exhaustiveness

`MATCH` expressions should be **exhaustive**; the interpreter will warn (or error in strict mode) if patterns don't cover all cases:

```rumps
; WARNING: Non-exhaustive match
MATCH opt {
  Option.Some(v) => { v }
  ; Missing: Option.None
}

; Fix with wildcard or explicit case
MATCH opt {
  Option.Some(v) => { v }
  _ => { "default" }
}
```

#### Match as Expression

`MATCH` is an expression and yields a value:

```rumps
SET label = MATCH status {
  Status.Pending => { "waiting" }
  Status.Active => { "in progress" }
  Status.Completed => { "done" }
  Status.Failed => { "error" }
}

; In COLLECT with MATCH
COLLECT ^DATA
  SELECT MATCH value.status {
    DataStatus.HasValue(v) => { v }
    _ => { "N/A" }
  }
  OUTPUT
```

## Error Handling

RUMPS provides structured error handling through `CATCH` and `HANDLE` constructs. By default, when an operation produces a `Result.Err`, the interpreter raises a `RuntimeError` and halts execution. These constructs allow graceful recovery.

### CATCH: Handling Errors

`CATCH` intercepts errors from any expression or block, receiving only the error value:

```rumps
; On a single expression
risky-operation() CATCH e => {
  OUTPUT TO ERROR "Operation failed: " ++ e.message
  fallback-value
}

; On a block
{
  SET data = fetch-remote(url)
  process(data)
} CATCH e => {
  OUTPUT TO ERROR "Pipeline failed: " ++ e
  Option.None
}
```

The `CATCH` block receives an error object with at least:
- `e.message`: Human-readable error description
- `e.kind`: Error category (e.g., `"IoError"`, `"ParseError"`, `"TxConflict"`)
- `e.source`: Optional underlying cause

### HANDLE: Receiving the Full Result

`HANDLE` gives you the complete `Result`, allowing you to act on both success and failure:

```rumps
; Explicit Result handling
db-operation() HANDLE r => {
  MATCH r {
    Result.Ok(v) => {
      OUTPUT "Success: " ++ v
      v
    }
    Result.Err(e) => {
      log-error(e)
      default-value
    }
  }
}

; Shorthand with pattern match
db-operation() HANDLE {
  Result.Ok(v) => { process(v) }
  Result.Err(e) => { recover(e) }
}
```

The shorthand form (without `r =>`) directly pattern matches the result.

### Transaction Error Handling

Transactions use `CATCH` for error handling, combined with `ON CONFLICT` for conflict-specific strategies:

```rumps
TRANSACTION {
  SET ^ACCOUNT(from, "BALANCE") = ^ACCOUNT(from, "BALANCE") - amount
  SET ^ACCOUNT(to, "BALANCE") = ^ACCOUNT(to, "BALANCE") + amount
} ON CONFLICT RETRY 3
  CATCH e => {
    log-error("Transfer failed", e)
    notify-admin(e)
  }
  FINALLY {
    cleanup-locks()
  }
```

- `ON CONFLICT`: Handles transaction conflicts specifically (see Transaction section)
- `CATCH`: Handles any error after conflict resolution exhausted
- `FINALLY`: Always runs, regardless of success or failure

### Error Propagation in Streams

Within `COLLECT` streams, errors can be handled per-element or for the entire stream:

```rumps
; Per-element error handling
COLLECT ^RECORDS
  MAP record => {
    parse-record(record) CATCH e => {
      { error: e.message, original: record }
    }
  }
  INTO results

; Skip errors silently
COLLECT ^RECORDS
  MAP record => parse-record(record) CATCH _ => { Option.None }
  FILTER Option.is-some
  MAP Option.unwrap
  INTO valid-records

; Fail-fast (default behavior without CATCH)
COLLECT ^RECORDS
  MAP parse-record  ; First error halts the stream
  INTO results

; Collect errors separately
COLLECT ^RECORDS
  MAP record => parse-record(record) HANDLE {
    Result.Ok(v) => { Result.Ok(v) }
    Result.Err(e) => { Result.Err({ key: record.key, error: e }) }
  }
  PARTITION Result.is-ok INTO (successes, failures)
```

### TRY Blocks

For grouping multiple operations under unified error handling:

```rumps
TRY {
  SET config = load-config(path)
  SET conn = connect-db(config.db-url)
  SET data = query(conn, sql)
  process(data)
} CATCH e => {
  OUTPUT TO ERROR "Startup failed: " ++ e.message
  exit(1)
} FINALLY {
  close-conn(conn) CATCH _ => { }  ; Ignore cleanup errors
}
```

### Creating Errors

Functions can signal errors using `THROW` or by returning `Result.Err`:

```rumps
FUN divide (a: Int, b: Int) -> Result[Int, String] {
  MATCH b {
    0 => { Result.Err("Division by zero") }
    _ => { Result.Ok(a / b) }
  }
}

FUN require-positive (n: Int) -> Int {
  MATCH n > 0 {
    true => { n }
    false => { THROW "Expected positive number, got: " ++ n }
  }
}
```

`THROW` immediately raises a `RuntimeError`; callers must use `CATCH` or `HANDLE` to recover.

### Error Types

```rumps
; Built-in error structure
TYPE Error = {
  message: String,
  kind: String,
  source: Option[Error]
}

; Common error kinds
; - "RuntimeError": General interpreter error
; - "TypeError": Type mismatch at runtime
; - "IoError": File/network operation failed
; - "ParseError": Failed to parse input
; - "TxConflict": Transaction conflict
; - "TxAborted": Transaction aborted
; - "KeyNotFound": Global/local key doesn't exist
```

## Implementation Phases

### Phase 1: Core Stream Operations (Storage Layer)
- [ ] Implement `COLLECT` primitive in Rust (see `persistence.md` Phase 2.7)
- [ ] Basic filtering: WHERE, FILTER
- [ ] Basic transformation: SELECT
- [ ] Basic limiting: TAKE, SKIP
- [ ] Terminal operations: INTO, basic OUTPUT

### Phase 2: Advanced Operations
- [ ] Aggregation: COUNT, SUM, AVG, MIN, MAX
- [ ] Grouping: GROUP BY
- [ ] Sorting: SORT BY
- [ ] Joins: JOIN operation

### Phase 3: Parser and Interpreter
- [ ] Design formal grammar (EBNF)
- [ ] Implement two-phase lexer → parser using `chumsky` (see Parser Architecture section)
- [ ] Build AST representation
- [ ] Implement interpreter that calls Rust storage layer
- [ ] Sum type definitions (`TYPE ... = | Variant ...`)
- [ ] Pattern matching (`MATCH` expressions)
- [ ] Error handling (`CATCH`, `HANDLE`, `TRY`/`FINALLY`, `THROW`)

### Phase 4: Output Formatting
- [ ] Needs to support stdout/stderr writes
- [ ] Template string interpolation
- [ ] JSON output formatter
- [ ] Table output formatter
- [ ] CSV output formatter
- [ ] Custom separators
- [ ] Some other ideas:
  - To file
  - To remote (e.g. `OUTPUT TO HTTP "https://metrics.example.com/api"`)

### Phase 5: Parallel and Async
- [ ] PARALLEL operation implementation
- [ ] Async stream processing
- [ ] Backpressure handling
- [ ] Error recovery strategies

### Phase 6: Advanced Features
- [ ] Stream composition and reuse
- [ ] Custom operators
- [ ] Stream debugging tools
- [ ] Performance profiling
- [ ] Query optimization

## Open Design Questions

1. ~~**Syntax Style**: Should we support both block form and pipeline form, or standardize on one?~~ **Resolved**: Block form only for `COLLECT`. Pipeline operator (`|>`) retained for general collection operations (`MAP`, `FILTER`, `FOREACH`, `REDUCE`). The interpreter tracks **element type** through the pipeline: pre-SELECT operations use `key`/`value`; post-SELECT operations use `item` (scalars) or field names (objects). All COLLECT operations (including `EXECUTE`) use implicit bindings. See [Element Type Tracking](#element-type-tracking).
2. ~~**Type System**: How much type inference vs explicit typing?~~ **Resolved**: No static type checking. Type annotations are optional runtime hints—validated when executed, producing interpreter errors if violated. Conservative automatic coercion (into strings, between numerics, but not from strings to numbers). A future "strict mode" may add optional parse-time validation for annotated procedures.
3. ~~**Error Handling**: How to handle errors in stream processing?~~ **Resolved**: `CATCH e => { }` intercepts errors, `HANDLE { Ok(v) => ..., Err(e) => ... }` gives full `Result`. `TRY { } CATCH { } FINALLY { }` for grouped operations. Streams can handle errors per-element or fail-fast. See Error Handling section.
4. **Transaction Integration**: How do streams interact with transaction boundaries?
5. **Performance Hints**: Should we allow manual optimization hints?
6. **Debugging**: What debugging/profiling features should be built-in?
7. **Interop**: How to call Rust functions from DSL and vice versa?
8. **Standard Library**: What built-in functions should be provided?

## Immediate TODOs (Post-Storage Engine)

These tasks should be completed after the storage engine implementation is finished:

### Language Design
- [x] **Resolve syntax style decision**: Block form only for `COLLECT`; pipeline operator for general collections. Interpreter tracks element type: pre-SELECT uses `key`/`value`; post-SELECT uses `item` or field names. All COLLECT operations use implicit bindings. See [Element Type Tracking](#element-type-tracking).
- [ ] **Define operator precedence**: Establish clear precedence rules for all operations
- [x] **Specify type coercion rules**: Conservative coercion — into strings and between numerics, but NOT from strings to numbers (see Type Coercion section)
- [x] **Design error handling semantics**: `CATCH`, `HANDLE`, `TRY`/`FINALLY` constructs with per-element and fail-fast modes in streams (see Error Handling section)
- [x] **Design sum types and pattern matching**: `TYPE ... = | Variant1 | Variant2(payload)` syntax, `MATCH` expression with exhaustiveness checking (see Sum Types and Pattern Matching sections)
- [ ] **Establish naming conventions**: Variable naming, function naming, constants

### Formal Specification
- [ ] **Design formal EBNF grammar for RUMPS DSL**
  - Complete lexical structure (tokens, keywords, operators)
  - Expression grammar (including COLLECT streams, MATCH expressions)
  - Statement grammar (assignments, transactions, control flow, TRY/CATCH)
  - Sum type definitions and pattern syntax
  - Type annotations
  - Comments and documentation syntax
- [ ] **Create language specification document**
  - Formal semantics for each operation
  - Memory model and execution model
  - Transaction semantics within streams
  - Concurrency guarantees

### Parser Implementation
- [ ] **Create prototype parser for basic COLLECT operations**
  - Implement two-phase lexer → parser using `chumsky` (see Parser Architecture section)
  - Parse basic COLLECT with WHERE and SELECT
  - Generate initial AST representation
  - Add error recovery and helpful error messages
- [ ] **Implement full parser**
  - Support all stream operations
  - Handle both syntax forms (block and pipeline)
  - Include comprehensive error reporting with line/column info
  - Add syntax highlighting support data

### Interpreter/Compiler Design
- [ ] **Design AST representation**
  - Node types for all operations
  - Type information storage
  - Source location tracking
- [ ] **Create interpreter prototype**
  - Direct AST walking interpreter
  - Integration with Rust storage layer
  - Stream execution engine
- [ ] **Consider compilation strategies**
  - JIT compilation to Rust
  - Bytecode VM design
  - Direct native code generation

### Testing Infrastructure
- [ ] **Create DSL test suite**
  - Parser unit tests
  - Integration tests with storage
  - Performance benchmarks
  - Error case coverage
- [ ] **Build REPL for interactive development**
  - Syntax highlighting
  - Auto-completion
  - Interactive debugging

### Documentation
- [ ] **Write language tutorial**
  - Getting started guide
  - Migration guide from MUMPS
  - Best practices
  - Common patterns cookbook
- [ ] **Create reference documentation**
  - Complete operator reference
  - Built-in function reference
  - Standard library documentation
- [ ] **Develop example applications**
  - Healthcare data processing
  - Financial transaction processing
  - Log analysis system

## Design Principles

1. **Declarative First**: Express *what* not *how*
2. **Composable**: All operations should combine naturally
3. **Lazy by Default**: Don't compute until needed
4. **Memory Efficient**: Stream processing, not bulk loading
5. **Runtime Type Hints**: Optional annotations validated at runtime with clear error messages
6. **Automatic Coercion**: Implicit type conversions wherever sensible to reduce ceremony
7. **Predictable**: No hidden side effects or surprising behavior
8. **Performant**: Optimize automatically where possible
9. **Debuggable**: Clear error messages and debugging tools

## Parser Architecture

RUMPS uses a **two-phase lexer → parser** architecture built on the `chumsky` parser combinator library.

### Why Two-Phase

Single-pass parsing tends to become unwieldy for languages with:
- Whitespace-sensitive constructs (train-case identifiers vs spaced subtraction)
- Need to detect expression boundaries
- Complex operator sets with shared prefixes

A separate lexing phase handles these concerns cleanly, producing a token stream with spans that the parser then consumes.

### Architecture

```
Source → Lexer (tokens + spans) → Parser (AST) → Interpreter
              ↑                        ↑
          chumsky                  chumsky
```

### Why Chumsky

| Feature                    | Chumsky support                                           |
|----------------------------|-----------------------------------------------------------|
| Train-case identifiers     | Lexer handles `-` within idents vs spaced subtraction     |
| Operator precedence        | `pratt` parser or manual precedence climbing              |
| Good error messages        | Built-in error recovery, spans, and rich error types      |
| JSON-like literals         | Recursive descent is natural for `{ }` / `[ ]`            |
| Pipeline vs block forms    | Just grammar alternatives                                 |
| Regex literals `/pattern/` | Custom token with dedicated lexer rule                    |

If parsing becomes a performance bottleneck, rewrite it. Parser performance is rarely the bottleneck in practice for a query language.

### Lexer Considerations

**Train-case vs subtraction**: The rule "no spaces = identifier, spaces required for subtraction" means the lexer must be whitespace-aware when tokenizing `-`. If `-` has no surrounding whitespace and is followed by `[a-zA-Z]`, it's part of an identifier.

**Operator ambiguity**: Several operators share prefixes (`>`, `>=`, `->>`, `->`, `#>`, `#>>`). Use longest-match and order token rules carefully.

**String interpolation**: Template strings like `"Patient {id}: {name}"` can be:
- Parsed as raw strings, with interpolation handled in a later pass, or
- Tokenized into segments (`StringLit`, `Interpolation`, `StringLit`, ...) during lexing

The former is simpler; the latter gives better error spans for malformed interpolations (**NOTE**: leaning towards latter for better errors).

### Semantic Checks (Not Parsing Concerns)

Some things that look like parsing issues are actually semantic:

| Concern                               | Where handled                                  |
|---------------------------------------|------------------------------------------------|
| `"10" + 2` type error                 | Interpreter (AST just records `BinaryOp::Add`) |
| Undefined variable                    | Interpreter                                    |
| Type annotation mismatch              | Interpreter (or future strict mode)            |
| Transaction required for global write | Interpreter                                    |

## Future Enhancements

- **Strict Mode**: Optional parse-time validation when type annotations are present. When enabled, the interpreter would check annotated procedure signatures at definition time rather than call time, catching type mismatches earlier (useful for development/CI). This would not add static type inference—just validates that explicitly-annotated types are consistent.
- **Pattern Matching**: Destructuring in SELECT clauses
- **Window Functions**: Operations over sliding windows
- **Time-based Operations**: Temporal queries and aggregations
- **Recursive Queries**: Tree traversal operations
- **Memoization**: Cache expensive computations
- **Incremental Processing**: Process only changed data
- **Distributed Processing**: Scale across multiple nodes
- **ML Integration**: Built-in ML operations on streams

## References

- `persistence.md` - Phase 2.7: COLLECT implementation details
- `CLAUDE.md` - Project overview and RUMPS extensions
- MUMPS documentation - For comparison and compatibility

