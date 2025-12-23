# Phase 4: Static Type System

Add a Hindley-Milner style type inference and checking phase. The type checker runs after name resolution but before interpretation, rejecting programs with type errors at compile time.

## Design Decisions

| Decision         | Choice                                   | Rationale                                                                              |
|------------------|------------------------------------------|----------------------------------------------------------------------------------------|
| DB operations    | Infer from usage, fallback to annotation | `LET x = GET local(1); x + 1` infers `Int`; ambiguous cases require `LET x: T` or `AS` |
| Object typing    | Structural                               | Objects compatible if they have required fields                                        |
| Error handling   | Reject at compile                        | Type errors prevent execution                                                          |
| Numeric coercion | Float result                             | `Int + Float = Float` (widening)                                                       |
| Type erasure     | None                                     | Runtime type info preserved for `IS` operator                                          |

## Pipeline Integration

```
Lexer -> CST -> AST -> Name Resolution -> [TYPE CHECK] -> Interpreter
```

---

## Phase 4.1: Foundation

Create the core type representation and infrastructure.

### File Structure
```
crates/rumps-query/src/typecheck.rs   -- pub fn check(ast, registry) -> Result<(), Vec<TypeError>>
crates/rumps-query/src/typecheck/
  ty.rs                               -- Ty, TyVar, Scheme, Subst
  env.rs                              -- TypeEnv (scoped type bindings)
  error.rs                            -- TypeError enum
```

### Type Representation (`ty.rs`)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TyVar(u32);

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Var(TyVar),
    Bool, Int, Float, Char, String, Unit, Time, Range,
    Array(Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Object(BTreeMap<String, Ty>),
    Named(TypeId, Vec<Ty>),  // user-defined sum types
    Unknown,                  // database reads before inference
    Error,                    // error recovery
}

pub struct Scheme {
    pub vars: Vec<TyVar>,
    pub ty: Ty,
}

pub struct Subst(HashMap<TyVar, Ty>);
```

### Checklist

- [ ] Create `typecheck.rs` with module declarations
- [ ] Create `typecheck/ty.rs`:
  - [ ] `TyVar` newtype
  - [ ] `Ty` enum with all variants
  - [ ] `impl Ty`:
    - [ ] `fn free_vars(&self) -> HashSet<TyVar>`
    - [ ] `fn occurs(&self, v: TyVar) -> bool`
    - [ ] `fn apply(&self, subst: &Subst) -> Ty`
  - [ ] `Scheme` struct
  - [ ] `impl Scheme`:
    - [ ] `fn mono(ty: Ty) -> Self`
    - [ ] `fn instantiate(&self, ctx: &mut InferCtx) -> Ty`
  - [ ] `Subst` struct
  - [ ] `impl Subst`:
    - [ ] `fn empty() -> Self`
    - [ ] `fn singleton(v: TyVar, ty: Ty) -> Self`
    - [ ] `fn apply(&self, ty: &Ty) -> Ty`
    - [ ] `fn compose(&self, other: &Subst) -> Subst`
    - [ ] `fn extend(&mut self, v: TyVar, ty: Ty)`
- [ ] Create `typecheck/env.rs`:
  - [ ] `TypeEnv` struct with `scopes: Vec<HashMap<String, Scheme>>`
  - [ ] `impl TypeEnv`:
    - [ ] `fn new() -> Self`
    - [ ] `fn push_scope(&mut self)`
    - [ ] `fn pop_scope(&mut self)`
    - [ ] `fn bind(&mut self, name: &str, scheme: Scheme)`
    - [ ] `fn lookup(&self, name: &str) -> Option<&Scheme>`
    - [ ] `fn free_vars(&self) -> HashSet<TyVar>`
    - [ ] `fn generalize(&self, ty: &Ty) -> Scheme`
- [ ] Create `typecheck/error.rs`:
  - [ ] `TypeError` enum:
    - [ ] `Mismatch { expected: Ty, got: Ty, span: Span }`
    - [ ] `UndefinedVar(String, Span)`
    - [ ] `NotCallable(Ty, Span)`
    - [ ] `ArityMismatch { expected: usize, got: usize, span: Span }`
    - [ ] `NotNumeric(Ty, Span)`
    - [ ] `InfiniteType(TyVar, Ty, Span)`
    - [ ] `MissingAnnotation(Span)`
    - [ ] `UnknownType(String, Span)`
  - [ ] `impl Display for TypeError`

---

## Phase 4.2: Inference Context

Create the main inference context and constraint types.

### File Structure
```
crates/rumps-query/src/typecheck/
  infer.rs     -- InferCtx, constraint generation
  constraint.rs -- Constraint enum (optional, can be in infer.rs)
```

### Inference Context (`infer.rs`)

```rust
pub struct InferCtx<'a> {
    ast: &'a Ast,
    registry: &'a TypeRegistry,
    env: TypeEnv,
    constraints: Vec<Constraint>,
    next_var: u32,
    expr_types: HashMap<ExprId, Ty>,
    errors: Vec<TypeError>,
}

pub enum Constraint {
    Eq(Ty, Ty, Span),
    Numeric(Ty, Span),
    Callable { callee: Ty, args: Vec<Ty>, ret: Ty, span: Span },
}
```

### Checklist

- [ ] Create `typecheck/infer.rs`:
  - [ ] `Constraint` enum
  - [ ] `InferCtx` struct
  - [ ] `impl InferCtx`:
    - [ ] `fn new(ast: &Ast, registry: &TypeRegistry) -> Self`
    - [ ] `fn fresh_var(&mut self) -> TyVar`
    - [ ] `fn fresh(&mut self) -> Ty` (returns `Ty::Var(self.fresh_var())`)
    - [ ] `fn constrain(&mut self, c: Constraint)`
    - [ ] `fn unify(&mut self, t1: Ty, t2: Ty, span: Span)` (adds `Eq` constraint)
    - [ ] `fn record_type(&mut self, id: ExprId, ty: Ty)`
    - [ ] `fn error(&mut self, e: TypeError)`
- [ ] Add `mod infer` to `typecheck.rs`

---

## Phase 4.3: Literal and Variable Inference

Infer types for the simplest expressions.

### Inference Rules

| Expression      | Type                             |
|-----------------|----------------------------------|
| `true`, `false` | `Bool`                           |
| `42`            | `Int`                            |
| `3.14`          | `Float`                          |
| `'c'`           | `Char`                           |
| `"hello"`       | `String`                         |
| `Unit`          | `Unit`                           |
| `x` (variable)  | instantiate from `env.lookup(x)` |

### Checklist

- [ ] Add `fn infer_expr(&mut self, id: ExprId) -> Ty` to `InferCtx`
- [ ] Handle `Expr::Bool` -> `Ty::Bool`
- [ ] Handle `Expr::Int` -> `Ty::Int`
- [ ] Handle `Expr::Float` -> `Ty::Float`
- [ ] Handle `Expr::Char` -> `Ty::Char`
- [ ] Handle `Expr::String` -> `Ty::String`
- [ ] Handle `Expr::Var`:
  - [ ] Look up in `env`
  - [ ] If found, instantiate scheme with fresh vars
  - [ ] If not found, emit `TypeError::UndefinedVar`

---

## Phase 4.4: Operator Inference

Infer types for unary and binary operators.

### Binary Operator Rules

| Operator                 | Constraint                            | Result Type                                      |
|--------------------------|---------------------------------------|--------------------------------------------------|
| `+`, `-`, `*`, `%`, `**` | `Numeric(lhs)`, `Numeric(rhs)`        | `Float` if either is `Float`, else fresh numeric |
| `/`                      | `Numeric(lhs)`, `Numeric(rhs)`        | `Float`                                          |
| `//`                     | `Numeric(lhs)`, `Numeric(rhs)`        | `Int`                                            |
| `<`, `>`, `<=`, `>=`     | `lhs ~ rhs`                           | `Bool`                                           |
| `==`, `!=`               | `lhs ~ rhs`                           | `Bool`                                           |
| `&&`, `\|\|`             | `lhs ~ Bool`, `rhs ~ Bool`            | `Bool`                                           |
| `++`                     | `lhs ~ String`, `rhs ~ String`        | `String`                                         |
| `??`                     | `lhs ~ Option[?t]` or `Result[?t, _]` | `?t` (unify with `rhs`)                          |
| `\|>`                    | `Callable(rhs, [lhs], ?r)`            | `?r`                                             |
| `..`, `..=`              | `lhs ~ Int`, `rhs ~ Int`              | `Range`                                          |

### Unary Operator Rules

| Operator | Constraint         | Result Type     |
|----------|--------------------|-----------------|
| `-`      | `Numeric(operand)` | same as operand |
| `!`      | `operand ~ Bool`   | `Bool`          |

### Checklist

- [ ] Add `fn infer_binary(&mut self, lhs: ExprId, op: BinOp, rhs: ExprId, span: Span) -> Ty`
- [ ] Handle arithmetic ops (`Add`, `Sub`, `Mul`, `Mod`, `Pow`):
  - [ ] Add `Numeric` constraints for both operands
  - [ ] Result: fresh var with numeric constraint (or `Float` if either operand is `Float`)
- [ ] Handle `Div` -> always `Float`
- [ ] Handle `FloorDiv` -> always `Int`
- [ ] Handle comparison ops -> `Bool`
- [ ] Handle logical ops (`And`, `Or`) -> unify both with `Bool`, return `Bool`
- [ ] Handle `Concat` -> unify both with `String`, return `String`
- [ ] Handle `Coalesce`:
  - [ ] Check lhs is `Option[?t]` or `Result[?t, _]`
  - [ ] Unify rhs with `?t`
  - [ ] Return `?t`
- [ ] Handle `Pipe`:
  - [ ] Add `Callable` constraint
  - [ ] Return fresh var for result
- [ ] Handle `Range`, `RangeInclusive` -> `Range`
- [ ] Add `fn infer_unary(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> Ty`
- [ ] Handle `Neg` -> add `Numeric` constraint, return same type
- [ ] Handle `Not` -> unify with `Bool`, return `Bool`

---

## Phase 4.5: Collection Inference

Infer types for arrays, tuples, objects, and maps.

### Collection Rules

| Expression       | Type                       | Constraints                        |
|------------------|----------------------------|------------------------------------|
| `[a, b, c]`      | `Array[?t]`                | `a ~ ?t`, `b ~ ?t`, `c ~ ?t`       |
| `[]`             | `Array[?t]`                | (empty, `?t` is fresh)             |
| `(a, b, c)`      | `(?a, ?b, ?c)`             | -                                  |
| `{ x: a, y: b }` | `Object({ x: ?a, y: ?b })` | -                                  |
| `#{ k: v, ... }` | `Map[?k, ?v]`              | all keys ~ `?k`, all values ~ `?v` |

### Checklist

- [ ] Handle `Expr::Array`:
  - [ ] If empty, return `Ty::Array(fresh())`
  - [ ] Infer first element type `?t`
  - [ ] Unify all subsequent elements with `?t`
  - [ ] Return `Ty::Array(?t)`
- [ ] Handle `Expr::Tuple`:
  - [ ] Infer each element
  - [ ] Return `Ty::Tuple(vec![...])`
- [ ] Handle `Expr::Object`:
  - [ ] Infer each field value
  - [ ] Return `Ty::Object(BTreeMap { field: ty, ... })`
- [ ] Handle `Expr::MapLit`:
  - [ ] Infer key and value types
  - [ ] Unify all keys, unify all values
  - [ ] Return `Ty::Map(key_ty, val_ty)`

---

## Phase 4.6: Access and Indexing

Infer types for field access, tuple indexing, and array indexing.

### Access Rules

| Expression   | Type                  | Constraints                    |
|--------------|-----------------------|--------------------------------|
| `obj.field`  | `?t`                  | `obj` has field with type `?t` |
| `obj.?field` | `Option[?t]`          | optional field access          |
| `tuple.0`    | element type at index | -                              |
| `arr[i]`     | `?t`                  | `arr ~ Array[?t]`, `i ~ Int`   |

### Checklist

- [ ] Handle `Expr::Field`:
  - [ ] If base is `Object`, look up field type
  - [ ] If base is `Unknown`, create `Object({ field: ?t })` constraint
  - [ ] If field missing, emit error
- [ ] Handle `Expr::OptionalField`:
  - [ ] Same as `Field` but wrap result in `Option[?t]`
- [ ] Handle `Expr::TupleIndex`:
  - [ ] Check base is `Tuple`
  - [ ] Extract type at index (error if out of bounds)
- [ ] Handle `Expr::Index`:
  - [ ] Check base is `Array[?t]` or `Map[?k, ?v]`
  - [ ] For array: unify index with `Int`, return `?t`
  - [ ] For map: unify index with `?k`, return `?v`

---

## Phase 4.7: Function and Closure Inference

Infer types for closures, function calls, and function definitions.

### Function Rules

| Expression          | Type                                |
|---------------------|-------------------------------------|
| `\(x, y) -> body`   | `Fn([?x, ?y], ?body)`               |
| `\(x: Int) -> body` | `Fn([Int], ?body)`                  |
| `f(a, b)`           | `?r` with `Callable(f, [a, b], ?r)` |

### Checklist

- [ ] Handle `Expr::Closure`:
  - [ ] For each param: use annotation if present, else fresh var
  - [ ] Push scope, bind params
  - [ ] Infer body type
  - [ ] Pop scope
  - [ ] If return annotation present, unify body with it
  - [ ] Return `Ty::Fn(param_types, body_type)`
- [ ] Handle `Expr::Call`:
  - [ ] Infer callee type
  - [ ] Infer arg types
  - [ ] Create fresh var `?r` for result
  - [ ] Add `Callable` constraint
  - [ ] Return `?r`
- [ ] Handle `Stmt::Fun`:
  - [ ] Extract param types (annotations or fresh)
  - [ ] Push scope, bind params
  - [ ] Infer body
  - [ ] Pop scope
  - [ ] If return annotation, unify
  - [ ] Generalize and bind function name in env

---

## Phase 4.8: Control Flow

Infer types for conditionals, blocks, and match expressions.

### Control Flow Rules

| Expression                | Type        | Constraints                    |
|---------------------------|-------------|--------------------------------|
| `IF c THEN a ELSE b`      | `?t`        | `c ~ Bool`, `a ~ ?t`, `b ~ ?t` |
| `BLOCK { ...; e }`        | type of `e` | -                              |
| `MATCH e { p => b, ... }` | `?t`        | all branches ~ `?t`            |

### Checklist

- [ ] Handle `Expr::If`:
  - [ ] Unify condition with `Bool`
  - [ ] Infer both branches
  - [ ] Unify branches together
  - [ ] Return unified type
- [ ] Handle `Expr::Block`:
  - [ ] Push scope
  - [ ] Infer all statements
  - [ ] If final expr, return its type
  - [ ] Else return `Unit`
  - [ ] Pop scope
- [ ] Handle `Expr::Match`:
  - [ ] Infer scrutinee
  - [ ] For each arm:
    - [ ] Check pattern against scrutinee type
    - [ ] Bind pattern variables
    - [ ] Infer body
  - [ ] Unify all arm bodies
  - [ ] Return unified type

---

## Phase 4.9: Variant and Sum Type Inference

Infer types for variant constructors and pattern matching.

### Variant Rules

| Expression       | Type                      |
|------------------|---------------------------|
| `Option.None`    | `Option[?t]` (fresh)      |
| `Option.Some(x)` | `Option[typeof(x)]`       |
| `Result.Ok(x)`   | `Result[typeof(x), ?e]`   |
| `Result.Err(e)`  | `Result[?t, typeof(e)]`   |
| User variant     | `Named(TypeId, [params])` |

### Checklist

- [ ] Handle `Expr::Variant`:
  - [ ] Look up type and variant in registry
  - [ ] Infer arg types
  - [ ] Match against variant arity
  - [ ] For `Option`/`Result`: construct parameterized type
  - [ ] For user types: construct `Ty::Named`
- [ ] Handle pattern matching on variants:
  - [ ] Extract payload types from scrutinee
  - [ ] Bind to pattern variables

---

## Phase 4.10: Special Expressions

Handle unwrap, IS, AS, READ, GET, and other special cases.

### Special Rules

| Expression     | Type                | Notes                                  |
|----------------|---------------------|----------------------------------------|
| `e!` (unwrap)  | `?t`                | `e ~ Option[?t]` or `Result[?t, _]`    |
| `e IS T`       | `Bool`              | runtime check, no narrowing            |
| `e AS T`       | `T`                 | infallible cast                        |
| `e READ T`     | `Result[T, String]` | fallible conversion                    |
| `GET local(k)` | `Unknown`           | requires annotation or usage inference |

### Checklist

- [ ] Handle `Expr::Unwrap`:
  - [ ] Check operand is `Option[?t]` or `Result[?t, _]`
  - [ ] Return `?t`
- [ ] Handle `Expr::Is`:
  - [ ] Always returns `Bool`
  - [ ] No type narrowing (runtime check)
- [ ] Handle `Expr::As`:
  - [ ] Parse target type from annotation
  - [ ] Return target type (no constraint, runtime coercion)
- [ ] Handle `Expr::Read`:
  - [ ] Parse target type
  - [ ] Return `Result[T, String]`
- [ ] Handle `Expr::Get`:
  - [ ] Return `Ty::Unknown`
  - [ ] Narrowing happens via usage constraints

---

## Phase 4.11: Statements

Infer types for all statement types.

### Statement Rules

| Statement          | Effect                                 |
|--------------------|----------------------------------------|
| `LET x = e`        | bind `x` to `typeof(e)` in env         |
| `LET x: T = e`     | unify `typeof(e) ~ T`, bind `x` to `T` |
| `SET local(k) = e` | no env binding (db write)              |
| `OUTPUT e`         | infer `e`, no constraint on type       |
| `TYPE T = ...`     | register in type registry              |

### Checklist

- [ ] Add `fn infer_stmt(&mut self, id: StmtId)` to `InferCtx`
- [ ] Handle `Stmt::Let`:
  - [ ] Infer RHS
  - [ ] If type annotation, unify
  - [ ] Generalize and bind in env
- [ ] Handle `Stmt::Set`:
  - [ ] Infer subscripts and value
  - [ ] No env binding
- [ ] Handle `Stmt::Output`:
  - [ ] Infer expression
- [ ] Handle `Stmt::Fun`:
  - [ ] (already covered in Phase 4.7)
- [ ] Handle `Stmt::Type`:
  - [ ] Register type definition
  - [ ] No inference needed

---

## Phase 4.12: Unification Algorithm

Implement constraint solving.

### Unification Rules

```
unify(Var(v), t) = { v -> t } if v not in fv(t)
unify(t, Var(v)) = { v -> t } if v not in fv(t)
unify(Int, Float) = {} (coercion)
unify(Float, Int) = {} (coercion)
unify(Array(a), Array(b)) = unify(a, b)
unify(Fn(p1, r1), Fn(p2, r2)) = unify(p1, p2) . unify(r1, r2)
unify(Object(f1), Object(f2)) = unify common fields (structural)
unify(Unknown, _) = {}
unify(_, Unknown) = {}
unify(T, T) = {}
unify(_, _) = error
```

### Checklist

- [ ] Create `typecheck/unify.rs`
- [ ] Add `fn unify_types(&mut self, t1: &Ty, t2: &Ty, span: Span) -> Result<Subst, ()>`
- [ ] Handle `Var` binding (with occurs check)
- [ ] Handle primitive equality
- [ ] Handle numeric coercion (`Int` ~ `Float`)
- [ ] Handle `Array`, `Option`, `Result`, `Map` recursively
- [ ] Handle `Tuple` (element-wise, same length)
- [ ] Handle `Fn` (params + return)
- [ ] Handle `Object` (structural, common fields only)
- [ ] Handle `Named` (same TypeId, unify params)
- [ ] Handle `Unknown` (unifies with anything)
- [ ] Handle `Error` (unifies with anything, for recovery)
- [ ] Add `fn solve_constraints(&mut self) -> Subst`:
  - [ ] Process `Eq` constraints via unification
  - [ ] Process `Numeric` constraints (check resolved type is `Int` or `Float`)
  - [ ] Process `Callable` constraints (unify with `Fn` type)
  - [ ] Compose all substitutions

---

## Phase 4.13: Builtin Function Types

Register type schemes for all primitive/module functions.

### Example Signatures

```
; Array module
Array.len:    forall a. (Array[a]) -> Int
Array.map:    forall a b. (Array[a], (a) -> b) -> Array[b]
Array.filter: forall a. (Array[a], (a) -> Bool) -> Array[a]
Array.fold:   forall a b. (Array[a], b, (b, a) -> b) -> b

; Option module
Option.map:      forall a b. (Option[a], (a) -> b) -> Option[b]
Option.unwrap:   forall a. (Option[a]) -> a
Option.unwrap-or: forall a. (Option[a], a) -> a

; Object module
Object.keys:   (Object) -> Array[String]
Object.values: forall a. (Object) -> Array[a]  // or Unknown

; Math module
Math.abs:   (Float) -> Float
Math.sqrt:  (Float) -> Float
Math.floor: (Float) -> Int
```

### Checklist

- [ ] Create `typecheck/builtins.rs`
- [ ] Add `fn register_builtins(env: &mut TypeEnv, ctx: &mut InferCtx)`
- [ ] Register `Array` module functions
- [ ] Register `Option` module functions
- [ ] Register `Result` module functions
- [ ] Register `Object` module functions
- [ ] Register `String` module functions
- [ ] Register `Math` module functions
- [ ] Register `Time` module functions
- [ ] Register `Map` module functions

---

## Phase 4.14: Integration and Entry Point

Hook type checking into the interpreter pipeline.

### Entry Point (`typecheck.rs`)

```rust
pub fn check(ast: &Ast, registry: &TypeRegistry) -> Result<(), Vec<TypeError>> {
    let mut ctx = InferCtx::new(ast, registry);
    ctx.register_builtins();

    ast.stmt_ids().for_each(|id| ctx.infer_stmt(id));

    let subst = ctx.solve_constraints();
    ctx.apply_subst(&subst);
    ctx.check_remaining_unknowns();

    if ctx.errors.is_empty() {
        Ok(())
    } else {
        Err(ctx.errors)
    }
}
```

### Checklist

- [ ] Add `pub fn check(ast, registry) -> Result<(), Vec<TypeError>>` to `typecheck.rs`
- [ ] Modify `crates/rumps-query/src/lib.rs`:
  - [ ] Add `mod typecheck;`
- [ ] Modify `crates/rumps-query/src/interpreter.rs`:
  - [ ] Call `crate::typecheck::check(ast, &registry)?` after resolution
- [ ] Modify `crates/rumps-query/src/error.rs`:
  - [ ] Add `TypeErrors(Vec<TypeError>)` variant or integrate

---

## Phase 4.15: Error Messages and Diagnostics

Improve error messages with suggestions and context.

### Checklist

- [ ] Add span information to all error types
- [ ] Implement `Display` for `Ty` (pretty-print types)
- [ ] Add "expected X, got Y" format for mismatches
- [ ] Add suggestions for common mistakes:
  - [ ] "Did you mean to annotate this with `: T`?"
  - [ ] "Cannot use `+` on String; use `++` for concatenation"
  - [ ] "Array elements must have the same type"
- [ ] Integrate with `miette` for nice error rendering
- [ ] Add source snippets in error output

---

## Phase 4.16: Testing

Comprehensive test suite for the type checker.

### Test Categories

1. **Literals and variables**: all primitive types infer correctly
2. **Operators**: numeric, comparison, logical, string
3. **Collections**: array homogeneity, tuple, object, map
4. **Functions**: closures, calls, higher-order
5. **Control flow**: if/else, match, blocks
6. **Variants**: Option, Result, user-defined
7. **Errors**: type mismatches, undefined vars, arity
8. **Inference**: polymorphic functions, generalization
9. **Database**: GET with annotation, usage inference

### Checklist

- [ ] Create `crates/rumps-query/src/typecheck/tests.rs`
- [ ] Add unit tests for each `Ty` method
- [ ] Add unit tests for `Subst` operations
- [ ] Add unit tests for `TypeEnv` scoping
- [ ] Add integration tests for expression inference
- [ ] Add integration tests for statement inference
- [ ] Add integration tests for unification
- [ ] Add error case tests
- [ ] Add polymorphism tests
- [ ] Add end-to-end tests with `.rumps` scripts
  - [ ] Fix existing `.rumps` scripts (many will break)

---

## Critical Files to Modify

1. `crates/rumps-query/src/lib.rs` - add `mod typecheck`
2. `crates/rumps-query/src/interpreter.rs` - call type checker
3. `crates/rumps-query/src/error.rs` - add `TypeError` variant or integrate

## Critical Files to Read Before Implementation

1. `crates/rumps-query/src/ast.rs` - AST structure for traversal
2. `crates/rumps-query/src/value.rs` - `TypeId`, `TypeRegistry` for user types
3. `crates/rumps-query/src/resolve.rs` - resolution pass pattern to follow
4. `crates/rumps-query/src/interpreter/types.rs` - runtime type helpers for reference
5. `crates/rumps-query/src/primitives.rs` - builtin function signatures to type
