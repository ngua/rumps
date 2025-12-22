**NOTE**: When writing `*.rumps` scripts for integration testing, please use `train-case` casing for variables, functions, etc... Avoid other cases.

# Phase 3: User-Defined Types and Pattern Matching

This document tracks the third phase of implementing the RUMPS query language: user-defined sum types, structural object aliases, pattern matching with `MATCH`, regex, and higher-order collection operations.

**Prerequisites**: Phase 2 (operators, functions, closures) complete.

**Testing**: Each feature requires both unit tests and integration tests (`.rumps` script + `.expected` output in `tests/scripts/`).

**NOTE**: If integration tests are failing after modifications to parser, etc..., it may be due to outdated snapshots. Use `cargo insta` to fix

## Goals

1. Tuples (`(a, b, c)`)
2. Destructuring bindings (`LET (a, b) = ...`, `LET { x, y } = ...`)
3. User-defined sum types (`TYPE Status = Pending | Active`)
4. Structural object type aliases (`TYPE Patient = { name: String, age: Int }`)
5. Pattern matching (`MATCH`)
6. Built-in modules with collection operations (`Array.map`, `Array.filter`, `Object.keys`)
7. Range operator (`..`)
8. Spread operators (`...`)
9. Regex pattern matching (`MATCHES`, `/pattern/`)

## Existing Infrastructure

The following already exists and will be leveraged:

- `Value::Tagged(TypeExprId, u8, SmallVec<[ValueId; 4]>)` for sum type values
- `Value::Object(IndexMap<StringId, ValueId>)` for object values
- `TypeDef::Sum { name, type_params, variants }` for sum type definitions
- `TypeRegistry` with `register()`, `lookup()`, `lookup_variant()`
- `VariantDef { name, idx, arity }` for variant metadata
- `Expr::Variant(type_name, variant_name, args)` for constructing variants
- `TypePattern` enum for `IS` operator patterns
- Builtin types `Option` and `Result` already registered

## Phase 3 Tasks

### 0. Separate Field Access from Path Resolution

Currently `.` conflates two semantically different operations:
1. **Field access**: runtime operation on a *value* (`obj.field`)
2. **Path/namespace access**: compile-time name resolution (`Option.None`, future `Math.sin`)

This creates complexity in the interpreter and will get worse with modules. Fix by adding a name resolution phase.

#### Current State (Problem)

The parser emits `Expr::Field` for all `.` access, then the interpreter checks at runtime:
- Is this `TypeName.Variant`? → Create variant value
- Otherwise → Runtime field access on object

This is fragile: `ops.inc(5)` currently requires checking if `ops` is a type name at interpretation time.

#### Target State (Solution)

1. Parser emits `Expr::Field(base, name)` for ALL `.` access (no change)
2. Add name resolution pass between parsing and interpretation
3. Resolution converts qualified names to `Expr::Path` nodes:
   - `Option.None` → `Expr::Path(["Option", "None"])`
   - `Option.Some(x)` → `Expr::Variant("Option", "Some", [x])` (already exists)
4. `Expr::Field` remains purely for runtime field access on values

#### 0.1 AST Changes

- [x] Add `Expr::Path(SmallVec<[String; 2]>)` for resolved namespace paths
- [x] Keep `Expr::Field` for runtime field access only
- [x] Keep `Expr::Variant` for variant construction with args

#### 0.2 Name Resolution Pass

- [x] Add `resolve` module with `NameResolver` struct
- [x] Walk AST after parsing, before interpretation
- [x] For each `Expr::Field(base, name)`:
  - If `base` is `Expr::Var(type_name)` and `type_name` is a registered type:
    - If `name` is a zero-arity variant → convert to `Expr::Path([type_name, name])`
    - If followed by call with args → already handled by `Expr::Variant`
  - Otherwise → leave as `Expr::Field` (runtime field access)
- [x] For `Expr::Call` where callee is `Expr::Field`:
  - If matches `Type.Variant(args)` pattern → convert to `Expr::Variant`
  - Otherwise → leave as `Expr::Call` (method-like call on object field)

#### 0.3 Interpreter Changes

- [x] Remove type registry lookups from `field()` method
- [x] `field()` becomes purely runtime field access on `Value::Object`
- [x] Add `eval_path()` for `Expr::Path` nodes (lookup in registry, return variant value)
- [x] Simplify `call()` since variant detection moved to resolution

#### 0.4 Parser Cleanup

- [x] Remove uppercase check hack from `PostfixOp::Call` handling
- [x] Parser just emits `Expr::Field` and `Expr::Call`; resolution does the rest

#### 0.5 Future: Modules

This separation enables clean module support later:

```rumps
IMPORT Math

OUTPUT Math.sin(3.14)   ; Path resolves to module export
OUTPUT Math.PI          ; Path resolves to module constant

LET obj = { sin: x => x }
OUTPUT obj.sin(3.14)    ; Field access on object (different!)
```

Both use `.` syntax, but:
- `Math.sin` → `Expr::Path` (resolved at parse time)
- `obj.sin` → `Expr::Field` (resolved at runtime)

#### 0.6 Tests

- [x] Add resolution tests for type paths
- [x] Add resolution tests for field access (should remain `Expr::Field`)
- [x] Verify existing variant tests still pass
- [x] Verify object field closure tests still pass

---

### 1. Tuples

Fixed-size heterogeneous sequences. Unlike arrays, tuples can hold different types and have a known length at compile time.

```rumps
; Construction
LET pair = (1, "hello")
LET triple = (true, 42, "world")
LET nested = ((1, 2), (3, 4))

; Type annotation
LET point: (Int, Int) = (10, 20)
LET result: (Bool, String) = (true, "success")

; Access by index (0-based)
OUTPUT pair.0    ; 1
OUTPUT pair.1    ; "hello"
OUTPUT triple.2  ; "world"

; Destructuring in LET
LET (x, y) = point
OUTPUT x  ; 10

; In function signatures
FUN swap (p: (Int, Int)) -> (Int, Int) {
  (p.1, p.0)
}

; Returned from functions
FUN divmod (a: Int, b: Int) -> (Int, Int) {
  (a / b, a % b)
}

LET (quot, rem) = divmod(17, 5)
```

#### 1.1 AST

- [x] Add `Expr::Tuple(SmallVec<[ExprId; 4]>)` for tuple literals
- [x] Add `Expr::TupleIndex(ExprId, u32)` for tuple index access
- [x] Add `AstTypeExpr::Tuple(SmallVec<[AstTypeExprId; 4]>)` for tuple types

#### 1.2 Value

- [x] Add `Value::Tuple(TypeExprId, SmallVec<[ValueId; 4]>)` for runtime tuple values
- [x] Add `TypeExpr::Tuple(SmallVec<[TypeExprId; 4]>)` for tuple type expressions
- [x] Add `TypeId::TUPLE` constant

#### 1.3 Parser

- [x] Parse tuple literals: `(expr, expr, ...)` (must have at least 2 elements or trailing comma)
- [x] Distinguish from parenthesized expressions: `(expr)` is grouping, `(expr,)` or `(a, b)` is tuple
- [x] Parse tuple types: `(Type, Type, ...)`
- [x] Parse tuple index access: `expr.0`, `expr.1`, etc.

#### 1.4 Interpreter

- [x] Evaluate tuple construction
- [x] Implement index access for tuples (`.0`, `.1`, etc.)
- [x] Validate index is in bounds

#### 1.5 Tests

- [x] Add parser tests for tuple literals and types
- [x] Add interpreter tests for construction and access
- [x] Add integration test script (`46_tuples.rumps`)

---

### 2. Destructuring Bindings

Bind multiple variables at once by destructuring tuples, objects, and arrays. This is simpler than full `MATCH` pattern matching; it covers the common case of extracting values from composite types.

```rumps
; Tuple destructuring
LET (a, b) = (1, 2)
OUTPUT a  ; 1
OUTPUT b  ; 2

; Nested tuple destructuring
LET ((x, y), z) = ((1, 2), 3)

; Object destructuring (shorthand: field name = variable name)
LET { name, age } = { name: "Alice", age: 30, extra: true }
OUTPUT name  ; "Alice"
OUTPUT age   ; 30

; Object destructuring with rename
LET { name: n, age: a } = { name: "Bob", age: 25 }
OUTPUT n  ; "Bob"

; Array destructuring (exact match)
LET [first, second, third] = [1, 2, 3]
OUTPUT first   ; 1
OUTPUT second  ; 2

; Array destructuring with ignored rest
LET [head, ..] = [1, 2, 3, 4]
OUTPUT head  ; 1

; Array destructuring with bound rest
LET [first, ...tail] = [1, 2, 3, 4]
OUTPUT first  ; 1
OUTPUT tail   ; [2, 3, 4]

; Combining with functions
FUN divmod (a: Int, b: Int) -> (Int, Int) { (a / b, a % b) }
LET (quot, rem) = divmod(17, 5)
OUTPUT quot  ; 3
OUTPUT rem   ; 2
```

#### 2.1 AST

- [x] Add `RestPattern` and `BindingPattern` enums:
  ```rust
  enum RestPattern {
      Ignore,        // `..`
      Bind(String),  // `...name`
  }

  enum BindingPattern {
      /// Simple variable: `x`
      Var(String),
      /// Tuple: `(a, b, c)`
      Tuple(Vec<BindingPattern>),
      /// Object: `{ name, age }` or `{ name: n, age: a }`
      Object(Vec<(String, BindingPattern)>),
      /// Array: `[a, b]` (exact), `[a, ..]` (ignore rest), `[a, ...rest]` (bind rest)
      Array(Vec<BindingPattern>, Option<RestPattern>),
      /// Wildcard: `_` (ignore this position)
      Wildcard,
  }
  ```
- [x] Modify `Stmt::Let` to use `BindingPattern` instead of just `String`

#### 2.2 Parser

- [x] Parse `LET (a, b) = expr` as tuple destructuring
- [x] Parse `LET { name, age } = expr` as object destructuring (shorthand)
- [x] Parse `LET { name: n } = expr` as object destructuring (with rename)
- [x] Parse `LET [a, b] = expr` as array destructuring (exact match)
- [x] Parse `LET [head, ..] = expr` as array destructuring (ignore rest)
- [x] Parse `LET [head, ...tail] = expr` as array destructuring (bind rest)
- [x] Parse `LET _ = expr` as wildcard (evaluate but discard)
- [x] Support nested patterns: `LET ((a, b), c) = ...`

#### 2.3 Interpreter

- [x] Implement `destructure(pattern, value) -> Result<()>`:
  - `Var(name)` -> bind name to entire value
  - `Tuple(pats)` -> match `Value::Tuple`, recursively destructure elements
  - `Object(fields)` -> match `Value::Object`, extract named fields
  - `Array(pats, rest)` -> match `Value::Array`, bind prefix and optional rest
  - `Wildcard` -> return empty bindings (discard)
- [x] Error if structure doesn't match (e.g., tuple size mismatch)
- [x] Bind all extracted variables in scope

#### 2.4 Tests

- [x] Add parser tests for destructuring patterns
- [x] Add interpreter tests for all pattern types
- [x] Add integration test scripts:
  - `47_destructuring.rumps` (happy paths)
  - `48_destructure_errors.rumps` through `52_destructure_rest_err.rumps` (error cases)

---

### 3. User-Defined Sum Types (`TYPE ... = Variant | ...`)

Allow users to define their own sum types (tagged unions / ADTs).

```rumps
; Simple enumeration (no payloads)
TYPE Status = Pending | Active | Completed | Failed

; With payloads
TYPE Event =
  Click(Int, Int)
  | KeyPress(Char)
  | Resize(Int, Int)

; Parameterized (generic) sum types
TYPE Either[L, R] = Left(L) | Right(R)
```

**Usage:**
```rumps
LET s = Status.Pending
LET e = Event.Click(100, 200)
LET val = Either.Left("error")

IF s IS Status.Active {
  OUTPUT "Active!"
}
```

#### 2.1 Lexer

- [x] Add `Token::Type` keyword (`TYPE`)
- [x] Add `Token::SinglePipe` (`|`) for variant separator
  - Note: Already have `Token::PipePipe` for `||`; added `Token::SinglePipe` for single pipe

#### 2.2 AST

- [x] Add `Stmt::Type` for type declarations:
  ```rust
  Stmt::Type {
      name: String,
      type_params: SmallVec<[String; 2]>,  // e.g., `[L, R]` for `Either[L, R]`
      def: TypeDefAst,
  }
  ```
- [x] Add `TypeDefAst` enum:
  ```rust
  enum TypeDefAst {
      Sum(SmallVec<[VariantAst; 4]>),
      Struct(Vec<(String, AstTypeExprId)>),  // for Phase 3.2
  }
  ```
- [x] Add `VariantAst`:
  ```rust
  struct VariantAst {
      name: String,
      payloads: SmallVec<[AstTypeExprId; 2]>,  // payload types
  }
  ```

#### 2.3 Parser

- [x] Parse `TYPE Name = Variant1 | Variant2(T) | ...`
- [x] Handle type parameters: `TYPE Name[T, U] = ...`
- [x] Allow newlines between variants (indentation-based)
- [x] Variants can have zero or more typed payloads

#### 2.4 Interpreter

- [x] Process `Stmt::Type` to register in `TypeRegistry`:
  - Intern all names
  - Create `VariantDef` for each variant (auto-assign indices)
  - Register as `TypeDef::Sum`
- [x] User-defined types work with existing `Expr::Variant` evaluation
- [x] User-defined types work with existing `IS` patterns

#### 2.5 Tests

- [x] Add lexer tests for `TYPE` and `|`
- [x] Add parser tests for sum type declarations
- [x] Add interpreter tests for type registration and variant construction
- [x] Add integration test script (`53_user_sum_types.rumps`)

---

### 4. Structural Object Type Aliases (`TYPE ... = { ... }`)

Named aliases for structural object types. These map to `Value::Object` at runtime but provide:
- Named type for documentation and readability
- Optional runtime validation of field presence/types
- Direct field access (no `MATCH` required)

```rumps
TYPE Patient = {
  id: Int,
  name: String,
  age: Int,
  active: Bool
}

TYPE Address = {
  street: String,
  city: String,
  zip: String
}
```

**Usage:**
```rumps
; Construct using object literal (runtime validates against type)
LET p: Patient = { id: 1, name: "Alice", age: 30, active: true }

; Direct field access (NOT requiring MATCH)
OUTPUT p.name    ; "Alice"
OUTPUT p.age     ; 30

; In function signatures
FUN greet (p: Patient) -> String {
  "Hello, " ++ p.name ++ "!"
}

; Nested types
TYPE PatientRecord = {
  patient: Patient,
  address: Address
}

LET rec: PatientRecord = {
  patient: { id: 1, name: "Bob", age: 25, active: true },
  address: { street: "123 Main", city: "NYC", zip: "10001" }
}

OUTPUT rec.patient.name  ; "Bob"
```

#### 4.1 TypeDef Extension

- [x] Add `TypeDef::Struct` variant:
  ```rust
  TypeDef::Struct {
      name: StringId,
      fields: IndexMap<StringId, TypeExprId>,  // field name -> type
  }
  ```

#### 4.2 Parser

- [x] Parse `TYPE Name = { field: Type, ... }`
- [x] Reuse existing object literal parsing for field definitions
- [x] Handle newlines inside struct definition

#### 4.3 Interpreter

- [x] Register `TypeDef::Struct` in `TypeRegistry`
- [x] When assigning to typed variable (`LET x: TypeName = ...`):
  - If type is a struct, validate object **has required fields**
  - Additional fields are OK (i.e. extensible-record style)
- [x] Field access on objects with struct type works via existing `Expr::Field`

#### 4.4 Type Validation Strategy

Structural typing with optional nominal wrapper:

```rumps
; These are equivalent at runtime (both are Value::Object)
LET p1: Patient = { id: 1, name: "X", age: 20, active: true }
LET p2 = { id: 1, name: "X", age: 20, active: true }

; But p1 has type information for validation/documentation
```

**Decision**: Struct aliases are purely for documentation and optional validation. At runtime, they are `Value::Object`. The type annotation triggers validation at assignment time.

#### 4.5 Tests

- [x] Add parser tests for struct type declarations
- [x] Add interpreter tests for struct registration and validation
- [x] Add integration test script (`56_struct_types.rumps`)

---

### 5. Pattern Matching (`MATCH`)

The `MATCH` expression enables exhaustive, type-safe branching on values. Primary way to destructure sum types.

```rumps
MATCH <scrutinee> {
  <pattern> => <expr>
  <pattern> => <expr>
  ...
}
```

`MATCH` is an **expression** that evaluates to a value (like `IF`):

```rumps
; Bind result to a variable
LET label = MATCH status {
  Status.Pending => { "waiting" }
  Status.Active => { "in progress" }
  Status.Completed => { "done" }
  Status.Failed => { "error" }
}
OUTPUT label

; Use directly in expressions
OUTPUT "Status: " ++ MATCH s {
  Status.Pending => { "pending" }
  _ => { "other" }
}

; Return from function
FUN describe (opt: Option[Int]) -> String {
  MATCH opt {
    Option.Some(n) => { "Got " ++ n }
    Option.None => { "Nothing" }
  }
}

; Nested in other expressions
LET doubled = MATCH get-value() {
  Option.Some(n) => { n * 2 }
  Option.None => { 0 }
} + 10
```

#### 5.1 Matching Sum Types

```rumps
LET status = get-status()

MATCH status {
  Status.Pending => { OUTPUT "Waiting..." }
  Status.Active => { OUTPUT "In progress" }
  Status.Completed => { OUTPUT "Done!" }
  Status.Failed => { OUTPUT "Error occurred" }
}

; With payload binding
MATCH event {
  Event.Click(x, y) => { OUTPUT "Click at " ++ x ++ ", " ++ y }
  Event.KeyPress(c) => { OUTPUT "Key: " ++ c }
  Event.Resize(w, h) => { resize-window(w, h) }
}
```

#### 5.2 Matching Option and Result

```rumps
LET name = GET ^PATIENT(id, "NAME")

MATCH name {
  Option.Some(n) => { OUTPUT "Patient: " ++ n }
  Option.None => { OUTPUT "Unknown patient" }
}

MATCH try-parse(input) {
  Result.Ok(n) => { n * 2 }
  Result.Err(e) => {
    OUTPUT TO ERROR "Parse failed: " ++ e
    0
  }
}
```

#### 5.3 Matching Literals and Wildcards

```rumps
MATCH count {
  0 => { "none" }
  1 => { "one" }
  n => { "many: " ++ n }  ; bind to variable
}

MATCH cmd {
  "quit" => { exit() }
  "help" => { show-help() }
  _ => { OUTPUT "Unknown command" }  ; wildcard
}
```

#### 5.4 Pattern Guards

```rumps
MATCH n {
  x IF x > 100 => { "large" }
  x IF x > 0 => { "positive" }
  0 => { "zero" }
  _ => { "negative" }
}
```

#### 5.5 Object Patterns

Destructure objects by field name:

```rumps
MATCH person {
  { name, age } => { OUTPUT name ++ " is " ++ age }
}

; With nested patterns
MATCH record {
  { patient: { name }, address: { city } } => {
    OUTPUT name ++ " lives in " ++ city
  }
}

; Partial matching (only extract some fields)
MATCH user {
  { name, role: "admin" } => { OUTPUT name ++ " is admin" }
  { name } => { OUTPUT name ++ " is regular user" }
}

; With wildcards
MATCH event {
  { type: "click", x, y } => { handle-click(x, y) }
  { type: "key", key } => { handle-key(key) }
  _ => { OUTPUT "unknown event" }
}
```

#### 5.6 Tuple Patterns

```rumps
MATCH pair {
  (0, y) => { "x is zero, y is " ++ y }
  (x, 0) => { "y is zero, x is " ++ x }
  (x, y) => { "x=" ++ x ++ ", y=" ++ y }
}

; Nested tuple patterns
MATCH nested {
  ((a, b), (c, d)) => { a + b + c + d }
}

; With literals
MATCH result {
  (true, msg) => { OUTPUT "Success: " ++ msg }
  (false, err) => { OUTPUT "Error: " ++ err }
}
```

#### 5.7 Nested Patterns

```rumps
MATCH nested {
  Option.Some(Option.Some(v)) => { "doubly wrapped: " ++ v }
  Option.Some(Option.None) => { "outer Some, inner None" }
  Option.None => { "outer None" }
}

; Variant containing tuple
MATCH result {
  Result.Ok((a, b)) => { process(a, b) }
  Result.Err(e) => { OUTPUT "Error: " ++ e }
}

; Variant containing object
MATCH result {
  Result.Ok({ data, status }) => { process(data, status) }
  Result.Err({ code, message }) => { OUTPUT "Error " ++ code ++ ": " ++ message }
}
```

#### 5.8 Implementation

##### Lexer

- [x] Add `Token::Match` keyword
- [x] `Token::FatArrow` (`=>`) should already exist from Phase 2 closures

##### AST

- [x] Add `Expr::Match(ExprId, Vec<MatchArm>)`:
  ```rust
  struct MatchArm {
      pattern: Pattern,
      guard: Option<ExprId>,  // IF condition
      body: ExprId,
  }
  ```
- [x] Add `Pattern` enum:
  ```rust
  enum Pattern {
      /// Wildcard: `_`
      Wildcard,

      /// Variable binding: `x`, `name`
      Var(String),

      /// Literal: `0`, `"hello"`, `true`
      Literal(Literal),

      /// Variant with bindings: `Option.Some(x)`, `Result.Err(e)`
      Variant(String, String, SmallVec<[Pattern; 2]>),

      /// Object destructuring: `{ name, age }`, `{ name, role: "admin" }`
      /// Fields are (field_name, pattern); shorthand `{ name }` desugars to `{ name: name }`
      Object(SmallVec<[(String, Pattern); 8]>),

      /// Tuple: `(a, b, c)`
      Tuple(SmallVec<[Pattern; 4]>),
  }
  ```

##### Parser

- [x] Parse `MATCH expr { arm... }`
- [x] Parse patterns: wildcards, variables, literals, variants, objects, tuples
- [x] Parse object patterns: `{ field }` (shorthand), `{ field: pattern }` (with nested pattern)
- [x] Parse tuple patterns: `(pat, pat, ...)`
- [x] Parse optional guards: `pattern IF cond => body`
- [x] Handle newlines between arms

##### Interpreter

- [x] Evaluate scrutinee once
- [x] Try each arm in order:
  - Attempt to match pattern against value
  - If pattern matches, bind variables to scope
  - If guard exists, evaluate it; if false, continue to next arm
  - If guard passes (or no guard), evaluate body in scope with bindings
- [x] Return first matching arm's body value
- [x] Error if no arm matches (non-exhaustive)

##### Pattern Matching Algorithm

```rust
fn matches(&self, pat: &Pattern, val: &Value) -> Option<Vec<(StringId, ValueId)>> {
    match (pat, val) {
        (Pattern::Wildcard, _) => Some(vec![]),
        (Pattern::Var(name), _) => Some(vec![(intern(name), val_id)]),
        (Pattern::Literal(lit), val) => {
            if lit_matches(lit, val) { Some(vec![]) } else { None }
        }
        (Pattern::Variant(ty, var, sub_pats), Value::Tagged(ty_expr, idx, payloads)) => {
            // Check type and variant match
            // Recursively match sub-patterns against payloads
            // Collect all bindings
        }
        (Pattern::Object(fields), Value::Object(obj)) => {
            // For each (field_name, sub_pattern) in fields:
            //   - Look up field_name in obj; fail if missing
            //   - Recursively match sub_pattern against field value
            //   - Collect all bindings
            // Object may have extra fields (partial match OK)
        }
        (Pattern::Tuple(pats), Value::Tuple(vals)) => {
            // Check lengths match
            // Recursively match each sub-pattern against corresponding value
            // Collect all bindings
        }
        _ => None,
    }
}
```

##### Tests

- [x] Add lexer tests for `MATCH`
- [x] Add parser tests for match expressions and patterns
- [x] Add interpreter tests for pattern matching
- [x] Add integration test script (`62_match.rumps`)
  - [x] Make sure to add items that bind the match to a `LET`, i.e. `LET r = MATCH ...`

---

### 6. Built-in Modules and Collection Operations

With function types and closures in place from Phase 2, collection operations are straightforward. However, they belong in **modules**, not as bare case-insensitive primitives.

```rumps
; Module-qualified function calls (case-sensitive)
Array.map(x => x * 2, [1, 2, 3])
Array.filter(x => x > 2, [1, 2, 3, 4])
Array.reduce((acc, x) => acc + x, 0, [1, 2, 3])

; Object utilities
Object.keys({ a: 1, b: 2 })      ; ["a", "b"]
Object.values({ a: 1, b: 2 })    ; Result.Ok([1, 2])
Object.entries({ a: 1, b: 2 })   ; Result.Ok([("a", 1), ("b", 2)])
Object.from-entries([("a", 1)])  ; { a: 1 }

; Future modules
Math.sqrt(16)    ; 4.0
String.split("a,b", ",")  ; ["a", "b"]
```

Type signatures:
```rumps
; Array.map: ((T) -> U, Array[T]) -> Array[U]
; Array.filter: ((T) -> Bool, Array[T]) -> Array[T]
; Array.reduce: ((A, T) -> A, A, Array[T]) -> A
; Object.keys: Object -> Array[String]
; Object.values: Object -> Result[Array[T], String]
```

#### 6.1 Design: Module-Qualified Functions

**Rationale**: The previous design used case-insensitive bare primitives (`KEYS`, `MAP`, etc.), which created an awkward hybrid:
- Keywords (`LET`, `MATCH`) are case-insensitive with special syntax
- Type paths (`Option.Some`) use `.` notation and are case-sensitive
- Primitives were case-insensitive but called like functions; inconsistent

The new design uses **module-qualified functions**:
- Consistent `.` notation: `Object.keys`, `Array.map`, `Math.sqrt`
- Case-sensitive (matches type paths): `Object.keys` works, `object.KEYS` does not
- Same resolution mechanism as type paths (extend `resolve.rs`)
- Clean separation: keywords are special, everything else uses modules

**Built-in modules** (provided by the runtime):
- `Object`: `keys`, `values`, `entries`, `from-entries`
- `Array`: `map`, `filter`, `reduce`, `fold`, `take`, `drop`, etc.
- `Math`: `sqrt`, `sin`, `cos`, `abs`, `floor`, `ceil`, etc. (future)
- `String`: `split`, `trim`, `starts_with`, `ends_with`, etc. (future)

**Future: User-defined modules** (not in Phase 3):
```rumps
; Explicit MODULE blocks
MODULE Utils {
  FUN double (x: Int) -> Int { x * 2 }
  FUN triple (x: Int) -> Int { x * 3 }
}

OUTPUT Utils.double(5)  ; 10
```

**Nested modules** (infrastructure in place):

The module system supports nested modules via the `Module.submodules` field.
This enables paths like `Math.Trig.sin(x)` for future built-in or user-defined
modules. The resolution pass handles paths of arbitrary length.

#### 6.2 Implementation

##### 6.2.1 Module Infrastructure

- [x] Add `Module` struct with nested module support:
  ```rust
  struct Module {
      functions: HashMap<String, PrimFn>,
      submodules: HashMap<String, Module>,
  }
  ```
- [x] Replace `Environment.primitives: HashMap<String, PrimFn>` with:
  ```rust
  modules: HashMap<String, Module>,  // "Object" -> Module { ... }
  ```
- [x] Add `Environment::get_module_fn(&self, path: &[&str]) -> Option<&PrimFn>`
- [x] Add `Environment::module_fn_exists(&self, path: &[&str]) -> bool`
- [x] Add `BUILTIN_MODULE_NAMES` constant as single source of truth
- [x] Register built-in modules in `Environment::new()`:
  - `Object` module with `keys`, `values`, `entries`, `from-entries`
  - `Array` module (initially empty, populated in 6.2.5)

##### 6.2.2 Name Resolution for Modules

Extend `resolve.rs` to handle module paths:

- [x] Add `collect_path_segments()` to handle nested field chains
- [x] Recognize `Expr::Field(Var(module), fn_name)` and nested `Field` chains
- [x] If path starts with a known module name:
  - Convert to `Expr::Path([module, fn_name, ...])` (reuses existing Path node)
- [x] If first segment is a type name, existing variant resolution applies
- [x] If neither, leave as `Expr::Field` for runtime field access

**Resolution flow**:
```
Object.keys      → Expr::Path(["Object", "keys"])
Math.Trig.sin    → Expr::Path(["Math", "Trig", "sin"])
Option.None      → Expr::Variant("Option", "None", [])
obj.field        → Expr::Field (unchanged)
```

**First-class module functions**: Module functions can be used as values:
```rumps
LET keys-fn = Object.keys       ; Value::ModuleFn { path: ["Object", "keys"] }
OUTPUT obj |> keys-fn           ; Pipeline works
OUTPUT keys-fn({ a: 1, b: 2 })  ; Direct call works
```

##### 6.2.3 Interpreter Changes

- [x] Add `Value::ModuleFn { path: SmallVec<[StringId; 4]> }` for module function references
- [x] Add `interpreter/modules.rs` for path evaluation
- [x] `Expr::Path` evaluates to `Value::ModuleFn` when path refers to a module function
- [x] `Value::ModuleFn` can be called directly or used in pipelines
- [x] Add `invoke_module_fn()` to handle module function calls
- [x] Remove old flat primitive lookup from function call handling

##### 6.2.4 Object Module Functions

Migrate existing primitives to `Object` module:

- [x] `Object.keys`: `Object -> Array[String]`
  - Return array of field names in iteration order
- [x] `Object.values`: `Object -> Result[Array[T], String]`
  - Return `Result.Ok(array)` if homogeneous, `Result.Err(msg)` otherwise
- [x] `Object.entries`: `Object -> Result[Array[(String, T)], String]`
  - Return `Result.Ok(array)` of tuples if homogeneous
- [x] `Object.from-entries`: `Array[(String, T)] -> Object`
  - Construct object from tuples; later entries override
- [x] Update tests to use module-qualified syntax
- [x] Add integration test script (`66_object_module.rumps`)

##### 6.2.5 Array Module Functions with HoFs

- [x] `Array.map`: `((T) -> U, Array[T]) -> Array[U]`
  - Apply function to each element, return new array
  - Validates homogeneity during evaluation (not post-traversal)
- [x] `Array.filter`: `((T) -> Bool, Array[T]) -> Array[T]`
  - Keep elements where predicate returns truthy
- [x] `Array.reduce`: `((A, T) -> A, A, Array[T]) -> A`
  - Fold left: `reducer(reducer(init, arr[0]), arr[1])...`
- [x] Add interpreter tests for Array module functions
- [x] Add integration test script (`71_array_module.rumps`)

**Implementation note**: Array functions are higher-order (they invoke closures) and are implemented directly on `Interpreter` in `call.rs` rather than as `PrimFn` functions. Placeholder functions are registered in the environment so `module_fn_exists` returns true for name resolution; the placeholders are intercepted in `invoke_module_fn` and dispatch to the interpreter methods.

##### 6.3 Other builtins for std lib

Implement the following in primitives.rs; **NOTE**: make sure these are registered in `Environment::register_builtins` under the correct module!:

For e.g. getting `Array` values, use e.g. `ValueArena::get_array` to avoid unnecessary cloning. Similar `ValueArena::get_*` functions may be needed e.g. for `String.*` primitives

**NOTE**: For each new module implementation, add a corresponding `.rumps` script and snapshot testing **all** primitives implemented.

- [x] `Array` — More array operations
  | Function                 | Description                  | Example                                  |
  |--------------------------|------------------------------|------------------------------------------|
  | `Array.length(arr)`      | Get length                   | `Array.length([1,2,3])` → `3`            |
  | `Array.push(arr, val)`   | Append element               | `Array.push([1,2], 3)` → `[1,2,3]`       |
  | `Array.pop(arr)`         | Remove last                  | `Array.pop([1,2,3])` → `[1,2]`           |
  | `Array.head(arr)`        | First element (if it exists) | `Array.head([1,2,3])` → `Option.Some(1)` |
  |                          |                              | `Array.head([])` → `Option.None`         |
  | `Array.tail(arr)`        | All but first                | `Array.tail([1,2,3])` → `[2,3]`          |
  | `Array.reverse(arr)`     | Reverse order                | `Array.reverse([1,2,3])` → `[3,2,1]`     |
  | `Array.sort(arr)`        | Sort ascending               | `Array.sort([3,1,2])` → `[1,2,3]`        |
  | `Array.slice(arr, i, j)` | Subarray                     | `Array.slice([1,2,3,4], 1, 3)` → `[2,3]` |
  | `Array.contains(arr, v)` | Check membership             | `Array.contains([1,2,3], 2)` → `true`    |
  | `Array.concat(a, b)`     | Concatenate                  | `Array.concat([1], [2])` → `[1,2]`       |
  **NOTE**: Some existing `Array` module primitives are _not_ implemented in primitives.rs as they need access to HoF evaluation (e.g. `Array.map`, `Array.filter`, etc...). The primitives above can be directly implemented on `Prim`, however.
  **NOTE**: `concat` MUST check that both arrays have the same element type! Use `base_type_of` to avoid cloning entire arrays
  **Integration test**: `72_array_primitives.rumps`

- [ ] `String` — String operations
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

- [ ] `Math` — Mathematical operations
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

- [ ] `Random` — Generating random values
  | Function          | Description           | Example                     |
  |-------------------|-----------------------|-----------------------------|
  | `Random.random()` | Random 0-1 (as float) | `Random.random()` → `0.xxx` |

- [ ] `Option` — Option operations
  | Function                 | Description          | Example                                   |
  |--------------------------|----------------------|-------------------------------------------|
  | `Option.unwrap-or(o, d)` | Get value or default | `Option.unwrap-or(None, 0)` → `0`         |
  | `Option.map(o, f)`       | Transform if Some    | `Option.map(Some(1), double)` → `Some(2)` |
  **NOTE**: `Option.unwrap` is not needed; use the `!` postfix operator (already implemented)
  **NOTE**: `Option.{is-none, is-some}` is not needed; use the `IS` primitive

- [ ] `Result` — Result operations
  | Function                 | Description          | Example                                   |
  |--------------------------|----------------------|-------------------------------------------|
  | `Result.unwrap-or(r, d)` | Get value or default | `Result.unwrap-or(Err("x"), 0)` → `0`     |
  | `Result.map(r, f)`       | Transform if Ok      | `Result.map(Ok(1), double)` → `Ok(2)`     |
  | `Result.map-err(r, f)`   | Transform if Err     | `Result.map-err(Err("x"), upper)` → `...` |
  **NOTE**: `Result.unwrap` is not needed; use the `!` postfix operator (already implemented)
  **NOTE**: `Result.{is-none, is-some}` is not needed; use the `IS` primitive

---

### 7. Range Operator (`..`)

Creates a lazy range of integers.

```rumps
1..10           ; range from 1 to 10 (inclusive? exclusive? TBD)
0..n            ; range from 0 to n
Array.map(x => x * x, 1..100)
```

- [ ] Add `Token::DotDot` to lexer
- [ ] Add `Expr::Range(ExprId, ExprId)` to AST (start, end)
- [ ] Add `Value::Range { start: i64, end: i64 }` variant
- [ ] Decide: inclusive (`1..=10`) vs exclusive (`1..10`) end
  - Recommend: `..` is exclusive (like Rust), `..=` is inclusive
- [ ] Implement range creation in interpreter
- [ ] Implement iteration protocol for ranges (for use with pipeline/closures)
- [ ] Add unit tests
- [ ] Add integration test script

#### 7.1 Range Semantics

Ranges use Rust-style exclusive end by default:

```rumps
1..5      ; 1, 2, 3, 4 (exclusive end)
1..=5     ; 1, 2, 3, 4, 5 (inclusive end)
```

Ranges are lazy; they don't allocate an array. They're iterable values that work with `|>` and collection operations.

Range precedence is higher than comparison but lower than additive:

```rumps
1..n + 1       ; 1..(n + 1), not (1..n) + 1
0..len - 1     ; 0..(len - 1)
```

---

### 8. Spread Operators (`...`)

Spread syntax for arrays and objects.

```rumps
; Array spread
LET arr1 = [1, 2, 3]
LET arr2 = [4, 5, 6]
LET combined = [...arr1, ...arr2]  ; [1, 2, 3, 4, 5, 6]

; Prepend/append
LET with_zero = [0, ...arr1]       ; [0, 1, 2, 3]
LET with_four = [...arr1, 4]       ; [1, 2, 3, 4]

; Object spread
LET base = { name: "Alice", age: 30 }
LET updated = { ...base, age: 31 }  ; { name: "Alice", age: 31 }

; Merge objects (later spreads override earlier)
LET defaults = { color: "red", size: "medium" }
LET user = { size: "large" }
LET config = { ...defaults, ...user }  ; { color: "red", size: "large" }
```

#### 8.1 Lexer

- [x] Add `Token::DotDotDot` (`...`) for spread operator

*Note: Already implemented as part of destructuring bindings (section 2) for array rest patterns.*

#### 8.2 AST

- [ ] Add `Expr::Spread(ExprId)` for spread expressions
- [ ] Modify `Expr::Array` to allow spread elements
- [ ] Modify `Expr::Object` to allow spread entries

#### 8.3 Parser

- [ ] Parse `...expr` inside array literals
- [ ] Parse `...expr` inside object literals
- [ ] Spread only valid inside array/object literals (not standalone)

#### 8.4 Interpreter

- [ ] Array spread: iterate source array, append elements to result
- [ ] Object spread: iterate source object entries, insert into result
- [ ] Later entries override earlier ones for objects

#### 8.5 Tests

- [ ] Add lexer tests for `...`
- [ ] Add parser tests for spread in arrays and objects
- [ ] Add interpreter tests
- [ ] Add integration test script (`XX_spread.rumps`)

---

### 9. Regex Pattern Matching (`MATCHES`)

Pattern matching with regex literals.

```rumps
IF email MATCHES /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

IF ssn MATCHES /^\d{3}-\d{2}-\d{4}$/ {
  OUTPUT "Valid SSN format"
}

; Negation via NOT
IF NOT (input MATCHES /[<>]/) {
  OUTPUT "No angle brackets"
}
```

#### 9.1 Lexer

- [ ] Add regex literal support (`/pattern/`)
  - Handle escape sequences (`\/`, `\\`)
  - Regex ends at unescaped `/`
- [ ] Add `Token::Regex(String)` for regex literals
- [ ] Add `Token::Matches` keyword

#### 9.2 AST

- [ ] Add `Expr::Matches(ExprId, String)` (value, pattern)

#### 9.3 Interpreter

- [ ] Add `regex` crate dependency
- [ ] Compile regex on first use (cache compiled patterns)
- [ ] Evaluate: coerce left to string, test against regex, return `Bool`

#### 9.4 Tests

- [ ] Add lexer tests for regex literals
- [ ] Add parser tests
- [ ] Add interpreter tests
- [ ] Add integration test script (`XX_regex.rumps`)

**Deferred regex features** (for later):
- Named capture groups (`(?<name>...)`) and `Match.name` access
- Regex flags (`/pattern/i`, `/pattern/m`)

---

## Design Decisions

### Sum Types Use Existing Infrastructure

User-defined sum types leverage:
- `TypeDef::Sum` for registration
- `Value::Tagged` for runtime representation
- `Expr::Variant` for construction
- Existing `IS` patterns for simple checks

The `TYPE` statement simply registers a new sum type; variant construction and pattern matching use existing mechanisms.

### Struct Types Are Object Aliases

`TYPE Foo = { ... }` creates a named alias for a structural object type:
- At runtime, values are `Value::Object`
- Type annotation triggers validation on assignment
- No wrapper; direct field access works
- Purely additive; untyped objects remain valid

### MATCH Is Exhaustive

`MATCH` should cover all cases. The interpreter will error if no arm matches. Future: add exhaustiveness checking at parse time for known sum types.

### Pattern Binding Scope

Variables bound in patterns are only visible in that arm's body:

```rumps
MATCH opt {
  Option.Some(x) => { OUTPUT x }  ; x visible here
  Option.None => { OUTPUT "none" }  ; x NOT visible here
}
OUTPUT x  ; ERROR: x not in scope
```

### MATCH vs IS

- Use `IS` for simple boolean checks, especially with `IF`
- Use `MATCH` for multi-way branching and complex destructuring

```rumps
; Simple check: use `IS`
IF x IS Option.Some(v) {
  OUTPUT v
}

; Multi-way: use MATCH
MATCH result {
  Result.Ok(v) => { process(v) }
  Result.Err(e) => { handle(e) }
}
```

---

## Success Criteria

The following should work:

```rumps
; Tuples
LET point = (10, 20)
OUTPUT point.0  ; 10
OUTPUT point.1  ; 20

LET (x, y) = point
OUTPUT x  ; 10

FUN divmod (a: Int, b: Int) -> (Int, Int) {
  (a / b, a % b)
}
LET (quot, rem) = divmod(17, 5)
OUTPUT quot  ; 3
OUTPUT rem   ; 2

; MATCH on tuples
MATCH point {
  (0, y) => { OUTPUT "On y-axis at " ++ y }
  (x, 0) => { OUTPUT "On x-axis at " ++ x }
  (x, y) => { OUTPUT "At " ++ x ++ ", " ++ y }
}

; User-defined sum type
TYPE Status =
  Pending
  | InProgress(String)
  | Completed
  | Failed(String)

LET s = Status.InProgress("step 1")

IF s IS Status.InProgress(msg) {
  OUTPUT "Working: " ++ msg
}

; Struct type alias
TYPE Person = {
  name: String,
  age: Int
}

LET p: Person = { name: "Alice", age: 30 }
OUTPUT p.name  ; "Alice" (direct access, no MATCH)

; MATCH on sum type
MATCH s {
  Status.Pending => { OUTPUT "Waiting" }
  Status.InProgress(msg) => { OUTPUT "Progress: " ++ msg }
  Status.Completed => { OUTPUT "Done" }
  Status.Failed(err) => { OUTPUT "Error: " ++ err }
}

; MATCH with guards
LET n = 42
MATCH n {
  x IF x > 100 => { "large" }
  x IF x > 0 => { "positive" }
  0 => { "zero" }
  _ => { "negative" }
}

; MATCH on objects (destructuring)
LET user = { name: "Bob", role: "admin", id: 42 }
MATCH user {
  { name, role: "admin" } => { OUTPUT name ++ " is an admin" }
  { name } => { OUTPUT name ++ " is a regular user" }
}

; Regex matching (MATCHES is a keyword)
LET email = "user@example.com"
IF email MATCHES /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

; Collection operations (module-qualified functions)
LET doubled = Array.map(x => x * 2, [1, 2, 3])
OUTPUT doubled  ; [2, 4, 6]

LET evens = Array.filter(x => x % 2 == 0, [1, 2, 3, 4])
OUTPUT evens  ; [2, 4]

LET sum = Array.reduce((acc, x) => acc + x, 0, [1, 2, 3, 4])
OUTPUT sum  ; 10

; Object utilities
OUTPUT Object.keys({ a: 1, b: 2 })  ; ["a", "b"]
OUTPUT { a: 1 } |> Object.keys      ; Pipeline with module fn

; Ranges
LET r = 1..5  ; [1, 2, 3, 4] (exclusive end)
LET squares = Array.map(x => x * x, 1..=5)  ; [1, 4, 9, 16, 25]

; Spread operators
LET arr1 = [1, 2, 3]
LET arr2 = [4, 5, 6]
LET combined = [...arr1, ...arr2]
OUTPUT combined  ; [1, 2, 3, 4, 5, 6]

LET base = { name: "Alice", age: 30 }
LET updated = { ...base, age: 31 }
OUTPUT updated.age  ; 31
```

---

### Implemented: Postfix `!` Unwrap Operator

Postfix `!` operator for unwrapping `Option` and `Result` values was implemented outside the planned phases. This provides a convenient shorthand for users who prefer runtime errors over explicit error handling.

```rumps
LET val = Option.Some(42)
OUTPUT val!              ; 42

LET ok = Result.Ok("success")
OUTPUT ok!               ; success

LET entries = Object.entries({ a: 1 })!
OUTPUT entries           ; [ (a, 1) ]
```

Semantics:
- `Option.Some(v)!` -> `v`
- `Option.None!` -> runtime error: "cannot unwrap Option.None"
- `Result.Ok(v)!` -> `v`
- `Result.Err(e)!` -> runtime error: "unwrap failed: <stringified e>"
- Other types -> type error

Integration tests: `67_unwrap.rumps`, `68_unwrap_none_err.rumps`,
`69_unwrap_err.rumps`, `70_unwrap_type_err.rumps`
