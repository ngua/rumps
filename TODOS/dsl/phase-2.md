# Phase 2: Operators, Functions, and Patterns

This document tracks the second phase of implementing the RUMPS query language: operators, functions, closures, ranges, and regex pattern matching.

**Prerequisites**: Phase 1 (infrastructure) complete.

**Testing**: Each feature requires both unit tests and integration tests (`.rumps` script + `.expected` output in `tests/scripts/`).

**NOTE**: If integration tests are failing after modifications to parser, etc..., it may be due to outdated snapshots. Use `cargo insta` to fix

## Goals

1. Coalesce and optional chaining (`??`, `?.`)
2. Runtime type checking and casting (`is`, `as`, `read`)
3. Arithmetic power operator (`**`)
4. Function types (`(Int, Int) -> Int`)
5. Named functions (`FUN`) with higher-order support
6. Closures/anonymous functions (`x => expr`)
7. First-class functions (pass named functions as values)
8. Pipeline operator (`|>`)

## Phase 2 Tasks

### 0.1. Variant Constructor Syntax

**PRIORITY: HIGH** - Core infrastructure needed before other Phase 2 features.

The type system supports `Option` and `Result` variants with payloads, but there was no syntax to construct them explicitly. This blocks testing of `??`, `is`, pattern matching, etc.

**Implemented**:

```rumps
; With payload
LET some = Option.Some(42)
LET ok = Result.Ok("success")
LET err = Result.Err("failed")

; Zero-arity
LET none = Option.None

; Nested
LET nested = Option.Some(Result.Ok(1))

; With expressions
LET x = 10
LET computed = Option.Some(x * 2)
```

- [x] Add `Expr::Variant(type_name, variant_name, args)` to AST
- [x] Add `TypeRegistry::lookup_variant()` method
- [x] Modify parser's `fold_postfix` to handle `Type.Variant(args)` syntax
- [x] Add interpreter evaluation for `Expr::Variant`
- [x] Modify `field()` to handle zero-arity variants like `Option.None`
- [x] Add parser unit tests
- [x] Add interpreter unit tests
- [x] Add integration test script (`29_variant_constructors.rumps`)

### 0.2. Indentation-Based Expression Continuation

**PRIORITY: HIGH** - Core infrastructure that should have been working in Phase 1.

The lexer correctly emits `Indent`/`Dedent` tokens, but the parser ignores them. Expressions should continue across newlines when indented:

```rumps
LET x = 1
    + 2       ; Indent after newline = continuation
    + 3       ; Same indent level = still continuing

OUTPUT x      ; Should print 6
```

Currently fails with: `parse error: unexpected '+' (expected statement)`

**Implementation**: Parser handles `Newline`/`Indent`/`Dedent` tokens via `opt_newlines()`. All whitespace tokens are preserved in the token stream for formatters; the parser skips them where continuation is allowed (binary operators, array/object contents, function arguments, etc.).

- [x] Update parser to use `opt_newlines()` around binary operators
- [x] Update parser to handle newlines in arrays, objects, function calls
- [x] Add parser tests for continuation cases
- [x] Add integration test script (`28_indent_continuation.rumps`)

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

### 2. Optional Chaining Operator (`?.`)

Safe field/subscript access that short-circuits to `Option.None` if the base is `None`.

```rumps
SET city = patient?.address?.city ?? "N/A"
```

- [x] Add `Token::QuestionDot` to lexer
- [x] Add `Expr::OptionalField(ExprId, String)` to AST
- [x] Implement in interpreter:
  - Evaluate base expression
  - If `Option.None`, return `Option.None`
  - If `Option.Some(v)`, access field on `v`, wrap result in `Option.Some`
  - If non-Option value, access field normally, wrap result in `Option.Some`
- [x] Add unit tests
- [x] Add integration test script (`30_optional_chaining.rumps`)

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

; With ELSE branch (bindings NOT visible in ELSE)
IF x is Option.Some(val) {
  OUTPUT "Got: " ++ val
} ELSE {
  OUTPUT "No value"
  ; `val` is NOT in scope here
}

; Multiple payloads (if we ever have them)
IF pair is Pair(a, b) {
  OUTPUT a ++ ", " ++ b
}
```

- [x] Add `Token::Is` keyword to lexer
- [x] Define `TypePattern` enum:
  ```rust
  enum TypePattern {
      Type(TypeId),                                    // `is Int`, `is String`
      Variant(TypeId, u8),                             // `is Option.None` (no parens)
      VariantWildcard(TypeId, u8),                     // `is Option.Some(_)`
      VariantBind(TypeId, u8, SmallVec<[String; 2]>),  // `is Option.Some(val)`
  }
  ```
- [x] Add `Expr::Is(ExprId, TypePattern)` to AST
- [x] Implement in interpreter:
  - For `Type(tid)`: check if value's type matches `tid`
  - For `Variant(tid, idx)`: check if value is `Tagged(tid, idx, _)` (variant has no payload)
  - For `VariantWildcard(tid, idx)`: check variant, ignore payload
  - For `VariantBind(tid, idx, names)`: check variant, bind payload values to names in scope
- [x] Scope handling for bindings:
  - Bindings are only visible in the `then` branch of the `IF`
  - Bindings must NOT be visible in `ELSE` branch or after the `IF` statement
  - Create a new scope frame, bind variables, evaluate body, pop scope
  - `ELSE` branch (if present) is evaluated in the outer scope (no bindings)
  - Arity check: number of binding names must match variant's payload arity
- [x] Add unit tests
- [x] Add integration test script (`31_is_operator.rumps`)

### 4. Type Cast Operator (`as`)

Explicit type conversion with runtime validation.

```rumps
LET f = 42 as Float
LET s = 3.14 as String
; SET arr = json-data as Array[Int] (FUTURE; can't implement now, haven't done JSON yet)
```

- [x] Add `Token::As` keyword to lexer
- [x] Add `Expr::As(ExprId, AstTypeExprId)` to AST
- [x] Implement coercion rules in interpreter:
  - `Int -> Float`: widen
  - `Float -> Int`: truncate
  - `T -> String`: stringify (any type can convert to string)
  - `Bool -> Int`: `false` -> `0`, `true` -> `1`
  - **NOTE**: Extend this list as new infallible conversions are needed
  - For fallible conversions (`String -> Int`, `String -> Float`, `Int -> Bool`), use `read` (section 4.1)
- [x] Add unit tests
- [x] Add integration test script (`34_as_cast.rumps`)

### 4.1. Fallible Conversion Operator (`read`)

Explicit type conversion that returns a `Result[T, String]` instead of throwing a runtime error. Use this for conversions that may fail based on the input value.

```rumps
; String parsing
LET n = "42" read Int           ; Result.Ok(42)
LET bad = "abc" read Int        ; Result.Err("invalid integer: abc")

LET f = "3.14" read Float       ; Result.Ok(3.14)
LET bad2 = "xyz" read Float     ; Result.Err("invalid float: xyz")

; Strict bool conversion (only 0 and 1)
LET t = 1 read Bool             ; Result.Ok(true)
LET f = 0 read Bool             ; Result.Ok(false)
LET bad3 = 42 read Bool         ; Result.Err("expected 0 or 1 for Bool, got 42")

; Chain with ?? for default
LET port = env-port read Int ?? 8080

; Chain with is for error handling
LET parsed = user-input read Int
IF parsed is Result.Err(e) {
  OUTPUT "Parse error: " ++ e
}
```

- [x] Add `Token::Read` keyword to lexer
- [x] Add `Expr::Read(ExprId, AstTypeExprId)` to AST
- [x] Implement in interpreter (returns `Result[T, String]` value, NOT `Err(crate::Error)`):
  - `String -> Int`: parse, `Result.Err` if invalid
  - `String -> Float`: parse, `Result.Err` if invalid
  - `Int -> Bool`: `0` -> `Result.Ok(false)`, `1` -> `Result.Ok(true)`, else `Result.Err`
  - **NOTE**: Extend this list as new fallible conversions are needed
- [x] Add unit tests
- [x] Add integration test script (`35_read_convert.rumps`)

### 5. Power Operator (`**`)

Exponentiation.

```rumps
SET squared = x ** 2
SET cubed = 2 ** 10
```

- [x] Add `Token::StarStar` to lexer
- [x] Add `BinOp::Pow` to AST
- [x] Update parser precedence (power is higher than multiplicative, right-associative)
- [x] Implement in interpreter:
  - `Int ** Int`: use `i64::checked_pow` (overflow falls back to float)
  - `Float ** Float`: use `f64::powf`
  - `Int ** Float` or `Float ** Int`: coerce to float, use `powf`
  - `Int ** negative Int`: coerce to float for fractional result
- [x] Add unit tests
- [x] Add integration test script (`36_power.rumps`)

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

- [x] Add `Token::Arrow` (`->`) to lexer
  - **Must** parse before `-` to avoid consuming as `Minus`
- [x] Add lexer tests for `->` token
- [x] Extend AST type expression representation:
  ```rust
  /// Type expression in the AST (for annotations).
  enum AstTypeExpr {
      Named(String),                                      // `Int`, `String`
      App(String, SmallVec<[AstTypeExprId; 2]>),          // `Array[Int]`, `Result[T, E]`
      Fn(SmallVec<[AstTypeExprId; 4]>, AstTypeExprId),    // `(Int, Int) -> Int`
  }
  ```
- [x] Extend runtime `TypeExpr` in `value.rs`:
  ```rust
  enum TypeExpr {
      Named(TypeId),
      App(TypeId, SmallVec<[TypeExprId; 2]>),
      Fn(SmallVec<[TypeExprId; 4]>, TypeExprId),  // params, return
  }
  ```
- [x] Update parser to handle function type syntax:
  - `->` is right-associative: `Int -> Int -> Int` parses as `Int -> (Int -> Int)`
  - Parentheses group parameters: `(Int, Int) -> Int`
  - Empty parens for nullary: `() -> Int`
- [x] Implement type expression resolution (AST -> runtime `TypeExprId`)
- [x] Add unit tests for function type parsing

### 7. Closures (Anonymous Functions)

Lambda expressions with arrow syntax and optional type annotations. Type annotations can be any type expression, including function types. Closures are first-class values and can be bound to variables via `LET`.

**Note**: Calling closures requires `Expr::Call` (section 9). This section focuses on closure creation and binding.

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

; Binding closures to variables
LET double = x => x * 2
LET add = (a: Int, b: Int) -> Int => a + b

; Closure capturing a variable
LET factor = 3
LET scale = (x: Int) -> Int => x * factor
```

- [x] Add `Token::FatArrow` (`=>`) to lexer
- [x] Add AST representation:
  ```rust
  Expr::Closure {
      params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,  // name + optional type expr
      ret: Option<AstTypeExprId>,                               // optional return type expr
      body: ExprId,
  }
  ```
- [x] Define `CapturedEnv` for closures (captured lexical scope)
- [x] Add `Value::Closure` variant to runtime values:
  ```rust
  Value::Closure {
      params: SmallVec<[(StringId, Option<TypeExprId>); 4]>,
      ret: Option<TypeExprId>,
      body: ExprId,
      env: CapturedEnv,  // captured lexical scope
  }
  ```
- [x] Implement closure creation (captures current environment)
- [x] Add unit tests
- [x] Add integration test script (minimal: bind closures, verify they're values)

### 8. Named Functions (`FUN`)

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

- [x] Add `Token::Fun` keyword to lexer
- [x] Add AST representation:
  ```rust
  Stmt::Fun {
      name: String,
      params: SmallVec<[(String, Option<AstTypeExprId>); 4]>,  // name + optional type expr
      ret: Option<AstTypeExprId>,                               // optional return type expr
      body: ExprId,
  }
  ```
- [x] Parse type annotations using full type expression grammar (section 6)
- [x] Add function storage to interpreter (name -> definition)
- [x] Implement function definition (stores in environment)
- [x] Support recursion (function visible in its own body)
- [ ] Runtime type checking at call site:
  - If param has type annotation, validate argument against type expr
  - For function types, check that argument is callable with matching signature
  - If return type annotation, check result type before returning
  - Produce clear error: `"expected (Int) -> Int, got Int for parameter 'f'"`
- [x] Add unit tests
- [x] Add integration test script

### 9. Function Calls and First-Class Functions

Complete the existing `Expr::Call` implementation and support named functions as first-class values. Critically, since closures can be bound to variables (section 7), the call evaluation must check lexically bound variables when resolving the callee.

**NOTE**: Basic name-based function calling was implemented alongside Step 8. This step completes expression-based callees and runtime type checking.

```rumps
; Direct calls to named functions
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

; Calling closures bound to variables
LET double = x => x * 2
OUTPUT double(10)                 ; 20 (looks up `double` in scope, finds closure)

; Calling closure with captured variable
LET factor = 3
LET scale = (x: Int) -> Int => x * factor
OUTPUT scale(10)                  ; 30

; Closures in objects (requires expression-based callee)
LET ops = { inc: x => x + 1, dec: x => x - 1 }
OUTPUT ops.inc(5)                 ; 6 (field access yields closure, then call)

; Chained calls (requires expression-based callee)
FUN make_adder (n) { x => x + n }
OUTPUT make_adder(5)(10)          ; 15

; IIFE (requires expression-based callee)
OUTPUT (x => x * 2)(21)           ; 42
```

**Implemented in Step 8:**

- [x] Implement `Expr::Call` in interpreter (name-based):
  - If callee is an identifier, first check named function registry
  - If not found in registry, look up identifier in lexical scope (may be a bound closure)
  - If callee is `Value::Closure` or `Value::Function`, apply it
  - Evaluate arguments
  - Create new scope with parameters bound to arguments
  - Evaluate function body
  - Return result (last expression value)
- [x] Add `Value::Function` variant for named function references
- [x] Resolve bare identifiers: if name refers to a function (not a variable), produce `Value::Function`
- [x] Arity checking (error if wrong number of arguments)
- [x] Implement closure application (restore captured env, bind params, evaluate body)
- [x] Add unit tests (basic calling)
- [x] Add integration test script (`38_named_functions.rumps`)

**Remaining (all complete):**

- [x] Expression-based callees (`ops.inc(5)`, `make_adder(5)(10)`, `(x => x)(5)`):
  - Change AST: `Expr::Call(String, args)` -> `Expr::Call(ExprId, args)`
  - Parser: treat `(args)` as postfix operator in `fold_postfix`, like `.field` or `[idx]`
  - Interpreter: evaluate callee expression, then dispatch based on value type
  - Handle chained calls: `f(a)(b)` parses as `Call(Call(f, [a]), [b])`
- [x] Validate objects containing closures cannot be serialized:
  - `Interpreter::jsonify` recursively checks object values
  - If any value is `Value::Closure` or `Value::Function`, returns error
  - Error messages: `"closures cannot be serialized to JSON"` / `"functions cannot be serialized to JSON"`
- [x] Runtime type checking for function/closure params and return types:
  - `bind_params` validates arguments against param type annotations
  - `fn_value_matches` checks callable values against function type signatures
  - `check_return_type` validates return value against return type annotation
  - Clear error messages for type mismatches
- [x] Add unit tests (expression-based callees, type checking)
- [x] Add integration test script (`39_expression_callees.rumps`)

### 10. Pipeline Operator (`|>`)

Left-to-right function application.

```rumps
[1, 2, 3] |> MAP x => x * 2 |> FILTER x => x > 2

; Equivalent to:
FILTER (x => x > 2) (MAP (x => x * 2) [1, 2, 3])

; With named functions
value |> transform |> validate |> save
```

- [x] Add `Token::Pipe` (`|>`) to lexer
- [x] Add `BinOp::Pipe` to AST
- [x] Implement in interpreter:
  - Evaluate left operand (the value)
  - Evaluate right operand (should be a function/closure)
  - Apply right to left: `right(left)`
- [x] Left-associative, low precedence
- [x] Add unit tests
- [x] Add integration test script (`45_pipeline.rumps`)

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

**Binding scope:** Variables bound by `is` are only visible in the `then` branch of the `IF`. They do not leak into the `ELSE` branch or surrounding scope. This is critical for correctness; in the `ELSE` branch, the pattern did not match, so the bindings would have no valid value.

```rumps
IF x is Option.Some(val) {
  OUTPUT val           ; `val` is bound here
} ELSE {
  OUTPUT "none"        ; `val` is NOT in scope; using it here is an error
}
OUTPUT val             ; ERROR: `val` not in scope (binding expired)
```

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

### `as` vs `read` vs Implicit Coercion

Three levels of type conversion:

1. **Implicit coercion** (Phase 1): Automatic, safe widening in expressions like `Int + Float`.

2. **`as`** (infallible): Explicit conversions that always succeed but may lose precision:
   - `Float as Int` (truncates)
   - `42 as Float` (widens)
   - `T as String` (stringify anything)
   - `Bool as Int` (`false` -> `0`, `true` -> `1`)

3. **`read`** (fallible): Conversions that may fail; returns `Result[T, String]`:
   - `"42" read Int` -> `Result.Ok(42)`
   - `"bad" read Int` -> `Result.Err("invalid integer: bad")`
   - `1 read Bool` -> `Result.Ok(true)` (only `0` and `1` valid)

Use `read` when parsing user input or data that might be malformed. Chain with `??` for defaults or `is Result.Err(e)` to handle errors.

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
  ; `e` is NOT in scope here
}

; Chained pattern matching with ELSE
LET maybe = Option.None
IF maybe is Option.Some(v) {
  OUTPUT "Got: " ++ (v as String)
} ELSE {
  OUTPUT "Nothing there"
}

; Wildcard: check variant without binding
IF opt is Option.Some(_) {
  OUTPUT "Has some value"
}

; Type casting (infallible)
LET s = 3.14 as String
OUTPUT "Pi is " ++ s

LET f = 42 as Float
OUTPUT f + 0.5  ; 42.5

; Fallible conversion with read
LET parsed = "123" read Int
IF parsed is Result.Ok(n) {
  OUTPUT n + 1  ; 124
}

LET bad = "abc" read Int
IF bad is Result.Err(e) {
  OUTPUT "Error: " ++ e  ; "Error: invalid integer: abc"
}

; read with ?? for defaults
LET port = "8080" read Int ?? 3000
OUTPUT port  ; 8080

LET fallback = "invalid" read Int ?? 3000
OUTPUT fallback  ; 3000

; Strict bool conversion
LET b = 1 read Bool ?? false
OUTPUT b  ; true

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
```

## Implementation Notes

### Parser Refactoring: CST Intermediate Representation

The parser was refactored to use a two-pass architecture:

```text
Tokens  -->  CST (owned, boxed)  -->  AST (arena-allocated)
             ^^^^^^^^^^^^^^^^^^       ^^^^^^^^^^^^^^^^^^^^^
             chumsky produces         lowering pass produces
```

**Rationale**: Chumsky parsers must implement `Clone`, which forced the previous implementation to use `Rc<RefCell<Ast>>` with pervasive `Rc::clone()` calls (62+ clones, numbered variables like `ast2`, `ast3`, etc.). The CST decouples parsing from arena allocation:

1. **Parsing**: Chumsky parsers return owned CST nodes (`cst::Expr`, `cst::Stmt`, `cst::TypeExpr`). Since CST uses `Box<T>` for recursion, no shared state is needed.

2. **Lowering**: A single pass (`lower.rs`) converts CST to AST with direct `&mut Ast` access. No `Rc` cloning, no numbered variables.

**Files added** (under `parser/` submodule):
- `parser/cst.rs`: CST type definitions mirroring AST but with `Box<T>` recursion
- `parser/lower.rs`: CST to AST lowering pass

**Trade-offs**:
- Extra heap allocation for CST nodes before lowering (negligible for typical program sizes)
- Two traversals instead of one (negligible; I/O dominates in DB query languages)
- Duplicate type definitions (intentional separation of concerns)

The public API (`Parser::parse`, `ParseResult`) remains unchanged.
