# Phase 2: Operators, Functions, and Patterns

This document tracks the second phase of implementing the RUMPS query language: operators, functions, closures, ranges, and regex pattern matching.

**Prerequisites**: Phase 1 (infrastructure) complete.

**Testing**: Each feature requires both unit tests and integration tests (`.rumps` script + `.expected` output in `tests/scripts/`).

**NOTE**: If integration tests are failing after modifications to parser, etc..., it may be due to outdated snapshots. Use `cargo insta` to fix

## Goals

1. Coalesce and optional chaining (`??`, `?.`)
2. Runtime type checking and casting (`is`, `as`)
3. Arithmetic power operator (`**`)
4. Function types (`(Int, Int) -> Int`)
5. Named functions (`FUN`) with higher-order support
6. Closures/anonymous functions (`x => expr`)
7. First-class functions (pass named functions as values)
8. Pipeline operator (`|>`)
9. Range operator (`..`)
10. Regex pattern matching (`matches`, `/pattern/`)

## Phase 2 Tasks

### 1. Coalesce Operator (`??`)

Unwraps a "success" container (`Option.Some` or `Result.Ok`), or falls back to the right operand.

```rumps
SET name = GET ^PATIENT(id, "NAME") ?? "Unknown"
SET config = user-config ?? default-config
SET value = risky-operation() ?? "fallback"  ; works with Result too
```

- [x] Add `Token::QuestionQuestion` to lexer
- [x] Add `BinOp::Coalesce` to AST
- [x] Implement in interpreter:
  - Evaluate left operand
  - If `Tagged(OPTION, 1, [v])` (i.e., `Some(v)`), return `v` (unwrapped)
  - If `Tagged(OPTION, 0, [])` (i.e., `None`), evaluate and return right operand
  - If `Tagged(RESULT, 0, [v])` (i.e., `Ok(v)`), return `v` (unwrapped)
  - If `Tagged(RESULT, 1, [_])` (i.e., `Err(_)`), evaluate and return right operand (error discarded)
  - If left is any other value (not an Option/Result), return type error
- [x] Add unit tests
- [x] Add integration test script (partial; see note below)

**Note: Parser limitation with `IF` expressions**

The current parser does not allow `IF` expressions in expression contexts (e.g., `LET x = IF FALSE { 42 }`). `IF` is only parsed at the statement level. This blocks full integration testing of `??` with `Option.None` values from `IF` without `ELSE`.

The parser architecture has two separate grammars:
- **Statement grammar**: includes `IF`, `LET`, `SET`, `OUTPUT`, etc.
- **Expression grammar**: includes literals, variables, binary ops, blocks `{ }`, but NOT `IF`

To fix this, `IF` needs to be added to `primary_expr` in the expression grammar, similar to how block expressions `{ ... }` are handled. This would allow `IF` to appear anywhere an expression is expected.

### 2. Optional Chaining Operator (`?.`)

Safe field/subscript access that short-circuits to `Option.None` if the base is `None`.

```rumps
SET city = patient?.address?.city ?? "N/A"
```

- [ ] Add `Token::QuestionDot` to lexer
- [ ] Add `Expr::OptionalField(ExprId, String)` to AST
- [ ] Implement in interpreter:
  - Evaluate base expression
  - If `Option.None`, return `Option.None`
  - If `Option.Some(v)`, access field on `v`, wrap result in `Option.Some`
  - If non-Option value, access field normally, wrap result in `Option.Some`
- [ ] Add unit tests
- [ ] Add integration test script

### 3. Type Check Operator (`is`) with Pattern Binding

Runtime type checking that returns a boolean. Supports optional pattern binding for variant payloads (similar to Rust's `if let`).

```rumps
; Simple type check
IF value is Int {
  OUTPUT "It's an integer"
}

; Variant check without binding
IF x is Option.None {
  OUTPUT "No value"
}

; Variant check with binding (like Rust's if let)
IF x is Option.Some(val) {
  OUTPUT "Got: " ++ val    ; val is bound in this scope
}

IF result is Result.Ok(data) {
  OUTPUT data.name         ; data is bound
}

IF result is Result.Err(e) {
  OUTPUT "Error: " ++ e
}

; Wildcard to check variant without binding payload
IF x is Option.Some(_) {
  OUTPUT "Has a value"
}

; Multiple payloads (if we ever have them)
IF pair is Pair(a, b) {
  OUTPUT a ++ ", " ++ b
}
```

- [ ] Add `Token::Is` keyword to lexer
- [ ] Add `Expr::Is(ExprId, TypePattern)` to AST
- [ ] Define `TypePattern` enum:
  ```rust
  enum TypePattern {
      Type(TypeId),                                    // `is Int`, `is String`
      Variant(TypeId, u8),                             // `is Option.None` (no parens)
      VariantWildcard(TypeId, u8),                     // `is Option.Some(_)`
      VariantBind(TypeId, u8, SmallVec<[String; 2]>),  // `is Option.Some(val)`
  }
  ```
- [ ] Implement in interpreter:
  - For `Type(tid)`: check if value's type matches `tid`
  - For `Variant(tid, idx)`: check if value is `Tagged(tid, idx, _)` (variant has no payload)
  - For `VariantWildcard(tid, idx)`: check variant, ignore payload
  - For `VariantBind(tid, idx, names)`: check variant, bind payload values to names in scope
- [ ] Scope handling for bindings:
  - Bindings are only visible in the `then` branch of the `IF`
  - Create a new scope frame, bind variables, evaluate body, pop scope
  - Arity check: number of binding names must match variant's payload arity
- [ ] Add unit tests
- [ ] Add integration test script

### 4. Type Cast Operator (`as`)

Explicit type conversion with runtime validation.

```rumps
SET n = "42" as Int
SET s = 3.14 as String
SET arr = json-data as Array[Int]
```

- [ ] Add `Token::As` keyword to lexer
- [ ] Add `Expr::As(ExprId, TypeExprId)` to AST
- [ ] Implement coercion rules in interpreter:
  - `String -> Int`: parse, error if invalid
  - `String -> Float`: parse, error if invalid
  - `Int -> Float`: widen
  - `Float -> Int`: truncate
  - `Any -> String`: stringify
  - `Int -> Bool`: `0` -> `false`, else `true`
  - `Bool -> Int`: `false` -> `0`, `true` -> `1`
  - Failed conversions produce runtime errors with clear messages
- [ ] Add unit tests
- [ ] Add integration test script

### 5. Power Operator (`**`)

Exponentiation.

```rumps
SET squared = x ** 2
SET cubed = 2 ** 10
```

- [ ] Add `Token::StarStar` to lexer
- [ ] Add `BinOp::Power` to AST
- [ ] Update parser precedence (power is higher than multiplicative, right-associative)
- [ ] Implement in interpreter:
  - `Int ** Int`: use `i64::pow` (handle overflow)
  - `Float ** Float`: use `f64::powf`
  - `Int ** Float` or `Float ** Int`: coerce to float, use `powf`
- [ ] Add unit tests
- [ ] Add integration test script

### 6. Function Types

Type expressions for functions, enabling higher-order function signatures.

```rumps
; Function type syntax: (params...) -> ReturnType
; Single parameter (parens optional)
Int -> Int
(Int) -> Int

; Multiple parameters
(Int, Int) -> Int

; Nullary function
() -> String

; Higher-order: function that takes a function
((Int) -> Int, Int) -> Int

; Function returning a function
(Int) -> (Int) -> Int
```

- [ ] Extend AST type expression representation:
  ```rust
  /// Type expression in the AST (for annotations).
  enum AstTypeExpr {
      Named(String),                                      // `Int`, `String`
      App(String, SmallVec<[AstTypeExprId; 2]>),          // `Array[Int]`, `Result[T, E]`
      Fn(SmallVec<[AstTypeExprId; 4]>, AstTypeExprId),    // `(Int, Int) -> Int`
  }
  ```
- [ ] Extend runtime `TypeExpr` in `value.rs`:
  ```rust
  enum TypeExpr {
      Named(TypeId),
      App(TypeId, SmallVec<[TypeExprId; 2]>),
      Fn(SmallVec<[TypeExprId; 4]>, TypeExprId),  // params, return
  }
  ```
- [ ] Update parser to handle function type syntax:
  - `->` is right-associative: `Int -> Int -> Int` parses as `Int -> (Int -> Int)`
  - Parentheses group parameters: `(Int, Int) -> Int`
  - Empty parens for nullary: `() -> Int`
- [ ] Implement type expression resolution (AST -> runtime `TypeExprId`)
- [ ] Add unit tests for function type parsing
- [ ] Add integration tests

### 7. Named Functions (`FUN`)

User-defined named functions with optional type annotations. Type annotations can be any type expression, including function types for higher-order functions.

```rumps
; Untyped (types inferred/unchecked)
FUN greet (name) {
  OUTPUT "Hello, " ++ name ++ "!"
}

; Typed parameters
FUN add (a: Int, b: Int) {
  a + b
}

; Typed parameters and return type
FUN square (x: Int) -> Int {
  x * x
}

; Recursive
FUN factorial (n: Int) -> Int {
  IF n <= 1 { 1 }
  ELSE { n * factorial(n - 1) }
}

; Higher-order: function parameter
FUN apply (f: (Int) -> Int, x: Int) -> Int {
  f(x)
}

; Higher-order: returns a function
FUN make-adder (n: Int) -> (Int) -> Int {
  x => x + n
}

; Composition
FUN compose (f: (Int) -> Int, g: (Int) -> Int) -> (Int) -> Int {
  x => f(g(x))
}
```

- [ ] Add `Token::Fun` keyword to lexer
- [ ] Add `Token::Arrow` (`->`) for return type annotation
- [ ] Add AST representation:
  ```rust
  Stmt::Fun {
      name: String,
      params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,  // name + optional type expr
      ret: Option<AstTypeExprId>,                               // optional return type expr
      body: ExprId,
  }
  ```
- [ ] Parse type annotations using full type expression grammar (section 6)
- [ ] Add function storage to interpreter (name -> definition)
- [ ] Implement function definition (stores in environment)
- [ ] Support recursion (function visible in its own body)
- [ ] Runtime type checking at call site:
  - If param has type annotation, validate argument against type expr
  - For function types, check that argument is callable with matching signature
  - If return type annotation, check result type before returning
  - Produce clear error: `"expected (Int) -> Int, got Int for parameter 'f'"`
- [ ] Add unit tests
- [ ] Add integration test script

### 8. Function Calls and First-Class Functions

Complete the existing `Expr::Call` implementation and support named functions as first-class values.

```rumps
; Direct calls
greet("World")
SET sum = add(10, 20)
SET area = square(side) * 4

; Named function as value (no parens = reference, not call)
LET f = square         ; f is now a function value
OUTPUT f(5)            ; 25

; Pass named function to higher-order function
OUTPUT apply(square, 5)           ; 25
OUTPUT apply(double, 5)           ; 10

; Compose named functions
LET sq-then-dbl = compose(double, square)
OUTPUT sq-then-dbl(3)             ; 18 (square(3)=9, double(9)=18)
```

- [ ] Implement `Expr::Call` in interpreter:
  - Look up function by name OR evaluate callee expression
  - If callee is `Value::Closure` or `Value::Function`, apply it
  - Evaluate arguments
  - Create new scope with parameters bound to arguments
  - Evaluate function body
  - Return result (last expression value)
- [ ] Add `Value::Function` variant for named function references:
  ```rust
  Value::Function {
      name: StringId,
      params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
      ret: Option<TypeExprId>,
      body: ExprId,
  }
  ```
- [ ] Resolve bare identifiers: if name refers to a function (not a variable), produce `Value::Function`
- [ ] Arity checking (error if wrong number of arguments)
- [ ] Accept both `Value::Function` and `Value::Closure` at call sites expecting function-typed arguments:
  - Named function reference: `apply(square, 5)`
  - Untyped closure: `apply(x => x * 2, 5)`
  - Typed closure: `apply((x: Int) -> Int => x * 2, 5)`
  - Validate against the declared function type (arity, param types, return type)
- [ ] Add unit tests
- [ ] Add integration test script

### 9. Closures (Anonymous Functions)

Lambda expressions with arrow syntax and optional type annotations. Type annotations can be any type expression, including function types.

```rumps
; Simple closure (untyped)
x => x * 2

; Multi-parameter (untyped)
(a, b) => a + b

; Typed parameter
(x: Int) => x * 2

; Typed parameters
(a: Int, b: Int) => a + b

; With return type
(x: Int) -> Int => x * x

; Block body
x => {
  LET doubled = x * 2
  doubled * doubled
}

; Higher-order closure: takes a function
(f: (Int) -> Int, x: Int) -> Int => f(x)

; Closure returning a closure
(n: Int) -> (Int) -> Int => (x => x + n)
```

- [ ] Add `Token::FatArrow` (`=>`) to lexer
- [ ] Add AST representation:
  ```rust
  Expr::Closure {
      params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,  // name + optional type expr
      ret: Option<AstTypeExprId>,                               // optional return type expr
      body: ExprId,
  }
  ```
- [ ] Add `Value::Closure` variant to runtime values:
  ```rust
  Value::Closure {
      params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
      ret: Option<TypeExprId>,
      body: ExprId,
      env: CapturedEnv,  // captured lexical scope
  }
  ```
- [ ] Implement closure creation (captures current environment)
- [ ] Implement closure application (like function call but with captured env)
- [ ] Runtime type checking (same as named functions, including function type params)
- [ ] Add unit tests
- [ ] Add integration test script

### 10. Pipeline Operator (`|>`)

Left-to-right function application.

```rumps
[1, 2, 3] |> MAP x => x * 2 |> FILTER x => x > 2

; Equivalent to:
FILTER (x => x > 2) (MAP (x => x * 2) [1, 2, 3])

; With named functions
value |> transform |> validate |> save
```

- [ ] Add `Token::Pipe` (`|>`) to lexer
- [ ] Add `BinOp::Pipe` to AST
- [ ] Implement in interpreter:
  - Evaluate left operand (the value)
  - Evaluate right operand (should be a function/closure)
  - Apply right to left: `right(left)`
- [ ] Left-associative, low precedence
- [ ] Add unit tests
- [ ] Add integration test script

### 11. Range Operator (`..`)

Creates a lazy range of integers.

```rumps
1..10           ; range from 1 to 10 (inclusive? exclusive? TBD)
0..n            ; range from 0 to n
1..100 |> MAP x => x * x
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

### 12. Regex Pattern Matching (`matches`)

Pattern matching with regex literals.

```rumps
IF email matches /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

IF ssn matches /^\d{3}-\d{2}-\d{4}$/ {
  OUTPUT "Valid SSN format"
}

; Negation
IF input matches! /[<>]/ {
  OUTPUT "No angle brackets"
}
```

- [ ] Add regex literal support to lexer (`/pattern/`)
  - Handle escape sequences (`\/`, `\\`)
  - Consider flags suffix (`/pattern/i` for case-insensitive)
- [ ] Add `Token::Regex(String)` for regex literals
- [ ] Add `Token::Matches` keyword to lexer
- [ ] Add `Expr::Matches(ExprId, String)` to AST (value, pattern)
- [ ] Add `regex` crate dependency
- [ ] Implement in interpreter:
  - Compile regex (cache compiled patterns)
  - Return `true`/`false` for match
- [ ] Add unit tests
- [ ] Add integration test script

**Deferred regex features** (for later):
- Named capture groups (`(?<name>...)`) and `Match.name` access
- `matches!` negation syntax (can use `NOT (x matches /.../)` for now)
- Regex flags (`/pattern/i`, `/pattern/m`)

## Deferred to Later Phases

### Spread Operator (`...`)
```rumps
SET combined = [...arr1, ...arr2]
```
**Reason**: Needs more collection/array infrastructure and clear semantics for objects vs arrays.

### Contains Operator (`contains`)
```rumps
IF name contains "Smith" { ... }
```
**Reason**: Lower priority; can be implemented as `String.contains(a, b)` function first.

### JSON Operators
```rumps
data->>"name"           ; field access
data #>> ["a", "b"]     ; path access
obj @> {"key": "val"}   ; containment
data ? "field"          ; key existence
obj || defaults         ; merge
```
**Reason**: Extensive set (10+ operators). Deserves its own focused phase.

### Higher-Order Collection Operations
```rumps
MAP (x => x * 2) [1, 2, 3]
FILTER (x => x > 2) [1, 2, 3, 4]
REDUCE (acc, x => acc + x) 0 [1, 2, 3]
```
**Reason**: With function types and closures in place, these are now straightforward to implement. Deferred to Phase 3 to focus Phase 2 on the foundational type and function infrastructure. The implementations will use function types like:
```rumps
; MAP signature: ((T) -> U, Array[T]) -> Array[U]
; FILTER signature: ((T) -> Bool, Array[T]) -> Array[T]
; REDUCE signature: ((A, T) -> A, A, Array[T]) -> A
```

## Design Decisions

### `??` Unwraps Success Containers

The coalesce operator unwraps `Option.Some(v)` or `Result.Ok(v)` to `v`, returning the fallback otherwise:

```rumps
; Option
SET x = Some(42) ?? 0   ; x = 42, not Some(42)
SET y = None ?? 0       ; y = 0

; Result
SET a = Ok(42) ?? 0     ; a = 42
SET b = Err("oops") ?? 0  ; b = 0 (error discarded)
```

For `Option`, this matches Rust's `unwrap_or` semantics. For `Result`, the error is intentionally discarded; if you need to handle the error, use `is Result.Err(e)` pattern binding instead.

### `?.` Returns `Option`

Optional chaining always returns an `Option`, even if the base was not an Option:

```rumps
SET patient = { name: "John" }
SET name = patient?.name   ; name = Some("John"), not "John"
```

This ensures consistent typing and composability with `??`.

### `is` Checks Variant with Optional Binding

For sum types, `is` checks the variant and optionally binds the payload:

```rumps
SET x = Option.Some(42)

; Variant check (no binding)
x is Option.Some       ; true (checks variant)
x is Int               ; false (x is an Option, not an Int)

; Variant check with binding (like Rust's if let)
IF x is Option.Some(val) {
  OUTPUT val           ; val = 42, bound in this scope only
}

; Wildcard: check variant, ignore payload
IF x is Option.Some(_) {
  OUTPUT "has value"
}
```

**Binding scope:** Variables bound by `is` are only visible in the `then` branch of the `IF`. They do not leak into the `else` branch or surrounding scope.

**Arity checking:** The number of binding names must match the variant's payload arity:
```rumps
; Option.Some has arity 1
IF x is Option.Some(a, b) { ... }  ; ERROR: expected 1 binding, got 2

; Result.Ok has arity 1
IF r is Result.Ok(val) { ... }     ; OK
```

**No parens vs empty parens:** For zero-arity variants, no parens are used:
```rumps
IF x is Option.None { ... }    ; correct
IF x is Option.None() { ... }  ; parse error
```

To check payload type after unwrapping:
```rumps
(x ?? 0) is Int    ; true
```

### `as` vs Implicit Coercion

`as` is for explicit conversions that may fail or lose precision. Implicit coercion (Phase 1) handles safe cases like `Int + Float`. Use `as` when:
- Parsing strings to numbers (`"42" as Int`)
- Narrowing (`Float as Int`)
- The conversion might fail

### Functions Return Last Expression

No explicit `RETURN` keyword. The last expression in a function body is its result:

```rumps
FUN add (a, b) {
  a + b          ; this is returned
}
```

For side-effect-only functions, the result is `Option.None`:

```rumps
FUN log (msg) {
  OUTPUT msg     ; OUTPUT returns None
}
```

### Closures Capture by Value

Closures capture their environment at creation time (by value, not reference):

```rumps
LET x = 10
LET f = n => n + x
LET x = 20           ; rebind x
OUTPUT f(5)          ; outputs 15, not 25
```

This avoids complexity around mutable captures and matches the immutable-by-default style.

### Function Types

Function types use arrow syntax: `(params...) -> Return`. This enables higher-order functions with full type safety.

**Syntax rules:**

```rumps
; Parentheses required for multiple or zero params
(Int, Int) -> Int      ; two params
() -> String           ; nullary

; Single param: parens optional
Int -> Int             ; same as (Int) -> Int
(Int) -> Int           ; explicit

; Arrow is right-associative (currying)
Int -> Int -> Int      ; same as Int -> (Int -> Int)
(Int) -> (Int) -> Int  ; explicit

; Higher-order: function params need parens
((Int) -> Int) -> Int  ; takes a function, returns Int
```

**Type checking:**

Function type annotations are checked at runtime when:
1. A function/closure is passed as an argument with a function type annotation
2. A function/closure is called and its parameter/return types are annotated

Checking validates:
- Arity matches (same number of parameters)
- Parameter types are compatible (structural, not nominal)
- Return type is compatible

**Structural compatibility:**

Two function types are compatible if their parameter and return types match structurally:

```rumps
FUN apply (f: (Int) -> Int, x: Int) -> Int { f(x) }

; These all work:
apply(square, 5)                    ; named function
apply(x => x * 2, 5)                ; untyped closure
apply((x: Int) -> Int => x * 2, 5)  ; typed closure
```

**Named functions as values:**

When a named function is referenced without being called, it produces a function value:

```rumps
FUN double (x: Int) -> Int { x * 2 }

LET f = double    ; f is a function value, not a call
OUTPUT f(5)       ; 10

; Equivalent to:
LET g = (x: Int) -> Int => x * 2
```

This enables passing named functions to higher-order functions without wrapping them in closures.

### Pipeline Precedence

`|>` has very low precedence (lower than comparison) and is left-associative:

```rumps
a |> f |> g |> h     ; ((a |> f) |> g) |> h
x + 1 |> double      ; (x + 1) |> double
```

### Range Semantics

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

### Regex Literals

Regex literals use `/pattern/` syntax. The pattern is compiled at first use and cached.

```rumps
/^hello/           ; anchored at start
/world$/           ; anchored at end
/\d{3}-\d{4}/      ; digit patterns
```

The `matches` operator returns a boolean. For capture groups and more advanced features, use a `Regex.match(pattern, string)` function (deferred).

Regex literals cannot span multiple lines. Use string concatenation for complex patterns:

```rumps
LET pattern = "^(" ++ part1 ++ ")|(" ++ part2 ++ ")$"
IF text matches Regex.compile(pattern) { ... }
```

## Success Criteria

The following should work:

```rumps
; Null coalesce
LET name = GET ^PATIENT(999, "NAME") ?? "Unknown"
OUTPUT name  ; "Unknown"

; Optional chaining
LET patient = { address: { city: "NYC" } }
LET city = patient?.address?.city ?? "N/A"
OUTPUT city  ; "NYC"

LET empty = Option.None
LET missing = empty?.field ?? "default"
OUTPUT missing  ; "default"

; Type checking
LET x = 42
IF x is Int {
  OUTPUT "integer"
}

LET opt = Option.Some(10)
IF opt is Option.Some {
  OUTPUT "has value"
}

; Pattern binding with is (like Rust's if let)
IF opt is Option.Some(val) {
  OUTPUT "Got: " ++ (val as String)  ; val = 10, bound in this scope
}

LET result = Result.Ok({ name: "Alice", age: 30 })
IF result is Result.Ok(data) {
  OUTPUT data.name    ; "Alice"
}

IF result is Result.Err(e) {
  OUTPUT "Error: " ++ e
} ELSE {
  OUTPUT "Success!"
}

; Wildcard: check variant without binding
IF opt is Option.Some(_) {
  OUTPUT "Has some value"
}

; Type casting
LET n = "123" as Int
OUTPUT n + 1  ; 124

LET s = 3.14 as String
OUTPUT "Pi is " ++ s

; Power
OUTPUT 2 ** 10  ; 1024
OUTPUT 3.0 ** 0.5  ; ~1.732

; Named functions (untyped)
FUN square (x) {
  x * x
}

OUTPUT square(5)      ; 25

; Named functions (typed)
FUN add (a: Int, b: Int) -> Int {
  a + b
}

FUN greet (name: String) -> String {
  "Hello, " ++ name ++ "!"
}

OUTPUT add(10, 20)    ; 30
OUTPUT greet("World") ; "Hello, World!"

; Type error example (would fail at runtime)
; add("x", "y")  ; ERROR: expected Int, got String for parameter 'a'

; Recursive function (typed)
FUN factorial (n: Int) -> Int {
  IF n <= 1 { 1 }
  ELSE { n * factorial(n - 1) }
}

OUTPUT factorial(5)   ; 120

; Closures (untyped)
LET double = x => x * 2
OUTPUT double(21)     ; 42

; Closures (typed)
LET add-typed = (a: Int, b: Int) -> Int => a + b
OUTPUT add-typed(10, 20)  ; 30

; Closure with captured variable
LET multiplier = 3
LET triple = (x: Int) -> Int => x * multiplier
OUTPUT triple(10)     ; 30

; Higher-order function: function as parameter
FUN apply (f: (Int) -> Int, x: Int) -> Int {
  f(x)
}

OUTPUT apply(square, 5)       ; 25 (pass named function)
OUTPUT apply(x => x + 1, 5)   ; 6  (pass closure)

; Higher-order function: returns a function
FUN make-adder (n: Int) -> (Int) -> Int {
  x => x + n
}

LET add5 = make-adder(5)
OUTPUT add5(10)               ; 15

; Function composition
FUN compose (f: (Int) -> Int, g: (Int) -> Int) -> (Int) -> Int {
  x => f(g(x))
}

LET double = x => x * 2
LET sq-then-dbl = compose(double, square)
OUTPUT sq-then-dbl(3)         ; 18 (square(3)=9, double(9)=18)

; Named function as first-class value
LET f = square                ; reference, not call
OUTPUT f(4)                   ; 16

; Higher-order with typed closure
LET apply-twice = (f: (Int) -> Int, x: Int) -> Int => f(f(x))
OUTPUT apply-twice(double, 5) ; 20

; Pipeline
LET result = 5 |> double |> square
OUTPUT result         ; 100

; Pipeline with inline closure
LET nums = 10
LET result2 = nums |> (x => x + 1) |> (x => x * 2)
OUTPUT result2        ; 22

; Range
LET r = 1..5
; r is a range value

; Range with pipeline (when iteration is supported)
; 1..10 |> MAP x => x * x

; Regex matching
LET email = "user@example.com"
IF email matches /^[^@]+@[^@]+\.[^@]+$/ {
  OUTPUT "Valid email"
}

LET phone = "555-1234"
IF phone matches /^\d{3}-\d{4}$/ {
  OUTPUT "Valid phone"
}

; Negated match
LET input = "safe text"
IF NOT (input matches /<script>/) {
  OUTPUT "No script tags"
}
```
