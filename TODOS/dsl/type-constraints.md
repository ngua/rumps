# Type Parameter Constraints

This document describes the plan for adding user-facing type parameter constraints to the RUMPS query language.

## Overview

Currently, users can write parametrically polymorphic functions but cannot constrain type parameters:

```rumps
FUN f[T](x: T) -> String { ... }  ; T is unconstrained
```

This plan adds constraint syntax:

```rumps
FUN f[T: Into[Json], U](x: T, y: U) -> Json { ... }
```

## Design Decisions

### Constraints to Expose

The typechecker has the following internal constraints (defined at `crates/rumps-query/src/typecheck/infer.rs:50`):

| Constraint    | Description                           | Expose?   | Rationale                                        |
|---------------|---------------------------------------|-----------|--------------------------------------------------|
| `Numeric`     | Type is `Int` or `Float`              | **Yes**   | Useful for generic numeric functions             |
| `Into[U]`     | Type can be converted to `U`          | **Yes**   | Useful for formatting/serialization functions    |
| `Subscriptable` | Type can be a DB subscript key      | **Yes**   | Useful for DB abstraction functions              |
| `Storable`    | Type can be stored in DB              | **Yes**   | Useful for DB abstraction functions              |
| `Iterable[T]` | Type is `Array[T]` or `Range`         | **Yes**   | Useful for generic collection functions          |
| `Fallible[T]` | Type is `Option[T]` or `Result[T, E]` | **Yes**   | Useful for error handling abstractions           |
| `Eq`          | Two types must be equal               | **No**    | Internal unification; like Haskell's `~`         |
| `Callable`    | Type is callable                      | **No**    | Internal; hard to express arity/signature        |
| `HasField`    | Type has a specific field             | **No**    | Internal; could expose later as row polymorphism |

### Syntax

Use the `T: Constraint` syntax, familiar from Rust. Parameterized constraints like `Into[U]`,
`Iterable[T]`, and `Fallible[T]` take a type argument:

```rumps
; Single constraint
FUN f[T: Numeric](x: T, y: T) -> T { x + y }

; Into constraint (conversion)
FUN to-str[T: Into[String]](x: T) -> String { x AS String }

; Multiple constraints with `+`
FUN numeric-to-str[T: Numeric + Into[String]](x: T) -> String { (x + x) AS String }

; Mixed constrained and unconstrained
FUN pair[T: Storable, U](x: T, y: U) -> (T, U) { (x, y) }

; Closures too
LET f = [T: Numeric](x: T, y: T) -> T => x * y
```

### AST Representation

Currently type params are `SmallVec<[String; 2]>`. Change to include constraints:

```rust
// New type for a type parameter with optional constraints
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TypeParam {
    pub name: String,
    pub constraints: SmallVec<[ParamConstraint; 2]>,
}

// User-facing constraint enum (subset of internal Constraint)
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ParamConstraint {
    Numeric,
    Subscriptable,
    Storable,
    Iterable(AstTypeExprId),
    Fallible(AstTypeExprId),
    Into(AstTypeExprId),   // Target type; constrained param is the source
}
```

## Implementation Plan

### Phase 1: Lexer & Token Updates

1. Add constraint name tokens or use existing `Ident` tokens
2. The `+` token is already available (currently used for arithmetic)

**Files**: `crates/rumps-query/src/lexer.rs`, `crates/rumps-query/src/token.rs`

### Phase 2: CST Updates

1. Add `TypeParam` struct to `crates/rumps-query/src/parser/cst.rs`
2. Add `ParamConstraint` enum to CST
3. Update `StmtKind::Fun`, `StmtKind::Type`, `StmtKind::Union` to use `Vec<TypeParam>` instead of `Vec<String>`
4. Update `ExprKind::Closure` similarly

**Files**: `crates/rumps-query/src/parser/cst.rs`

### Phase 3: Parser Updates

1. Update type parameter parsing in `fun_stmt`, `type_stmt`, `union_stmt`, closure parsing
2. Parse `IDENT` optionally followed by `:` then constraint list
3. Parse constraint list as `IDENT ('+' IDENT)*`

Current type param parsing (at `crates/rumps-query/src/parser.rs:455`):
```rust
let type_params = just(Token::LBracket)
    .ignore_then(Self::opt_newlines())
    .ignore_then(Self::ident().separated_by(type_param_sep).at_least(1).allow_trailing())
    // ...
```

New pattern: parse simple constraints (e.g., `Numeric`, `Storable`) and parameterized
constraints (e.g., `Iterable[T]`, `Fallible[T]`, `Into[U]`). Simple constraints have no
arguments, parameterized constraints have a type argument in brackets.

**Files**: `crates/rumps-query/src/parser.rs`

### Phase 4: AST Updates

1. Add `TypeParam` and `ParamConstraint` to `crates/rumps-query/src/ast.rs`
2. Update `Stmt::Fun`, `Stmt::Type`, `Stmt::Union` to use `SmallVec<[TypeParam; 2]>`
3. Update `Expr::Closure` similarly

**Files**: `crates/rumps-query/src/ast.rs`

### Phase 5: Lowering Updates

1. Update `crates/rumps-query/src/parser/lower.rs` to convert CST `TypeParam` to AST `TypeParam`
2. Should be straightforward mapping

**Files**: `crates/rumps-query/src/parser/lower.rs`

### Phase 6: Typechecker Updates

The key change: when creating fresh type variables for type params, also emit constraints.

Current code at `crates/rumps-query/src/typecheck/infer/stmt.rs:212`:
```rust
let type_param_subst: HashMap<_, _> = type_params
    .iter()
    .map(|tp| {
        let id = self.env.intern(tp);
        let tv = self.fresh();
        (id, tv)
    })
    .collect();
```

New pattern: emit constraints for each type parameter based on the constraint kind.
For `T: Into[U]`, emit a `Constraint::Into { from: T, to: U, span }` constraint.

Same pattern needed in:
- `crates/rumps-query/src/typecheck/infer/stmt.rs` (for `FUN`)
- `crates/rumps-query/src/typecheck/infer/expr.rs` (for closures)

**Files**:
- `crates/rumps-query/src/typecheck/infer/stmt.rs`
- `crates/rumps-query/src/typecheck/infer/expr.rs`

### Phase 7: Tests

Add test scripts in `crates/rumps-query/scripts/`:

1. Basic constraint usage
2. Multiple constraints on one param
3. Mix of constrained and unconstrained params
4. Error cases: unknown constraint name, constraint violation

### Phase 8: Documentation

1. Update any DSL documentation
2. Add examples to module docs

## Why Not Expose Certain Constraints

### `Eq` (Not Exposed)

The `Eq` constraint is the internal unification constraint, analogous to Haskell's `~` type equality operator. It says "these two types must be equal."

**Why not expose**:
- Users don't need to express "T equals U"; they can just use the same type parameter
- Exposing it would complicate the constraint system without clear benefit
- The syntax `T ~ U` or `T = U` would be confusing with value equality

### `Callable` (Not Exposed)

The `Callable` constraint says "this type can be called with these argument types and returns this result type."

**Why not expose**:
- Would need complex syntax: `T: Callable[(Int, Int) -> String]`
- Users can already write function types directly: `f: (Int, Int) -> String`
- No clear use case for "any callable" without specifying signature

### `HasField` (Not Exposed Yet)

The `HasField` constraint says "this type has a field named X of type Y."

**Why not expose (yet)**:
- Internal constraint for field access on type variables
- Could be exposed later as row polymorphism: `T: { name: String, ... }`
- Requires more syntax design work

## Future Work

1. **Row polymorphism**: Expose `HasField` with syntax like `T: { field: Type }`
2. **Custom constraints**: Allow users to define their own constraint aliases

## Example Use Cases

```rumps
; Generic sum function
FUN sum[T: Numeric](arr: Array[T]) -> T {
    Array.fold(0, (acc, x) => acc + x, arr)
}

; Generic serialize and log
FUN log-json[T: Into[String] + Into[Json]](label: String, val: T) {
    @OUTPUT label ++ ": " ++ (val AS Json)
}

; Generic DB utilities
FUN cache-get[K: Subscriptable, V: Storable](key: K) -> Option[V] {
    @GET cache(key)
}

; Generic collection transform (elements must be convertible to String)
FUN map-to-string[T: Into[String], C: Iterable[T]](coll: C) -> Array[String] {
    Array.map(x => x AS String, coll)
}
```
