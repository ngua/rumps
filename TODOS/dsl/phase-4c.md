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

- [ ] Extend AST to support type parameter lists on `fn` declarations
- [ ] Extend parser to accept `<T, U, ...>` after function name
- [ ] Update `resolve` pass to track type parameter scope
- [ ] Extract common instantiation logic from `ModuleFn` handling into shared utility
- [ ] Implement instantiation for user-defined generic functions using extracted logic
- [ ] Support generic closures (infer type params from usage context)
- [ ] Add test scripts covering:
  - [ ] Simple identity function `fn id<T>(x: T) -> T`
  - [ ] Multiple type parameters `fn pair<A, B>(a: A, b: B) -> (A, B)`
  - [ ] Generic closures passed to higher-order functions
  - [ ] Error cases: unused type params, constraint violations

---

## 2. Prefix `?` for `Some` Wrapping

Add a prefix `?` operator that wraps a value in `Some(_)`, providing ergonomic `Option` construction.

### Syntax

```rumps
let x = ?42        // Some(42)
let y = ?foo.bar   // Some(foo.bar)
```

### Implementation

- [ ] Add `PrefixQuestion` token to lexer
- [ ] Add `Wrap` (or similar) variant to AST expression types
- [ ] Update parser to handle prefix `?`
- [ ] Typecheck: infer `?e : T?` when `e : T`
- [ ] Interpreter: wrap value in `Value::Some(_)`
- [ ] Add test scripts for basic usage and nested `??x` if desired

---

## 3. Modules Containing Type Definitions

Modules should be able to contain anything the top-level can, including type and struct definitions.

### Goals

- `module Foo { type Bar = ... }` works
- Types are accessible as `Foo.Bar`
- Structs, enums, unions all supported inside modules

### Implementation

- [ ] Extend module AST node to include type definitions
- [ ] Update parser to accept `type`, `struct`, `enum` inside `module { ... }`
- [ ] Update resolve pass to register module-scoped types
- [ ] Update typecheck to look up `Mod.Type` paths
- [ ] Ensure nested modules work (if supported)
- [ ] Add test scripts:
  - [ ] Module with struct definition
  - [ ] Module with enum/union definition
  - [ ] Accessing module types from outside
  - [ ] Error: undefined type in module

---

## 4. `Ordering` Type

Add a builtin `Ordering` enum for comparison results, enabling proper `sort-by` and comparison utilities.

### Definition

```rumps
enum Ordering { Lt, Eq, Gt }
```

### Implementation

- [ ] Add `Ordering` as builtin enum type
- [ ] Register `Ordering`, `Lt`, `Eq`, `Gt` in global type/value environment
- [ ] Ensure pattern matching on `Ordering` works
- [ ] Add `compare` primitives or method if needed (e.g., `Int.compare`)
- [ ] Add test script demonstrating usage

---

## 5. Array Utilities

Extend `Array` module with additional functional utilities.

### New Functions

| Function      | Signature                          | Description                                             |
|---------------|------------------------------------|---------------------------------------------------------|
| `sort-by`     | `((T, T) -> Ordering, [T]) -> [T]` | Sort array using comparator                             |
| `zip`         | `([A], [B]) -> [(A, B)]`           | Pair elements from two arrays                           |
| `zip-with`    | `((A, B) -> C, [A], [B]) -> [C]`   | Zip with combining function                             |
| `unzip`       | `[(A, B)] -> ([A], [B])`           | Split array of pairs                                    |
| `sort`        | `[T] -> [T]`                       | Sort using default ordering (requires `Ord` or similar) |
| `reverse`     | `[T] -> [T]`                       | Reverse array order                                     |
| `intersperse` | `(T, [T]) -> [T]`                  | Insert element between each pair                        |

### Implementation

- [ ] Implement `sort-by` (requires `Ordering` type first)
- [ ] Implement `zip`
- [ ] Implement `zip-with`
- [ ] Implement `unzip`
- [ ] Implement `reverse`
- [ ] Implement `intersperse`
- [ ] Consider `sort` with default ordering (may need traits/constraints)
- [ ] Add test scripts for each function

---

## 6. `Io` Module

Add an `Io` module for effectful operations. All actual I/O should use `tokio` under the hood.

### Core Functions

| Function   | Signature        | Description                  |
|------------|------------------|------------------------------|
| `get-line` | `() -> String`   | Read line from stdin         |
| `print`    | `(String) -> ()` | Print to stdout (no newline) |
| `println`  | `(String) -> ()` | Print to stdout with newline |
| `eprint`   | `(String) -> ()` | Print to stderr              |
| `eprintln` | `(String) -> ()` | Print to stderr with newline |

### Implementation

- [ ] Create `Io` module structure
- [ ] Implement `get-line` using `tokio::io::stdin`
- [ ] Implement `print` / `println`
- [ ] Implement `eprint` / `eprintln`
- [ ] Ensure interpreter supports async primitives properly
- [ ] Add test scripts (may need special test harness for I/O)

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
enum Path { File(FilePath), Dir(FilePath) }
```

Usage:

```rumps
Io.Directory.list-dir(dir) |> Array.each(|entry| {
  match entry {
    File(fp) => Io.println("file: " ++ fp.to-string)
    Dir(fp)  => Io.println("dir: " ++ fp.to-string)
  }
})
```

### Functions

| Function          | Signature                                      | Description                    |
|-------------------|------------------------------------------------|--------------------------------|
| `list-dir`        | `(FilePath) -> [Path]`                         | List directory contents        |
| `move-path`       | `({ src: FilePath, dest: FilePath }) -> ()`    | Move/rename path               |
| `copy-path`       | `({ src: FilePath, dest: FilePath }) -> ()`    | Copy path                      |
| `remove`          | `(FilePath) -> ()`                             | Remove file or empty directory |
| `remove-all`      | `(FilePath) -> ()`                             | Remove recursively             |
| `exists`          | `(FilePath) -> Bool`                           | Check if path exists           |
| `is-file`         | `(FilePath) -> Bool`                           | Check if path is a file        |
| `is-dir`          | `(FilePath) -> Bool`                           | Check if path is a directory   |
| `read-file`       | `(FilePath) -> String`                         | Read entire file as string     |
| `write-file`      | `({ path: FilePath, contents: String }) -> ()` | Write string to file           |
| `append-file`     | `({ path: FilePath, contents: String }) -> ()` | Append string to file          |
| `create-dir`      | `(FilePath) -> ()`                             | Create directory               |
| `create-dir-all`  | `(FilePath) -> ()`                             | Create directory and parents   |
| `current-dir`     | `() -> FilePath`                               | Get current working directory  |
| `set-current-dir` | `(FilePath) -> ()`                             | Change working directory       |

### Implementation

- [ ] Add `FilePath` as opaque builtin type
- [ ] Implement `String -> FilePath` coercion
- [ ] Add `Path` enum type as builtin
- [ ] Create `Io.Directory` submodule structure
- [ ] Implement core functions using `tokio::fs`:
  - [ ] `list-dir`
  - [ ] `move-path`
  - [ ] `copy-path`
  - [ ] `remove` / `remove-all`
  - [ ] `exists` / `is-file` / `is-dir`
  - [ ] `read-file` / `write-file` / `append-file`
  - [ ] `create-dir` / `create-dir-all`
  - [ ] `current-dir` / `set-current-dir`
- [ ] Handle errors appropriately (return `Result` or panic?)
- [ ] Add comprehensive test scripts

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

- [ ] Add `Expr::Spread(ExprId)` for spread expressions
- [ ] Modify `Expr::Array` to allow spread elements
- [ ] Modify `Expr::Object` to allow spread entries

### 8.3 Parser

- [ ] Parse `...expr` inside array literals
- [ ] Parse `...expr` inside object literals
- [ ] Spread only valid inside array/object literals (not standalone)

### 8.4 Typechecking

Array spread is straightforward; object spread requires care with structs vs anonymous objects.

#### 8.4.1 Array Spread

- [ ] `[...a, ...b]` where `a: [T]` and `b: [U]` requires `T` and `U` unifiable
- [ ] Result type is `[unify(T, U)]`
- [ ] Mixed elements and spreads: `[x, ...arr, y]` unifies `typeof(x)`, element type of `arr`, and `typeof(y)`

#### 8.4.2 Object Spread (Anonymous Objects)

- [ ] `{ ...a, ...b }` merges field sets; later fields override earlier
- [ ] For overridden fields, result type is the later field's type
- [ ] Result is an anonymous object with union of all fields

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

Recommended approach: Option 1 (always anonymous) with Option 3 (explicit annotation validates compatibility). This is simplest and most predictable.

- [ ] Spread of struct produces anonymous object with same fields
- [ ] Explicit type annotation on `LET` validates struct compatibility
- [ ] Error if spread result doesn't match annotated struct type

### 8.5 Interpreter

- [ ] Array spread: iterate source array, append elements to result
- [ ] Object spread: iterate source object entries, insert into result
- [ ] Later entries override earlier ones for objects

### 8.6 Tests

- [ ] Add parser tests for spread in arrays and objects
- [ ] Add typecheck tests for array spread unification
- [ ] Add typecheck tests for object spread (anonymous)
- [ ] Add typecheck tests for struct spread with/without annotation
- [ ] Add interpreter tests
- [ ] Add integration test script (`XX_spread.rumps`)

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

- [ ] Generic user-defined functions
- [ ] Generic closures
- [ ] Prefix `?` operator
- [ ] Module type definitions
- [ ] `Ordering` builtin type
- [ ] `Array.sort-by`
- [ ] `Array.zip` family
- [ ] `Io` module (stdin/stdout)
- [ ] `Io.Directory` module (file system)
- [ ] `FilePath` opaque type
- [ ] Spread operators (`...`)
- [ ] Regex pattern matching (`MATCHES`)
