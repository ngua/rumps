# Phase 4c: Pre-Merge Improvements

Items to complete before merging the typechecker branch into `rumps-query`.

**NOTE**: For typechecker, do **not** add unit tests. The typechecker must exercise the full pipeline (source -> lex -> parse -> CST -> AST -> resolve -> typecheck). Add a new integration test (RUMPS script + snapshot) FOR ALL items to test.

---

## 1. Generic User-Defined Functions and Closures

User-defined functions and closures should support type parameters, mirroring the polymorphism already available in `ModuleFn`s.

### Goals

- Allow syntax like `fn foo<T>(x: T) -> T { ... }`
- Closures should infer or accept explicit type parameters
- Unify instantiation logic between `ModuleFn` and user-defined generics

### Implementation

- [x] Extend AST to support type parameter lists on `fn` declarations
- [x] Extend parser to accept `<T, U, ...>` after function name
- [x] Update `resolve` pass to track type parameter scope
- [x] Extract common instantiation logic from `ModuleFn` handling into shared utility
- [x] Implement instantiation for user-defined generic functions using extracted logic
- [x] Support generic closures (infer type params from usage context)
- [x] Add test scripts covering:
  - [x] Simple identity function `fn id<T>(x: T) -> T`
  - [x] Multiple type parameters `fn pair<A, B>(a: A, b: B) -> (A, B)`
  - [x] Generic closures passed to higher-order functions
  - [x] Error cases: unused type params, constraint violations

---

## 2. Prefix `?` for `Some` Wrapping

Add a prefix `?` operator that wraps a value in `Some(_)`, providing ergonomic `Option` construction.

### Syntax

```rumps
let x = ?42        // Some(42)
let y = ?foo.bar   // Some(foo.bar)
```

### Implementation

- [x] Add `Token::Question` to lexer
- [x] Add `UnOp::Wrap` variant to AST
- [x] Update parser to handle prefix `?`
- [x] Typecheck: infer `?e : Option[T]` when `e : T`
- [x] Interpreter: wrap value in `Value::Some(_)` via `make_some`
- [x] Add test cases in `25_option_values.rumps`

**Note**: `??x` is lexed as the coalesce operator (`??`) followed by `x`, not as nested wrap. For nested wrapping, use explicit parentheses: `?(?x)`.

---

## 3. Modules

### 3.0. Modules Definitions

We need to allow users to define modules. Currently, we have several builtin modules (e.g. `Array`, `Map`, `Time`, etc...) that use the `Module` type. 

We need a way to allow _users_ to define modules. **NOTE**: modules can be recursive! I.e. contain submodules. See `Module` type, crates/rumps-query/src/env.rs:213

Syntax:

```
MODULE M {
  ; Any `FUN` (function) definition is registered as
  ; `Module.functions` and `Modules.types` (i.e the function and its type)
  ; NOTE: There MUST be a corresponding type for a `ModuleFn`
  FUN f(x: Int) -> Int { 
    x ** 2
  }
  
  ; Any `LET` bindings are registered in `Modules.constants` and `Module.const_types`
  ; NOTE: There MUST be a corresponding type for module constants
  LET x = 10
  
  ; Nested modules are supported; these go into `Module.submodules`
  ; and follow the same rules above regarding top-level modules
  Module N {
    ; ...
  }
  
  
}
```

This is largely a lexing (e.g. `Token::Module`), parsing, and environment/interpreter change.

Add a RUMPS integration test script when done that creates and accesses user-defined modules.

### 3.1 Modules Containing Type Definitions

Modules should be able to contain anything the top-level can, including type and struct definitions.

### Goals

- `MODULE Foo { TYPE Bar = ... }` works
- Types are accessible as `Foo.Bar`
- Structs, enums, unions all supported inside modules

### Implementation

- [x] Extend module AST node to include type definitions
  - Parser already accepts `TYPE`/`UNION` inside `MODULE`; validated at typecheck
- [x] Update parser to accept `TYPE` and `UNION` inside `MODULE { ... }`
  - Added qualified type name support (`Module.Type`) in type expressions
  - Added qualified variant patterns (`Module.Type.Variant`) in match patterns
- [x] Update resolve pass to register module-scoped types
  - Types are registered with qualified names (e.g., `Shapes.Shape`)
  - Variant resolution handles `Module.Type.Variant` paths
- [x] Update typecheck to look up `Mod.Type` paths
  - TypeRegistry stores qualified names; lookup works transparently
- [x] Ensure nested modules work (if supported)
  - Nested modules work: `Outer.Inner.Type.Variant`
- [x] Add test scripts:
  - [x] Module with struct definition (`Users.User`)
  - [x] Module with enum/union definition (`Shapes.Shape`, `Data.Primitive`)
  - [x] Accessing module types from outside (all test cases)
  - [x] Generic types in modules (`Container.Box[T]`)

---

## 4. `Ordering` Type

Add a builtin `Ordering` enum for comparison results, enabling proper `sort-by` and comparison utilities.

### Definition

```rumps
TYPE Ordering = Lt | Eq | Gt
```

### Implementation

- [x] Add `Ordering` as builtin enum type
- [x] Register `Ordering`, `Lt`, `Eq`, `Gt` in global type/value environment
- [x] Ensure pattern matching on `Ordering` works
- [ ] Add `compare` primitives or method if needed (e.g., `Int.compare`)
- [x] Add test script demonstrating usage

---

## 5. Array Utilities

Extend `Array` module with additional functional utilities.

### New Functions

| Function      | Signature                                                     | Description                                             |
|---------------|---------------------------------------------------------------|---------------------------------------------------------|
| `sort-by`     | `forall T. ((T, T) -> Ordering, Array[T]) -> Array[T]`        | Sort array using comparator                             |
| `zip`         | `forall T U. (Array[T], Array[U]) -> Array[(T, U)]`           | Pair elements from two arrays                           |
| `zip-with`    | `forall T U V. ((T, U) -> V, Array[T], Array[U]) -> Array[V]` | Zip with combining function                             |
| `unzip`       | `forall T U. Array[(T, U)] -> (Array[T], Array[U])`           | Split array of pairs                                    |
| `sort`        | `forall T. Array[T] -> Array[T]`                              | Sort using default ordering (requires `Ord` or similar) |
| `reverse`     | `forall T. Array[T] -> Array[T]`                              | Reverse array order                                     |
| `intersperse` | `forall T. (T, Array[T]) -> Array[T]`                         | Insert element between each pair                        |

### Implementation

- [x] Implement `sort-by`
- [x] Implement `zip`
- [x] Implement `zip-with`
- [x] Implement `unzip`
- [x] Implement `reverse` (already existed)
- [x] Implement `intersperse`

---

## 6. `Io` Module

Add an `Io` module for effectful operations. All actual I/O should use `tokio` under the hood.

### Core Functions

| Function   | Signature          | Description                  |
|------------|--------------------|------------------------------|
| `get-line` | `Unit -> String`   | Read line from stdin         |
| `print`    | `(String) -> Unit` | Print to stdout (no newline) |
| `println`  | `(String) -> Unit` | Print to stdout with newline |
| `eprint`   | `(String) -> Unit` | Print to stderr              |
| `eprintln` | `(String) -> Unit` | Print to stderr with newline |

### Implementation

**NOTE**: Use `tokio` for everything!

- [x] Create `Io` module structure
- [x] Implement `get-line` using `tokio::io::stdin`
- [x] Implement `print` / `println`
- [x] Implement `eprint` / `eprintln`

---

## 7. `Io.Directory` Module (File System Operations)

Add file system utilities under `Io.Directory`.

### Builtin Types

#### `FilePath` (Opaque)

An opaque type representing a file system path.

```rumps
// Construction via type coercion
let p = "/some/path" as FilePath

// Or via IsString-like implicit coercion (if supported)
let p: FilePath = "/some/path"
```

#### `Path` (Enum)

A tagged sum type representing a filesystem entry. Must be an enum (not an untagged union) because both variants wrap `FilePath`; the tag carries runtime-discovered information about whether the path is a file or directory.

```rumps
TYPE Path = File(FilePath) | Dir(FilePath)
```

### Functions

| Function         | Signature                                        | Description                    |
|------------------|--------------------------------------------------|--------------------------------|
| `list-dir`       | `(FilePath) -> [Path]`                           | List directory contents        |
| `move-path`      | `({ src: FilePath, dest: FilePath }) -> Unit`    | Move/rename path               |
| `copy-path`      | `({ src: FilePath, dest: FilePath }) -> Unit`    | Copy path                      |
| `remove`         | `(FilePath) -> Unit`                             | Remove file or empty directory |
| `remove-all`     | `(FilePath) -> Unit`                             | Remove recursively             |
| `exists`         | `(FilePath) -> Bool`                             | Check if path exists           |
| `is-file`        | `(FilePath) -> Bool`                             | Check if path is a file        |
| `is-dir`         | `(FilePath) -> Bool`                             | Check if path is a directory   |
| `read-file`      | `(FilePath) -> String`                           | Read entire file as string     |
| `write-file`     | `({ path: FilePath, contents: String }) -> Unit` | Write string to file           |
| `append-file`    | `({ path: FilePath, contents: String }) -> Unit` | Append string to file          |
| `create-dir`     | `(FilePath) -> Unit`                             | Create directory               |
| `create-dir-all` | `(FilePath) -> Unit`                             | Create directory and parents   |
| `pwd`            | `Unit -> FilePath`                               | Get current working directory  |
| `set-pwd`        | `(FilePath) -> Unit`                             | Change working directory       |
| `get-env`        | `(String) -> Option[String]`                     | Get env var (if any)           |
| `set-env`        | `(String) -> Unit`                               | Set env var                    |
|                  |                                                  |                                |

### Implementation

**NOTE**: Use `tokio` for everything!
**NOTE**: Make sure to register the `Io.Directory` submodule under the existing `Io` module! See e.g. `Math.Trig` on how to do this (`with_submodule` pattern)

- [x] Add `FilePath` as opaque builtin type
- [x] Implement `String -> FilePath` coercion
- [x] Add `Path` enum type as builtin
  - [x] **NOTE**: Make sure `InferCtx::types_compatible` evaluates to `true`, otherwise `Array[FilePath]` will be inferred as `Json`!
    - Current location to add `true` along with other types: crates/rumps-query/src/typecheck/infer/convert.rs:188
- [x] Create `Io.Directory` submodule structure
- [x] Implement core functions using `tokio::fs`:
  - [x] `list-dir`
  - [x] `move-path` (object type manually constructed; `scheme!` doesn't support objects)
  - [x] `copy-path` (object type manually constructed; `scheme!` doesn't support objects)
  - [x] `remove` / `remove-all`
  - [x] `exists` / `is-file` / `is-dir`
  - [x] `read-file`
  - [x] `write-file` / `append-file` (object type manually constructed)
  - [x] `create-dir` / `create-dir-all`
  - [x] `pwd` / `set-pwd`
  - [x] `get-env`
  - [x] `set-env` (object type manually constructed)
- [x] Handle errors appropriately (runtime errors with descriptive messages)
- [x] Add comprehensive test scripts

---

## 8. Spread Operators (`...`)

*Deferred from Phase 3 pending typechecker implementation.*

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

### 8.1 Lexer

- [x] Add `Token::DotDotDot` (`...`) for spread operator

*Note: Already implemented as part of destructuring bindings for array rest patterns.*

### 8.2 AST

- [x] Add `ArrayElem` enum with `Elem(ExprId)` and `Spread(ExprId)` variants
- [x] Add `ObjectEntry` enum with `Field(String, ExprId)` and `Spread(ExprId)` variants
- [x] Modify `Expr::Array` to use `Vec<ArrayElem>`
- [x] Modify `Expr::Object` to use `Vec<ObjectEntry>`

### 8.3 Parser

- [x] Parse `...expr` inside array literals
- [x] Parse `...expr` inside object literals
- [x] Spread only valid inside array/object literals (not standalone)

### 8.4 Typechecking

Array spread is straightforward; object spread requires care with structs vs anonymous objects.

#### 8.4.1 Array Spread

- [x] `[...a, ...b]` where `a: [T]` and `b: [U]` requires `T` and `U` unifiable
- [x] Result type is `[unify(T, U)]`
- [x] Mixed elements and spreads: `[x, ...arr, y]` unifies `typeof(x)`, element type of `arr`, and `typeof(y)`

#### 8.4.2 Object Spread (Anonymous Objects)

- [x] `{ ...a, ...b }` merges field sets; later fields override earlier
- [x] For overridden fields, result type is the later field's type
- [x] Result is an anonymous object with union of all fields

#### 8.4.3 Object Spread with Structs

This is the tricky case. Consider:

```rumps
TYPE Person = { name: String, age: Int }
LET p: Person = { name: "Alice", age: 30 }
LET updated = { ...p, age: 31 }  ; What type is this?
```

Options:
1. **Always anonymous**: `{ ...p, age: 31 }` is always an anonymous object, even if `p` is a struct
2. **Preserve struct when compatible**: If all fields match and no extra fields added, preserve the struct type
3. **Explicit annotation required**: `LET updated: Person = { ...p, age: 31 }`

**Implemented approach**: Option 2 (preserve struct when compatible). This works well with RUMPS's extensible-record semantics, where structs only require their declared fields but can have additional fields. When the first spread is a struct and no conflicting fields exist before it, the struct type is preserved. Adding extra fields produces an anonymous object (which is still compatible with the original struct due to extensible records).

- [x] Spread of struct preserves struct type when first spread and no prior fields
- [x] Explicit type annotation on `LET` validates struct compatibility
- [x] Adding extra fields to spread produces anonymous object (compatible via extensible records)

### 8.5 Interpreter

- [x] Array spread: iterate source array, append elements to result
- [x] Object spread: iterate source object entries, insert into result
- [x] Later entries override earlier ones for objects

### 8.6 Tests

- [x] Add parser tests for spread in arrays and objects
- [x] Add typecheck tests for array spread unification
- [x] Add typecheck tests for object spread (anonymous)
- [x] Add typecheck tests for struct spread with/without annotation
- [x] Add interpreter tests
- [x] Add integration test script (`98_spread.rumps`)

---

## 9. Regex Pattern Matching (`MATCHES`)

*Deferred from Phase 3 pending typechecker implementation.*

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

### 9.1 Builtin `Regex` Type

Add `Regex` as an opaque builtin type for compiled regex patterns.

```rumps
; Regex literals compile to Regex values
LET pat = /\d{3}-\d{4}/
LET email-pat = /^[^@]+@[^@]+\.[^@]+$/

; Can be reused
LET nums = ["123-4567", "abc", "999-0000"]
Array.filter(s => s MATCHES pat, nums)  ; ["123-4567", "999-0000"]

; Or passed to functions
FUN validate (pattern: Regex, input: String) -> Bool {
  input MATCHES pattern
}
```

- [ ] Add `TypeId::REGEX` constant
- [ ] Add `BuiltinType::Regex` variant
- [ ] Register `Regex` in `TypeRegistry::register_builtins`
- [ ] Add `Value::Regex(CompiledRegex)` (wrapper around `regex::Regex`)

### 9.2 Lexer

- [ ] Add regex literal support (`/pattern/`)
  - Handle escape sequences (`\/`, `\\`)
  - Regex ends at unescaped `/`
- [ ] Add `Token::Regex(String)` for regex literals
- [ ] Add `Token::Matches` keyword

### 9.3 AST

- [ ] Add `Expr::Regex(String)` for regex literals (compiles to `Value::Regex`)
- [ ] Add `Expr::Matches(ExprId, ExprId)` (value, pattern); pattern can be any expr of type `Regex`

### 9.4 Typechecking

Typing rules:

- [ ] `/pattern/` has type `Regex`
- [ ] `e MATCHES r` requires `e : Stringable` and `r : Regex`
- [ ] Result type is always `Bool`
- [ ] Validate regex syntax when typing `Expr::Regex`; report compile error for invalid patterns

**Note**: This is a good use case for the `Stringable` constraint; any type that can be converted to a string should work with `MATCHES`.

### 9.5 Interpreter

- [ ] Add `regex` crate dependency
- [ ] Compile regex literals at interpretation time (or cache if same literal used multiple times)
- [ ] `Value::Regex` holds compiled `regex::Regex`
- [ ] Evaluate `MATCHES`: stringify left operand, test against regex, return `Bool`

### 9.6 Tests

- [ ] Add lexer tests for regex literals
- [ ] Add parser tests for `Expr::Regex` and `Expr::Matches`
- [ ] Add typecheck tests:
  - [ ] Regex literal has type `Regex`
  - [ ] `MATCHES` requires `Stringable` left operand and `Regex` right operand
  - [ ] Result is `Bool`
  - [ ] Invalid regex pattern is a type error
- [ ] Add interpreter tests
- [ ] Add integration test script (`XX_regex.rumps`)

**Deferred regex features** (for later):
- Named capture groups (`(?<name>...)`) and `Match.name` access
- Regex flags (`/pattern/i`, `/pattern/m`)

---

## Summary Checklist

High-level tracking:

- [x] Generic user-defined functions
- [x] Generic closures
- [x] Prefix `?` operator
- [x] Module type definitions
- [x] `Ordering` builtin type
- [x] `Array.sort-by`
- [x] `Array.zip` family
- [x] `Io` module (stdin/stdout)
- [x] `Io.Directory` module (file system)
- [x] `FilePath` opaque type
- [ ] Spread operators (`...`)
- [ ] Regex pattern matching (`MATCHES`)
