# Phase 4c: Pre-Merge Improvements

Items to complete before merging the typechecker branch into `rumps-query`.

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
