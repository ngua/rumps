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
SET PATIENT_ID = 123
```

**Reserved keywords** (cannot be used as variable names):
`SET`, `GET`, `KILL`, `COLLECT`, `PROCEDURE`, `TRANSACTION`, `IF`, `ELSE`, `WHERE`, `SELECT`, `INTO`, `OUTPUT`, `INTO` etc.

### Train-Case Support

RUMPS supports **train-case** (kebab-case) identifiers, which are common in Lisp-like languages:

```rumps
SET my-var = 123
SET last-visit-date = Time.now()
PROCEDURE compute-total (items) DO ... END
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

TRANSACTION WITH ISOLATION READ_COMMITTED {
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
    FOREACH item => {
      TRANSACTION {  ; Nested transaction
        SET ^INVENTORY(item.id,"COUNT") = ^INVENTORY(item.id,"COUNT") - item.qty
      } ON CONFLICT ROLLBACK TO process-items
    }

  SET ^ORDER(id,"STATUS") = "COMPLETED"
}
```

### Transaction Context Variables
```rumps
TRANSACTION {
  ; Built-in transaction context
  SET ^AUDIT(Txn.id, "USER") = Session.user
  SET ^AUDIT(Txn.id, "TIME") = Txn.start-time
  SET ^AUDIT(Txn.id, "OPERATIONS") = Txn.op-count
}
```

### Error Handling in Transactions
```rumps
TRANSACTION {
  SET ^CRITICAL(id) = value
} ON ERROR {
  ; Error handling block
  LOG "Transaction failed: " + Error.message
  ALERT admin-user
} FINALLY {
  ; Cleanup code that always runs
  RELEASE locks
}
```

## Operators

RUMPS modernizes MUMPS operators, making them more readable and consistent with modern programming languages.

### Operator Comparison Table

| Category              | MUMPS           | RUMPS                | Description          | Notes              |
|-----------------------|-----------------|----------------------|----------------------|--------------------|
| **Arithmetic**        |                 |                      |                      |                    |
| Addition              | `+`             | `+`                  | Add two numbers      | Same               |
| Subtraction           | `-`             | `-`                  | Subtract             | Same               |
| Multiplication        | `*`             | `*`                  | Multiply             | Same               |
| Division              | `/`             | `/`                  | Divide (float)       | Same               |
| Integer Division      | `\`             | `//`                 | Integer division     | More intuitive     |
| Modulo                | `#`             | `%`                  | Remainder            | Standard notation  |
| Exponentiation        | `**`            | `^` or `**`          | Power                | Both supported     |
| **String**            |                 |                      |                      |                    |
| Concatenation         | `_`             | `+` or `++`          | String concat        | More intuitive     |
| Contains              | `[`             | `contains`           | Substring check      | Clearer            |
| Not Contains          | `']`            | `!contains`          | Not substring        | Clearer            |
| Follows               | `]`             | `>`                  | String comparison    | Context-aware      |
| Pattern Match         | `?`             | `matches` or `~`     | Regex match          | Modern regex       |
| **Comparison**        |                 |                      |                      |                    |
| Equals                | `=`             | `==`                 | Equality             | Consistent         |
| Not Equals            | `'=`            | `!=` or `≠`          | Inequality           | Standard           |
| Less Than             | `<`             | `<`                  | Less than            | Same               |
| Greater Than          | `>`             | `>`                  | Greater than         | Same               |
| Less or Equal         | `<=` or `'>`    | `<=` or `≤`          | Less or equal        | Standard           |
| Greater or Equal      | `>=` or `'<`    | `>=` or `≥`          | Greater or equal     | Standard           |
| **Logical**           |                 |                      |                      |                    |
| And                   | `&` or `&&`     | `AND` or `&&`        | Logical AND          | Clearer            |
| Or                    | `!` or `!!`     | `OR` or `\|\|`       | Logical OR           | Standard           |
| Not                   | `'`             | `NOT` or `!`         | Logical NOT          | Standard           |
| **Special**           |                 |                      |                      |                    |
| Indirection           | `@`             | `@` or `eval`        | Dynamic evaluation   | Enhanced           |
| Global Prefix         | `^`             | `^`                  | Global variable      | Same               |
| Function Prefix       | `$`             | `$`                  | Built-in function    | Same               |
| **Assignment**        |                 |                      |                      |                    |
| Set                   | `SET` or `S`    | `SET` or `=`         | Assignment           | Flexible           |
| Kill                  | `KILL` or `K`   | `KILL` or `DELETE`   | Delete variable      | Options            |
| **New in RUMPS**      |                 |                      |                      |                    |
| Null Coalesce         | N/A             | `??`                 | Default if null      | `a ?? b`           |
| Optional Chain        | N/A             | `?.`                 | Safe navigation      | `obj?.field`       |
| Pipe                  | N/A             | `\|>`                | Pipeline operator    | Functional         |
| Range                 | N/A             | `..`                 | Range operator       | `1..10`            |
| Spread                | N/A             | `...`                | Spread operator      | `...array`         |
| Type Check            | N/A             | `is`                 | Type checking        | `x is Number`      |

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
SET fullname = first + " " + last  ; or first ++ " " ++ last
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
^DATA
  |> COLLECT
  |> FILTER active
  |> MAP transform
  |> OUTPUT

; Range operator
FOR i IN 1..100 {  ; If we add traditional loops as alternative
  ; Process
}

; Spread operator in collections
SET combined = [...array1, ...array2]

; Type checking
IF value is Number {
  SET res = value * 2
} ELSE IF value is String {
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
  FILTER value != null
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

## Procedures

Procedures are named, reusable blocks of code. They can accept arguments, perform computations, and yield a result.

### Basic Syntax

```rumps
PROCEDURE <name> (<args>) DO
  <body>
END
```

### Simple Procedures

```rumps
PROCEDURE greet (name) DO
  OUTPUT "Hello, " + name + "!"
END

PROCEDURE add (a, b) DO
  a + b
END

PROCEDURE square (x) DO
  x * x
END
```

The last expression in a procedure body is its result (no explicit `RETURN`).

### Calling Procedures

```rumps
; Direct call
greet("World")

; Capture result
SET sum = add(10, 20)

; In expressions
SET area = square(side) * 4

; In pipelines
^NUMBERS
  |> COLLECT
  |> MAP square
  |> OUTPUT
```

### Multi-Statement Procedures

Use `;` or newlines to separate statements. The final expression is the result:

```rumps
PROCEDURE process-patient (id) DO
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
END
```

### Procedures with Side Effects

Procedures that perform side effects but don't need to yield a value:

```rumps
PROCEDURE log-access (user, resource) DO
  TRANSACTION {
    SET ^AUDIT(Time.now(), user) = resource
  }
END

PROCEDURE notify-all (msg) DO
  COLLECT ^USERS
    WHERE value..active == true
    FOREACH user => {
      send-notification(user..id, msg)
    }
END
```

### Procedures in Stream Operations

Procedures integrate naturally with `COLLECT` streams:

```rumps
PROCEDURE is-adult (record) DO
  record..age >= 18
END

PROCEDURE format-name (record) DO
  record..last + ", " + record..first
END

; Use in pipeline
COLLECT ^PERSONS
  FILTER is-adult
  MAP format-name
  OUTPUT
```

### Recursive Procedures

```rumps
PROCEDURE factorial (n) DO
  IF n <= 1 { 1 }
  ELSE { n * factorial(n - 1) }
END

PROCEDURE tree-sum (node-key) DO
  SET val = GET(^TREE(node-key, "VALUE")) ?? 0
  SET children-sum = COLLECT ^TREE(node-key, "CHILDREN")
    MAP tree-sum
    AGGREGATE SUM

  val + children-sum
END
```

### Closures / Anonymous Procedures (Future)

For inline use in streams:

```rumps
; Potential syntax options:

; Arrow syntax
COLLECT ^DATA
  MAP (x) => x * 2
  FILTER (x) => x > 10

; Block syntax
COLLECT ^DATA
  MAP { |x| x * 2 }
  FILTER { |x| x > 10 }

; DO syntax (consistent with procedures)
COLLECT ^DATA
  MAP DO (x) x * 2 END
  FILTER DO (x) x > 10 END
```

**Open question**: Which anonymous procedure syntax to adopt?

## Namespaces

RUMPS uses **PascalCase namespaces** with dot notation for organizing functions and types.

### Defining Namespaces

```rumps
NAMESPACE MyUtils DO
  PROCEDURE double (x: Int) -> Int DO
    x * 2
  END

  PROCEDURE triple (x: Int) -> Int DO
    x * 3
  END
END

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
| `Option.some(v)`         | Wrap value           | `Option.some(42)` → `Some(42)`            |
| `Option.none()`          | Empty option         | `Option.none()` → `None`                  |
| `Option.is-some(o)`      | Check if Some        | `Option.is-some(Some(1))` → `true`        |
| `Option.is-none(o)`      | Check if None        | `Option.is-none(None)` → `true`           |
| `Option.unwrap(o)`       | Get value or panic   | `Option.unwrap(Some(1))` → `1`            |
| `Option.unwrap-or(o, d)` | Get value or default | `Option.unwrap-or(None, 0)` → `0`         |
| `Option.map(o, f)`       | Transform if Some    | `Option.map(Some(1), double)` → `Some(2)` |

#### `Result` — Result operations

| Function                 | Description          | Example                                   |
|--------------------------|----------------------|-------------------------------------------|
| `Result.ok(v)`           | Success value        | `Result.ok(42)` → `Ok(42)`                |
| `Result.err(e)`          | Error value          | `Result.err("fail")` → `Err("fail")`      |
| `Result.is-ok(r)`        | Check if Ok          | `Result.is-ok(Ok(1))` → `true`            |
| `Result.is-err(r)`       | Check if Err         | `Result.is-err(Err("x"))` → `true`        |
| `Result.unwrap(r)`       | Get value or panic   | `Result.unwrap(Ok(1))` → `1`              |
| `Result.unwrap-or(r, d)` | Get value or default | `Result.unwrap-or(Err("x"), 0)` → `0`     |
| `Result.map(r, f)`       | Transform if Ok      | `Result.map(Ok(1), double)` → `Ok(2)`     |
| `Result.map-err(r, f)`   | Transform if Err     | `Result.map-err(Err("x"), upper)` → `...` |

#### `Io` — Input/Output (Future)

| Function                 | Description        |
|--------------------------|--------------------|
| `Io.read-file(path)`     | Read file contents |
| `Io.write-file(path, s)` | Write to file      |
| `Io.stdin()`             | Read from stdin    |
| `Io.print(s)`            | Print to stdout    |
| `Io.eprint(s)`           | Print to stderr    |

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

| Type     | Description           | Examples               |
|----------|-----------------------|------------------------|
| `Null`   | Null/undefined value  | `null`                 |
| `Bool`   | Boolean               | `true`, `false`        |
| `Int`    | Integer               | `42`, `-7`, `0`        |
| `Float`  | Floating-point number | `3.14`, `-0.5`, `1e10` |
| `String` | Text string           | `"hello"`, `""`        |

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

| Type          | Description                           | Native Equivalent   |
|---------------|---------------------------------------|---------------------|
| `Json`        | Any JSON value                        | `Any`               |
| `Json.Null`   | JSON null                             | `Null`              |
| `Json.Bool`   | JSON boolean                          | `Bool`              |
| `Json.Number` | JSON number (no int/float distinction)| `Number`            |
| `Json.String` | JSON string                           | `String`            |
| `Json.Array`  | JSON array (heterogeneous)            | `Array[Json]`       |
| `Json.Object` | JSON object (string keys)             | `Map[String, Json]` |

### Native vs JSON

```rumps
; Native types - from RUMPS operations, fully typed
SET keys: Array[Key] = COLLECT ^DATA SELECT key INTO
SET counts: Map[String, Int] = compute-histogram(data)

; JSON types - from parsing, dynamically typed
SET json: Json = Json.parse(input-str)
SET arr: Json.Array = Json.parse("[1, 2, 3]")

; Convert JSON to native (validated at runtime)
SET nums: Array[Int] = arr as Array[Int]

; Convert native to JSON
SET out: Json = nums as Json
```

**Key differences**:
- Native `Array[Int]` guarantees all elements are `Int`
- `Json.Array` can contain mixed types (`[1, "hello", true]`)
- Native `Map[K, V]` has typed keys; `Json.Object` always has `String` keys
- JSON numbers don't distinguish `Int` vs `Float`

### Type Annotations

Type annotations are **optional hints** that the interpreter validates at runtime. RUMPS is an interpreted query language, not a compiled programming language—there is no static type checking. Annotations serve as:

1. **Documentation** for procedure signatures
2. **Runtime guards** that produce interpreter errors if violated
3. **Self-describing contracts** for API boundaries

```rumps
; Untyped (no runtime validation)
PROCEDURE add (a, b) DO
  a + b
END

; Typed arguments (runtime error if wrong type passed)
PROCEDURE add (a: Int, b: Int) DO
  a + b
END

; Typed arguments and return (validates both input and output)
PROCEDURE add (a: Int, b: Int) -> Int DO
  a + b
END

; Complex types
PROCEDURE process (data: Json.Object) -> Json.Array DO
  Json.values(data)
END
```

When a type annotation is violated, the interpreter raises a runtime error with a clear message indicating the expected vs actual type.

### Type Checking

Runtime type checking with `is`:

```rumps
IF value is String {
  OUTPUT "It's a string: " + value
} ELSE IF value is Number {
  OUTPUT "It's a number: " + String.from(value)
} ELSE IF value is Null {
  OUTPUT "It's null"
}
```

Get type as a value with `Type.of`:

```rumps
SET t = Type.of(value)

IF t == String { ... }
IF t == Int OR t == Float { ... }
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

| Falsy                                         | Truthy         |
|-----------------------------------------------|----------------|
| `null`, `false`, `0`, `0.0`, `""`, `[]`, `{}` | Everything else|

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
PROCEDURE sum (nums: Array[Int]) -> Int DO
  nums |> AGGREGATE SUM
END

; Optional/nullable return
PROCEDURE find (id: Int) -> Option[String] DO
  GET(^DATA(id, "NAME"))
END

; Result type for fallible operations
PROCEDURE parse-int (s: String) -> Result[Int, String] DO
  ; returns Ok[Int] or Err[String]
END

; Stream processing with known element type
PROCEDURE get-names () -> Stream[String] DO
  COLLECT ^PATIENTS
    SELECT value..name
END

; Procedure as first-class value
PROCEDURE apply-twice (f: Proc[Int, Int], x: Int) -> Int DO
  f(f(x))
END

; Map type
PROCEDURE word-count (words: Array[String]) -> Map[String, Int] DO
  ; ...
END
```

### Structural Object Types (Future)

For objects with known shape:

```rumps
; Inline structural type
PROCEDURE process (patient: {name: String, age: Int}) DO
  OUTPUT patient..name
END

; Type alias
TYPE Patient = {
  name: String,
  age: Int,
  active: Bool
}

PROCEDURE admit (p: Patient) DO
  ; ...
END
```

**Resolved**: All type checking is **runtime only**. RUMPS is an interpreted query language—type annotations are validated when code executes, not at parse time.

## Fundamental Primitive: COLLECT

The `COLLECT` primitive is the **foundation for ALL iteration** in RUMPS. It creates a lazy stream from a B-tree variable that can be transformed, filtered, and consumed.

### Basic Syntax Forms

#### 1. Block Form
```rumps
COLLECT ^DATA
  WHERE condition
  SELECT transformation
  ACTION
```

#### 2. Pipeline Form
```rumps
^DATA
  |> COLLECT WHERE condition
  |> SELECT transformation
  |> ACTION
```

Both forms are equivalent and can be used interchangeably based on preference and readability.

## Stream Operations

All operations are composable and can be chained together. Operations are **lazy** - they don't execute until a terminal operation (like `OUTPUT` or `INTO`) is reached.

### Filtering Operations

#### WHERE - Filter by predicate
```rumps
COLLECT ^PATIENT
  WHERE key[0] > 100 AND key[0] < 200
  WHERE has-value  ; Multiple WHERE clauses are ANDed together
```

#### WHILE - Take while condition is true (early termination)
```rumps
COLLECT ^LOG
  WHILE key[0] <= "2025-01-01"  ; Stops at first false condition
```

#### FILTER - Post-selection filtering
```rumps
COLLECT ^PATIENT
  SELECT GET(^PATIENT(key[0],"NAME"))
  FILTER value.contains("Smith")
```

### Transformation Operations

#### SELECT - Transform each element
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

#### MAP - Alias for SELECT (for familiarity)
```rumps
COLLECT ^DATA
  MAP process-record
```

### Limiting Operations

#### TAKE - Take first N elements
```rumps
COLLECT ^LOG
  TAKE 100
```

#### SKIP - Skip first N elements
```rumps
COLLECT ^LOG
  SKIP 100
  TAKE 50  ; Get items 101-150
```

#### TAKE_WHILE / SKIP_WHILE - Conditional limiting
```rumps
COLLECT ^DATA
  SKIP_WHILE value < 0
  TAKE_WHILE value < 1000
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
```rumps
COLLECT ^PATIENT
  SELECT { id: key[0], name: GET(^PATIENT(key[0],"NAME")) }
  SORT BY name ASC
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
  MAP expensive-op
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

### OUTPUT - Write to console

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

### FOREACH - Side effects
```rumps
COLLECT ^TASKS
  FOREACH process-task  ; Execute function for each element
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
  SELECT {
    id: key[0],
    last-visit: GET(^PATIENT(key[0],"LASTVISIT"))
  }
  FILTER last-visit > 20250101
  OUTPUT "Patient {id} last visited on {last-visit}"

; Get count
COLLECT ^PATIENT
  WHERE has-descendants
  FILTER GET(^PATIENT(key[0],"LASTVISIT")) > 20250101
  COUNT INTO recent-count
```

### Example 2: Top 10 customers by order value
```rumps
^ORDERS
  |> COLLECT
  |> GROUP BY GET(^ORDERS(key[0],"CUSTOMER-ID"))
  |> AGGREGATE SUM GET(^ORDERS(key[0],"AMOUNT")) INTO total
  |> SORT BY total DESC
  |> TAKE 10
  |> JOIN ^CUSTOMER ON group-key
  |> SELECT {
       customer-name: GET(^CUSTOMER(group-key,"NAME")),
       total-orders: total
     }
  |> OUTPUT AS TABLE HEADERS ["Customer", "Total Orders"]
```

### Example 3: ETL Pipeline
```rumps
; Extract, transform, and load data
TRANSACTION {
  COLLECT ^RAW-DATA
    WHERE key[0] >= last-processed-id
    PARALLEL 5
    MAP validate-record
    FILTER is-valid
    MAP transform-record
    SELECT {
      id: generate-id(),
      data: transformed-val,
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
| Filtering           | `IF` statements in loop body     | `WHERE` / `FILTER`        | Declarative intent     |
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
| ```mumps```<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET NAME=$GET(^PAT(ID,"NAME"))`<br/>`. IF NAME["Smith" DO`<br/>`. . ; Process Smith patients` | ```rumps```<br/>`COLLECT ^PAT`<br/>`  WHERE has-descendants`<br/>`  SELECT GET(^PAT(key[0],"NAME"))`<br/>`  FILTER value.contains("Smith")`<br/>`  ; Process automatically` | Filter with condition |
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
- [ ] Implement lexer/tokenizer
- [ ] Implement parser (recursive descent or parser combinator)
  - Or just use `chumsky`? Worth investigating
- [ ] Build AST representation
- [ ] Implement interpreter that calls Rust storage layer

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

1. **Syntax Style**: Should we support both block form and pipeline form, or standardize on one?
2. ~~**Type System**: How much type inference vs explicit typing?~~ **Resolved**: No static type checking. Type annotations are optional runtime hints—validated when executed, producing interpreter errors if violated. Conservative automatic coercion (into strings, between numerics, but not from strings to numbers). A future "strict mode" may add optional parse-time validation for annotated procedures.
3. **Error Handling**: How to handle errors in stream processing?
4. **Transaction Integration**: How do streams interact with transaction boundaries?
5. **Performance Hints**: Should we allow manual optimization hints?
6. **Debugging**: What debugging/profiling features should be built-in?
7. **Interop**: How to call Rust functions from DSL and vice versa?
8. **Standard Library**: What built-in functions should be provided?

## Immediate TODOs (Post-Storage Engine)

These tasks should be completed after the storage engine implementation is finished:

### Language Design
- [ ] **Resolve syntax style decision**: Decide between supporting both block and pipeline forms or standardizing on one
  - Consider user survey or prototype both to test ergonomics
  - Document decision rationale for future reference
- [ ] **Define operator precedence**: Establish clear precedence rules for all operations
- [x] **Specify type coercion rules**: Conservative coercion — into strings and between numerics, but NOT from strings to numbers (see Type Coercion section)
- [ ] **Design error handling semantics**: Define how errors propagate through streams
- [ ] **Establish naming conventions**: Variable naming, function naming, constants

### Formal Specification
- [ ] **Design formal EBNF grammar for RUMPS DSL**
  - Complete lexical structure (tokens, keywords, operators)
  - Expression grammar (including COLLECT streams)
  - Statement grammar (assignments, transactions, control flow)
  - Type annotations (if explicit typing is supported)
  - Comments and documentation syntax
- [ ] **Create language specification document**
  - Formal semantics for each operation
  - Memory model and execution model
  - Transaction semantics within streams
  - Concurrency guarantees

### Parser Implementation
- [ ] **Create prototype parser for basic COLLECT operations**
  - Choose parsing approach (recursive descent, parser combinator, or parser generator)
  - Implement tokenizer/lexer
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

---

Last Updated: 2025-11-29
