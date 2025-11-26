# RUMPS DSL Design Document

This document outlines the design of the RUMPS Domain-Specific Language (DSL), a declarative descendant of MUMPS that replaces imperative loops with functional stream operations.

## Core Philosophy

RUMPS is **not** a reimplementation of MUMPS, but rather a **declarative evolution** that:
- Replaces all imperative loops (`FOR`, `WHILE`) with stream-based operations
- Provides functional composition through pipeline operators
- Ensures memory efficiency through lazy evaluation
- Enables automatic parallelization and optimization
- Maintains type safety (future enhancement)

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
  SET ^INVENTORY(item_id,"COUNT") = ^INVENTORY(item_id,"COUNT") - 1
  SET ^ORDER(order_id,"STATUS") = "PROCESSED"
} ON CONFLICT RETRY 3

TRANSACTION {
  ; Critical section that must succeed or fail entirely
  SET ^ACCOUNT(from,"BALANCE") = ^ACCOUNT(from,"BALANCE") - amount
  SET ^ACCOUNT(to,"BALANCE") = ^ACCOUNT(to,"BALANCE") + amount
} ON CONFLICT ABORT

TRANSACTION {
  ; Optimistic update - skip if already modified
  SET ^CACHE(key) = computed_value
} ON CONFLICT SKIP

TRANSACTION {
  ; Last-write-wins semantics
  SET ^CONFIG(setting) = new_value
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

  SAVEPOINT process_items

  $COLLECT ^ORDER(id,"ITEMS")
    FOREACH item => {
      TRANSACTION {  ; Nested transaction
        SET ^INVENTORY(item.id,"COUNT") = ^INVENTORY(item.id,"COUNT") - item.qty
      } ON CONFLICT ROLLBACK TO process_items
    }

  SET ^ORDER(id,"STATUS") = "COMPLETED"
}
```

### Transaction Context Variables
```rumps
TRANSACTION {
  ; Built-in transaction variables
  SET ^AUDIT($TXN.ID, "USER") = $USER
  SET ^AUDIT($TXN.ID, "TIME") = $TXN.START_TIME
  SET ^AUDIT($TXN.ID, "OPERATIONS") = $TXN.OP_COUNT
}
```

### Error Handling in Transactions
```rumps
TRANSACTION {
  SET ^CRITICAL(id) = value
} ON ERROR {
  ; Error handling block
  LOG "Transaction failed: " + $ERROR.MESSAGE
  ALERT admin_user
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
SET name = $GET(^PATIENT(id,"NAME")) ?? "Unknown"

; Optional chaining - safe navigation
SET city = patient?.address?.city ?? "N/A"

; Pipeline operator for functional composition
^DATA
  |> $COLLECT
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
  SET result = value * 2
} ELSE IF value is String {
  SET result = "Value: " + value
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
  SET valid_email = true
}

; Named capture groups
IF date matches /(?<year>\d{4})-(?<month>\d{2})-(?<day>\d{2})/ {
  SET year = $MATCH.year
  SET month = $MATCH.month
  SET day = $MATCH.day
}
```

## Fundamental Primitive: $COLLECT

The `$COLLECT` primitive is the **foundation for ALL iteration** in RUMPS. It creates a lazy stream from a B-tree variable that can be transformed, filtered, and consumed.

### Basic Syntax Forms

#### 1. Block Form
```rumps
$COLLECT ^DATA
  WHERE condition
  SELECT transformation
  ACTION
```

#### 2. Pipeline Form
```rumps
^DATA
  |> $COLLECT WHERE condition
  |> SELECT transformation
  |> ACTION
```

Both forms are equivalent and can be used interchangeably based on preference and readability.

## Stream Operations

All operations are composable and can be chained together. Operations are **lazy** - they don't execute until a terminal operation (like `OUTPUT` or `INTO`) is reached.

### Filtering Operations

#### WHERE - Filter by predicate
```rumps
$COLLECT ^PATIENT
  WHERE key[0] > 100 AND key[0] < 200
  WHERE has_value  ; Multiple WHERE clauses are ANDed together
```

#### WHILE - Take while condition is true (early termination)
```rumps
$COLLECT ^LOG
  WHILE key[0] <= "2025-01-01"  ; Stops at first false condition
```

#### FILTER - Post-selection filtering
```rumps
$COLLECT ^PATIENT
  SELECT $GET(^PATIENT(key[0],"NAME"))
  FILTER value.contains("Smith")
```

### Transformation Operations

#### SELECT - Transform each element
```rumps
; Simple selection
$COLLECT ^DATA
  SELECT value

; Field extraction
$COLLECT ^PATIENT
  SELECT $GET(^PATIENT(key[0],"NAME"))

; Object construction
$COLLECT ^PATIENT
  SELECT {
    id: key[0],
    name: $GET(^PATIENT(key[0],"NAME")),
    dob: $GET(^PATIENT(key[0],"DOB"))
  }
```

#### MAP - Alias for SELECT (for familiarity)
```rumps
$COLLECT ^DATA
  MAP fn_process_record
```

### Limiting Operations

#### TAKE - Take first N elements
```rumps
$COLLECT ^LOG
  TAKE 100
```

#### SKIP - Skip first N elements
```rumps
$COLLECT ^LOG
  SKIP 100
  TAKE 50  ; Get items 101-150
```

#### TAKE_WHILE / SKIP_WHILE - Conditional limiting
```rumps
$COLLECT ^DATA
  SKIP_WHILE value < 0
  TAKE_WHILE value < 1000
```

### Aggregation Operations

#### AGGREGATE - Multiple aggregations at once
```rumps
$COLLECT ^SALES
  SELECT $GET(^SALES(key[0],"AMOUNT"))
  AGGREGATE
    COUNT INTO total_sales
    SUM INTO total_revenue
    AVG INTO average_sale
    MIN INTO smallest_sale
    MAX INTO largest_sale
```

#### COUNT - Count elements
```rumps
$COLLECT ^PATIENT
  COUNT INTO patient_count
```

#### REDUCE - Custom reduction
```rumps
$COLLECT ^DATA
  REDUCE WITH fn_custom_reducer INITIAL 0 INTO result
```

### Grouping Operations

#### GROUP BY - Group elements by key
```rumps
$COLLECT ^VISITS
  WHERE key[1] == "2025"
  GROUP BY key[0]  ; Group by patient ID
  AGGREGATE COUNT INTO visit_counts
```

### Ordering Operations

#### SORT BY - Sort stream
```rumps
$COLLECT ^PATIENT
  SELECT { id: key[0], name: $GET(^PATIENT(key[0],"NAME")) }
  SORT BY name ASC
```

#### REVERSE - Reverse stream order
```rumps
$COLLECT ^DATA
  REVERSE
```

### Join Operations

#### JOIN - Join with another variable
```rumps
$COLLECT ^ORDER
  JOIN ^CUSTOMER ON key[0] == ^CUSTOMER.key[0]
  SELECT { order: value, customer: ^CUSTOMER.value }
```

### Parallel Processing

#### PARALLEL - Process in parallel
```rumps
$COLLECT ^RECORDS
  PARALLEL 10  ; Process up to 10 records concurrently
  MAP expensive_operation
```

## Terminal Operations

Terminal operations consume the stream and produce a result.

### INTO - Collect into variable
```rumps
$COLLECT ^DATA
  SELECT value
  INTO results  ; Local variable

$COLLECT ^DATA
  SELECT value
  INTO ^PROCESSED  ; Global variable (requires transaction)
```

### OUTPUT - Write to console

The `OUTPUT` operation in RUMPS supports multiple formatting options for flexible console output.

#### Simple Output
```rumps
$COLLECT ^DATA
  OUTPUT  ; Each item on new line
```

#### Template Output
```rumps
$COLLECT ^PATIENT
  SELECT { id: key[0], name: $GET(^PATIENT(key[0],"NAME")) }
  OUTPUT "Patient #{id}: {name}"
```

#### Formatted Output
```rumps
; JSON format
$COLLECT ^CONFIG
  OUTPUT AS JSON

; Table format
$COLLECT ^STATS
  OUTPUT AS TABLE HEADERS ["Date", "Count", "Average"]

; CSV format
$COLLECT ^DATA
  OUTPUT WITH SEPARATOR ","

; XML format (future)
$COLLECT ^DATA
  OUTPUT AS XML ROOT "records" ELEMENT "record"
```

#### Output Targets
```rumps
; Standard error
$COLLECT ^ERRORS
  OUTPUT TO ERROR

; File output (future)
$COLLECT ^DATA
  OUTPUT TO FILE "/tmp/output.txt"

; Network output (future)
$COLLECT ^METRICS
  OUTPUT TO HTTP "https://metrics.example.com/api"
```

#### Extended Output Examples
```rumps
; Simple output - each item on a new line
$COLLECT ^DATA
  OUTPUT

; Template-based formatting with field interpolation
$COLLECT ^PATIENT
  SELECT { id: key[0], name: $GET(^PATIENT(key[0],"NAME")) }
  OUTPUT "ID: {id} - Name: {name}"

; JSON output for structured data
$COLLECT ^CONFIG
  SELECT { key: key, value: value }
  OUTPUT AS JSON

; Table formatting for reports
$COLLECT ^STATS
  SELECT { date: key[0], total: value.sum, avg: value.avg }
  OUTPUT AS TABLE HEADERS ["Date", "Total", "Average"]

; Custom separators and formatting
$COLLECT ^LIST
  OUTPUT WITH SEPARATOR ", "  ; Output as comma-separated values

; Conditional output
$COLLECT ^ERRORS
  WHERE value.severity == "HIGH"
  OUTPUT TO ERROR  ; Write to stderr instead of stdout
```

This declarative output approach eliminates the need for manual formatting loops and provides consistent, reusable output patterns.

### FOREACH - Side effects
```rumps
$COLLECT ^TASKS
  FOREACH fn_process_task  ; Execute function for each element
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
$COLLECT ^PATIENT
  WHERE has_descendants
  SELECT {
    id: key[0],
    last_visit: $GET(^PATIENT(key[0],"LASTVISIT"))
  }
  FILTER last_visit > 20250101
  OUTPUT "Patient {id} last visited on {last_visit}"

; Get count
$COLLECT ^PATIENT
  WHERE has_descendants
  FILTER $GET(^PATIENT(key[0],"LASTVISIT")) > 20250101
  COUNT INTO recent_count
```

### Example 2: Top 10 customers by order value
```rumps
^ORDERS
  |> $COLLECT
  |> GROUP BY $GET(^ORDERS(key[0],"CUSTOMER_ID"))
  |> AGGREGATE SUM $GET(^ORDERS(key[0],"AMOUNT")) INTO total
  |> SORT BY total DESC
  |> TAKE 10
  |> JOIN ^CUSTOMER ON group_key
  |> SELECT {
       customer_name: $GET(^CUSTOMER(group_key,"NAME")),
       total_orders: total
     }
  |> OUTPUT AS TABLE HEADERS ["Customer", "Total Orders"]
```

### Example 3: ETL Pipeline
```rumps
; Extract, transform, and load data
TRANSACTION {
  $COLLECT ^RAW_DATA
    WHERE key[0] >= last_processed_id
    PARALLEL 5
    MAP fn_validate_record
    FILTER is_valid
    MAP fn_transform_record
    SELECT {
      id: generate_id(),
      data: transformed_value,
      processed_at: $NOW
    }
    INTO ^PROCESSED_DATA

  SET last_processed_id = $LAST(^RAW_DATA)
}
```

## Comparison with Traditional MUMPS

### Summary Table

| Pattern | Traditional MUMPS | RUMPS DSL | Benefits |
|---------|------------------|-----------|----------|
| Simple iteration | `FOR SET I=$O(^D(I)) Q:I="" DO` | `$COLLECT ^D` | Cleaner syntax |
| Filtering | `IF` statements in loop body | `WHERE` / `FILTER` clauses | Declarative intent |
| Counting | Manual counter variable | `COUNT INTO` | No state management |
| First N items | Counter with `QUIT` | `TAKE n` | Clear intent |
| Aggregation | Manual accumulator variables | `AGGREGATE` operations | Built-in operations |
| Output | Multiple `WRITE` statements | `OUTPUT` with templates | Flexible formatting |
| Parallel processing | Not available | `PARALLEL n` | Automatic optimization |
| Error handling | Manual checks | Stream error propagation | Consistent handling |

### Detailed Pattern Comparison

The following table shows common MUMPS iteration patterns and their conceptual RUMPS equivalents:

| Traditional MUMPS (Imperative) | Future RUMPS DSL (Declarative) | Description |
|--------------------------------|--------------------------------|-------------|
| ```mumps```<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. WRITE ID,!` | ```rumps```<br/>`$COLLECT ^DATA`<br/>`  SELECT key[0]`<br/>`  OUTPUT` | Iterate through all top-level keys |
| ```mumps```<br/>`SET CNT=0`<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET CNT=CNT+1`<br/>`WRITE "Total: ",CNT,!` | ```rumps```<br/>`$COLLECT ^PAT`<br/>`  COUNT INTO total`<br/>`WRITE "Total: ",total,!` | Count entries |
| ```mumps```<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. IF ID>100 QUIT`<br/>`. ; Process ID` | ```rumps```<br/>`$COLLECT ^DATA`<br/>`  WHILE key[0] <= 100`<br/>`  ; Process automatically` | Early termination with condition |
| ```mumps```<br/>`SET I=0`<br/>`FOR  SET ID=$ORDER(^LOG(ID)) QUIT:ID=""  DO`<br/>`. SET I=I+1`<br/>`. IF I>10 QUIT`<br/>`. ; Process first 10` | ```rumps```<br/>`$COLLECT ^LOG`<br/>`  TAKE 10`<br/>`  ; Process automatically` | Take first N entries |
| ```mumps```<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET NAME=$GET(^PAT(ID,"NAME"))`<br/>`. IF NAME["Smith" DO`<br/>`. . ; Process Smith patients` | ```rumps```<br/>`$COLLECT ^PAT`<br/>`  WHERE has_descendants`<br/>`  SELECT $GET(^PAT(key[0],"NAME"))`<br/>`  FILTER value.contains("Smith")`<br/>`  ; Process automatically` | Filter with condition |
| ```mumps```<br/>`KILL RESULTS`<br/>`SET CNT=0`<br/>`FOR  SET ID=$ORDER(^DATA(ID)) QUIT:ID=""  DO`<br/>`. SET CNT=CNT+1`<br/>`. SET RESULTS(CNT)=$GET(^DATA(ID,"VAL"))` | ```rumps```<br/>`$COLLECT ^DATA`<br/>`  SELECT $GET(^DATA(key[0],"VAL"))`<br/>`  INTO RESULTS` | Collect into array |
| ```mumps```<br/>`FOR  SET D=$ORDER(^LOG(2025,D)) QUIT:D=""  DO`<br/>`. FOR  SET T=$ORDER(^LOG(2025,D,T)) QUIT:T=""  DO`<br/>`. . ; Process each timestamp` | ```rumps```<br/>`$COLLECT ^LOG`<br/>`  WHERE key[0] == 2025 AND key.len == 3`<br/>`  ; All 2025 timestamps, flat` | Nested iteration (flattened) |
| ```mumps```<br/>`; Complex aggregation`<br/>`SET TOT=0,CNT=0`<br/>`FOR  SET ID=$ORDER(^SALE(ID)) QUIT:ID=""  DO`<br/>`. SET AMT=$GET(^SALE(ID,"AMOUNT"))`<br/>`. SET TOT=TOT+AMT,CNT=CNT+1`<br/>`SET AVG=TOT/CNT` | ```rumps```<br/>`$COLLECT ^SALE`<br/>`  SELECT $GET(^SALE(key[0],"AMOUNT"))`<br/>`  AGGREGATE`<br/>`    SUM INTO total`<br/>`    COUNT INTO count`<br/>`    AVG INTO average` | Aggregation operations |
| ```mumps```<br/>`; Display all patient info`<br/>`FOR  SET ID=$ORDER(^PAT(ID)) QUIT:ID=""  DO`<br/>`. SET NAME=$GET(^PAT(ID,"NAME"))`<br/>`. SET DOB=$GET(^PAT(ID,"DOB"))`<br/>`. WRITE "Patient ",ID,": ",NAME`<br/>`. WRITE " (DOB: ",DOB,")",!` | ```rumps```<br/>`$COLLECT ^PAT`<br/>`  SELECT {`<br/>`    id: key[0],`<br/>`    name: $GET(^PAT(key[0],"NAME")),`<br/>`    dob: $GET(^PAT(key[0],"DOB"))`<br/>`  }`<br/>`  OUTPUT "Patient {id}: {name} (DOB: {dob})"` | Console output with formatting |

### Key Advantages of RUMPS `$COLLECT` Approach

1. **No Manual State**: No need for iteration variables, counters, or manual loop control
2. **Declarative Intent**: The code expresses *what* you want, not *how* to iterate
3. **Automatic Optimization**: The runtime can optimize streaming, batching, and parallelization
4. **Composable Operations**: Stream operations naturally chain together
5. **Memory Efficient**: Lazy evaluation means data isn't loaded until needed
6. **Type Safe**: Can be statically checked at compile time (future enhancement)

## Implementation Phases

### Phase 1: Core Stream Operations (Storage Layer)
- [ ] Implement `$COLLECT` primitive in Rust (see `persistence.md` Phase 2.7)
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
- [ ] Build AST representation
- [ ] Implement interpreter that calls Rust storage layer

### Phase 4: Output Formatting
- [ ] Template string interpolation
- [ ] JSON output formatter
- [ ] Table output formatter
- [ ] CSV output formatter
- [ ] Custom separators

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
2. **Type System**: How much type inference vs explicit typing?
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
- [ ] **Specify type coercion rules**: When and how automatic type conversion happens
- [ ] **Design error handling semantics**: Define how errors propagate through streams
- [ ] **Establish naming conventions**: Variable naming, function naming, constants

### Formal Specification
- [ ] **Design formal EBNF grammar for RUMPS DSL**
  - Complete lexical structure (tokens, keywords, operators)
  - Expression grammar (including $COLLECT streams)
  - Statement grammar (assignments, transactions, control flow)
  - Type annotations (if explicit typing is supported)
  - Comments and documentation syntax
- [ ] **Create language specification document**
  - Formal semantics for each operation
  - Memory model and execution model
  - Transaction semantics within streams
  - Concurrency guarantees

### Parser Implementation
- [ ] **Create prototype parser for basic $COLLECT operations**
  - Choose parsing approach (recursive descent, parser combinator, or parser generator)
  - Implement tokenizer/lexer
  - Parse basic $COLLECT with WHERE and SELECT
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
5. **Type Safe**: Catch errors at compile/parse time when possible
6. **Predictable**: No hidden side effects or surprising behavior
7. **Performant**: Optimize automatically where possible
8. **Debuggable**: Clear error messages and debugging tools

## Future Enhancements

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

Last Updated: 2025-11-25
