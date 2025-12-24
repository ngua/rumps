# Phase 4: Static Type System

Add a Hindley-Milner style type inference and checking phase. The type checker runs after name resolution but before interpretation, rejecting programs with type errors at compile time.

## Design Decisions

| Decision         | Choice                                   | Rationale                                                                               |
|------------------|------------------------------------------|-----------------------------------------------------------------------------------------|
| DB operations    | Infer from usage, fallback to `Storable` | `LET x = GET local(1); x + 1` infers `Int`; ambiguous cases resolve to `Storable` union |
| Object typing    | Structural                               | Objects compatible if they have required fields                                         |
| Error handling   | Reject at compile                        | Type errors prevent execution                                                           |
| Numeric coercion | Float result                             | `Int + Float = Float` (widening)                                                        |
| Type erasure     | None                                     | Runtime type info preserved for `IS` operator                                           |

## Pipeline Integration

```
Lexer -> CST -> AST -> Name Resolution -> [TYPE CHECK] -> Interpreter
```

---

## Phase 4.0.0: Add Json Type

Before implementing the type checker, add the `Json` type to rumps-query. This is needed for database values (which can store JSON) and completes the value type system.

### JSON Literal Syntax

Distinguish native objects from JSON objects by key quoting:

```rumps
; Native object (unquoted keys)
{ id: 123, name: "John" }

; JSON object (quoted keys)
{ "id": 123, "name": "John" }
```

### JSON Access Operators

| Operator | Description                     | Example        | Result             |
|----------|---------------------------------|----------------|--------------------|
| `.`      | Get field (returns JSON)        | `data.name`    | `"John"` (as JSON) |
| `..`     | Get field (returns text/scalar) | `data..name`   | `John` (as String) |
| `->`     | Get field by key (returns JSON) | `data->"name"` | `"John"` (as JSON) |
| `->>`    | Get field by key (returns text) | `data->>"name"`| `John` (as String) |

### JSON Arrays

JSON arrays are created in two ways:

**1. Heterogeneous elements (implicit JSON)**
```rumps
[1, 'a', 10.01]           ; Json (heterogeneous = must be JSON)
[true, "hello", 42]       ; Json
```

**2. Explicit cast with `as Json`**
```rumps
[1, 2, 3] as Json         ; Json (homogeneous array cast to JSON)
"hello" as Json           ; Json (string literal)
42 as Json                ; Json (int literal)
3.14 as Json              ; Json (float literal)
true as Json              ; Json (bool literal)
```

### Checklist

- [ ] Lexer: Add `ArrowArrow` (`->>`) token
  - Note: `DotDot` (`..`), `Arrow` (`->`) already exist
- [ ] Parser/CST: Detect quoted vs unquoted object keys
- [ ] Parser/CST: Parse JSON access operators
- [ ] AST: Add `Expr::Json` variant
- [ ] AST: Add `Expr::JsonAccess` or extend `Expr::Field` for `..`, `->`, `->>`
- [ ] Interpreter: Evaluate JSON literals to `Value::Json`
- [ ] Interpreter: Implement JSON field access operators
- [ ] Value: Add `Value::Json`
- [ ] Tests: JSON literal parsing and access

---

## Phase 4.0.1: Add Union Types

Add genuine union types to the language. This enables typed database operations where `GET` returns `UNION Storable = Bool | Int | Float | Char | String | Json`.

### Syntax

```rumps
; Declare a union type
UNION Storable = Bool | Int | Float | Char | String | Json

; Use in annotations
LET x: Storable = GET local("key")

; Check with IS
IF x IS String {
  OUTPUT x ++ " is a string"
} ELSE {
  OUTPUT "not a string"
}

; Cast with AS (runtime, may fail)
LET s: String = x AS String
```

### Union Types in Functions

Union types can be used as both parameter types and return types:

```rumps
; Union as parameter type
FUN stringify(x: Storable) -> String {
  IF x IS String { x }
  ELSE { IF x IS Int { x AS String } ELSE { "unknown" } }
}

; Union as return type
FUN parse(s: String) -> Int | String {
  LET n = s READ Int
  IF n IS Result.Ok(n) { 
    n 
  } ELSE {
    s
  }
}

; Inline union in parameter
FUN double(x: Int | Float) -> Float {
  IF x IS Int { (x * 2) AS Float }
  ELSE { x * 2.0 }
}

; Function returning different union members
FUN fetch-value(key: String) -> Storable {
  LET raw = GET local(key)
  raw  ; returns Storable (whatever was stored)
}

; Narrowing a union with IS
FUN process(val: Storable) -> String {
  MATCH val {
    x IS Int    => "integer: " ++ (x AS String),
    x IS Float  => "float: " ++ (x AS String),
    x IS String => "string: " ++ x,
    x IS Bool   => "bool: " ++ (x AS String),
    _           => "other"
  }
}

; Closure with union types
LET handler: (Storable) -> Bool = x => x IS String || x IS Int
```

### Built-in Storable Union

Define a built-in union for database-storable values:

```rumps
UNION Storable = Bool | Int | Float | Char | String | Json
```

This is the return type of `GET` and element type of `COLLECT` (pre-`SELECT`).

### Checklist

- [ ] Lexer: Add `UNION` keyword
- [ ] Parser/CST: Parse `UNION Name = Type | Type | ...` declarations
- [ ] Parser/CST: Parse union types in annotations (`x: Int | String`)
- [ ] AST: Add `Stmt::Union` for declarations
- [ ] AST: Add `AstTypeExpr::Union(Vec<AstTypeExpr>)` for union type expressions
- [ ] TypeRegistry: Register union types
- [ ] Value: No change needed (unions are type-level, not value-level)
- [ ] Interpreter: `IS` checks against union members
- [ ] Interpreter: `AS` casts within union (runtime check)
- [ ] Parser/CST: Parse `x IS Type` patterns in MATCH arms
- [ ] AST: Add `Pattern::Is { binding, ty }` variant for type-narrowing patterns
- [ ] Interpreter: Evaluate `IS` patterns in `MATCH` (runtime type check + binding)
- [ ] Builtins: Define `Storable` union
  - [ ] **NOTE**: Treat `AS` as infallible _only_ for `Storable` to concrete member types
    - I.e. users can _always_ narrow from `Storable` to concrete type; rumtime type error if not successful
    - This is ergonomic choice for making DB access easier
- [ ] Update `GET` return type to `Storable`
- [ ] Tests: Union declaration, IS checks, AS casts

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
struct TyVar(u32);

#[derive(Clone, Debug, PartialEq)]
enum Ty {
    Var(TyVar),
    Bool, 
    Int, 
    Float, 
    Char, 
    String, 
    Unit, 
    Time, 
    Range,
    Array(Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Tuple(Vec<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Object(BTreeMap<String, Ty>),  // anonymous record (structural)
    Named(TypeId, Vec<Ty>),        // user-defined: sum types OR named records (TYPE)
    Unknown,                       // database reads before inference
    Error,                         // error recovery
}

struct Scheme {
    vars: Vec<TyVar>,
    ty: Ty,
}

struct Subst(HashMap<TyVar, Ty>);
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
  - [ ] `TypeError` enum (wrapped by `Error::StaticType` in main error.rs):
    - [ ] `Mismatch { expected: Ty, got: Ty, span: Span }`
    - [ ] `UndefinedVar(String, Span)`
    - [ ] `NotCallable(Ty, Span)`
    - [ ] `ArityMismatch { expected: usize, got: usize, span: Span }`
    - [ ] `NotNumeric(Ty, Span)`
    - [ ] `NotJsonable(Ty, Span)` - for `as Json`, `store`, etc.
    - [ ] `NotSubscript(Ty, Span)` - for SET/GET subscript keys
    - [ ] `NotStorable(Ty, Span)` - for SET value (must be DB-storable)
    - [ ] `MissingField { ty: TypeId, field: String, span: Span }` - struct missing required field
    - [ ] `FieldTypeMismatch { ty: TypeId, field: String, expected: Ty, got: Ty, span: Span }`
    - [ ] `InfiniteType(TyVar, Ty, Span)`
    - [ ] `MissingAnnotation(Span)`
    - [ ] `UnknownType(String, Span)`
  - [ ] `impl Display for TypeError`
  - [ ] Note: integrates with `crate::Error` via `Error::StaticType(TypeError)`

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
struct InferCtx<'a> {
    ast: &'a Ast,
    registry: &'a TypeRegistry,
    env: TypeEnv,
    constraints: Vec<Constraint>,
    next_var: u32,
    expr_types: HashMap<ExprId, Ty>,
    errors: Vec<TypeError>,
}

enum Constraint {
    Eq(Ty, Ty, Span),
    Numeric(Ty, Span),                                         // Int | Float
    Callable { callee: Ty, args: Vec<Ty>, ret: Ty, span: Span },
    Stringable(Ty, Span),                                      // everything (for ++ coercion, OUTPUT)
    Jsonable(Ty, Span),                                        // NOT Closure/Function/ModuleFn/Range
    Subscript(Ty, Span),                                       // Bool | Int | Float | Char | String | Json
    Storable(Ty, Span),                                        // Bool | Int | Float | Char | String | Json
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

- [ ] `impl InferCtx`: `fn infer_expr(&mut self, id: ExprId) -> Ty`
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
| `++`                     | `lhs ~ String`, `Stringable(rhs)`     | `String` (implicit coercion)                     |
| `??`                     | `lhs ~ Option[?t]` or `Result[?t, _]` | `?t` (unify with `rhs`)                          |
| `\|>`                    | `Callable(rhs, [lhs], ?r)`            | `?r`                                             |
| `..`, `..=`              | `lhs ~ Int`, `rhs ~ Int`              | `Range`                                          |

### Unary Operator Rules

| Operator | Constraint         | Result Type     |
|----------|--------------------|-----------------|
| `-`      | `Numeric(operand)` | same as operand |
| `!`      | `operand ~ Bool`   | `Bool`          |

### Checklist

- [ ] `impl InferCtx`: `fn infer_binary(&mut self, lhs: ExprId, op: BinOp, rhs: ExprId, span: Span) -> Ty`
- [ ] Handle arithmetic ops (`Add`, `Sub`, `Mul`, `Mod`, `Pow`):
  - [ ] Add `Numeric` constraints for both operands
  - [ ] Result: fresh var with numeric constraint (or `Float` if either operand is `Float`)
- [ ] Handle `Div` -> always `Float`
- [ ] Handle `FloorDiv` -> always `Int`
- [ ] Handle comparison ops -> `Bool`
- [ ] Handle logical ops (`And`, `Or`) -> unify both with `Bool`, return `Bool`
- [ ] Handle `Concat` -> add `Stringable` constraint for both, return `String`
- [ ] Handle `Coalesce`:
  - [ ] Check lhs is `Option[?t]` or `Result[?t, _]`
  - [ ] Unify rhs with `?t`
  - [ ] Return `?t`
- [ ] Handle `Pipe`:
  - [ ] Add `Callable` constraint
  - [ ] Return fresh var for result
- [ ] Handle `Range`, `RangeInclusive` -> `Range`
- [ ] `impl InferCtx`: `fn infer_unary(&mut self, op: UnaryOp, operand: ExprId, span: Span) -> Ty`
- [ ] Handle `Neg` -> add `Numeric` constraint, return same type
- [ ] Handle `Not` -> unify with `Bool`, return `Bool`

---

## Phase 4.5: Collection Inference

Infer types for arrays, tuples, objects, maps, and ranges.

### Collection Rules

| Expression         | Type                       | Constraints                        |
|--------------------|----------------------------|------------------------------------|
| `[a, b, c]`        | `Array[?t]`                | `a ~ ?t`, `b ~ ?t`, `c ~ ?t`       |
| `[]`               | `Array[?t]`                | (empty, `?t` is fresh)             |
| `(a, b, c)`        | `(?a, ?b, ?c)`             | -                                  |
| `{ x: a, y: b }`   | `Object({ x: ?a, y: ?b })` | -                                  |
| `{ k => v, ... }`  | `Map[?k, ?v]`              | all keys ~ `?k`, all values ~ `?v` |
| `a..b`, `a..=b`    | `Range`                    | `a ~ Int`, `b ~ Int`               |

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
- [ ] **Note**: Range (`..`, `..=`) is handled in Phase 4.4 (operators) but listed here as it's a collection type

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

| Expression            | Type                                |
|-----------------------|-------------------------------------|
| `(x, y) => body`      | `Fn([?x, ?y], ?body)`               |
| `(x: Int) => body`    | `Fn([Int], ?body)`                  |
| `f(a, b)`             | `?r` with `Callable(f, [a, b], ?r)` |

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
| `IF c { a } ELSE { b }`   | `?t`        | `c ~ Bool`, `a ~ ?t`, `b ~ ?t` |
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
    - [ ] Handle `Pattern::Is` (type-narrowing pattern):
      - [ ] Check target type is member of scrutinee's union (if union)
      - [ ] Bind variable with narrowed type in arm scope
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
  - [ ] If target is `Json`, add `Jsonable` constraint on operand
  - [ ] Return target type (runtime coercion)
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

- [ ] `impl InferCtx`: `fn infer_stmt(&mut self, id: StmtId)`
- [ ] Handle `Stmt::Let`:
  - [ ] Infer RHS type
  - [ ] If type annotation present:
    - [ ] Parse annotation to `Ty`
    - [ ] Unify RHS type with annotation type
    - [ ] For `Named` struct annotations: triggers extensible record check
  - [ ] Generalize and bind in env
- [ ] Handle `Stmt::Set`:
  - [ ] Infer subscripts and value
  - [ ] Add `Subscript` constraint for each subscript expression
  - [ ] Add `Storable` constraint for value (or infer via usage)
  - [ ] No env binding
- [ ] Handle `Stmt::Output`:
  - [ ] Infer expression
  - [ ] Add `Stringable` constraint (always satisfied; marks stringify needed)
- [ ] Handle `Stmt::Fun`:
  - [ ] (already covered in Phase 4.7)
- [ ] Handle `Stmt::Type`:
  - [ ] Register type definition in registry
  - [ ] For struct types: store required fields and their types
  - [ ] No inference needed (declaration only)

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
- [ ] `impl InferCtx`: `fn unify_types(&mut self, t1: &Ty, t2: &Ty, span: Span) -> Option<Subst>`
- [ ] Handle `Var` binding (with occurs check)
- [ ] Handle primitive equality
- [ ] Handle numeric coercion (`Int` ~ `Float`)
- [ ] Handle `Array`, `Option`, `Result`, `Map` recursively
- [ ] Handle `Tuple` (element-wise, same length)
- [ ] Handle `Fn` (params + return)
- [ ] Handle `Object` (structural, common fields only)
- [ ] Handle `Named` (same TypeId, unify params)
- [ ] Handle `Named` struct with `Object` (extensible record check):
  - [ ] Look up required fields from TypeRegistry
  - [ ] Check all required fields present in Object
  - [ ] Unify each required field's type
  - [ ] Extra fields in Object are allowed (extensible)
- [ ] Handle `Unknown` (unifies with anything)
- [ ] Handle `Error` (unifies with anything, for recovery)
- [ ] `impl InferCtx`: `fn solve_constraints(&mut self) -> Subst`
  - [ ] Process `Eq` constraints via unification
  - [ ] Process `Numeric` constraints (check resolved type is `Int` or `Float`)
  - [ ] Process `Callable` constraints (unify with `Fn` type)
  - [ ] Process `Stringable` constraints (always satisfied; marks implicit coercion)
  - [ ] Process `Jsonable` constraints (reject `Fn`, `Range`)
  - [ ] Process `Subscript` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [ ] Process `Storable` constraints (check is `Bool | Int | Float | Char | String | Json`)
  - [ ] Compose all substitutions

---

## Phase 4.13: Builtin Function Types

Register type schemes for all primitive/module functions.

### Example Signatures

```
; Array module (also accepts Range where Array[Int] expected)
Array.len:    forall a. (Array[a] | Range) -> Int
Array.map:    forall a b. (Array[a] | Range, (a) -> b) -> Array[b]
Array.filter: forall a. (Array[a] | Range, (a) -> Bool) -> Array[a]
Array.fold:   forall a b. (Array[a] | Range, b, (b, a) -> b) -> b

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
- [ ] `impl InferCtx`: `fn register_builtins(&mut self)`
- [ ] Register `Array` module functions
- [ ] Register `Option` module functions
- [ ] Register `Result` module functions
- [ ] Register `Object` module functions
- [ ] Register `String` module functions
- [ ] Register `Math` module functions
- [ ] Register `Time` module functions
- [ ] Register `Map` module functions
- [ ] Register `Range` module functions (if any)
- [ ] **Note**: Array functions accept `Range` as first arg (Range is iterable over `Int`)

---

## Phase 4.14: Integration and Entry Point

Hook type checking into the interpreter pipeline.

### Entry Point (`typecheck.rs`)

```rust
pub(crate) fn check(ast: &Ast, registry: &TypeRegistry) -> crate::Result<()> {
    let mut ctx = InferCtx::new(ast, registry);
    ctx.register_builtins();

    ast.stmt_ids().for_each(|id| ctx.infer_stmt(id));

    let subst = ctx.solve_constraints();
    ctx.apply_subst(&subst);
    ctx.check_remaining_unknowns();

    ctx.into_result()
}

impl InferCtx<'_> {
    fn into_result(self) -> crate::Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(Error::static_types(self.errors))
        }
    }
}
```

### Checklist

- [ ] Add `pub(crate) fn check(ast, registry) -> crate::Result<()>` to `typecheck.rs`
- [ ] `impl InferCtx`: `fn into_result(self) -> crate::Result<()>`
- [ ] Modify `crates/rumps-query/src/lib.rs`:
  - [ ] Add `mod typecheck;`
- [ ] Modify `crates/rumps-query/src/interpreter.rs`:
  - [ ] Call `crate::typecheck::check(ast, &registry)?` after resolution
- [ ] Modify `crates/rumps-query/src/error.rs`:
  - [ ] Rename `Error::Type` to `Error::RuntimeType` (runtime type mismatch)
  - [ ] Rename `Error::type_err()` to `Error::runtime_type()`
  - [ ] Add `Error::StaticType(TypeError)` variant for compile-time type errors
  - [ ] Add `Error::static_types(Vec<TypeError>) -> Self` constructor:
    - [ ] If single error, return `Error::StaticType(err)`
    - [ ] If multiple, wrap in `Error::Multiple`
  - [ ] Update `Diagnostic` impl: add `"rumps::static_type"` code for new variant
  - [ ] Update `ErrorDisplay` impl for new variant
  - [ ] Update all call sites of `Error::type_err()` to `Error::runtime_type()`

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

---

## Phase 4.17: Post-Type-Checker Cleanup

Once the type checker is working and we're confident in its soundness, eliminate redundant runtime type checks throughout the interpreter. This is a significant cleanup affecting ~180+ check sites.

### Summary of Eliminable Checks

| Category                    | Count | Primary Files               |
|-----------------------------|-------|-----------------------------|
| Arity checks                | 50+   | `primitives.rs`             |
| Array homogeneity           | 4     | `collections.rs`, `call.rs` |
| Binary operator type checks | 11+   | `ops.rs`                    |
| Type coercion helpers       | 5+    | `types.rs`, `primitives.rs` |
| IF/Unit checks              | 2     | `control.rs`                |
| Unwrap/Option/Result        | 5+    | `control.rs`                |
| Module function type checks | 100+  | `primitives.rs`             |

---

### 4.17.1: Remove Arity Checks (`primitives.rs`)

**Current pattern:**
```rust
Self::check_arity("Object.keys", &args, 1, ctx.span)?;
```

**After cleanup:**
```rust
// Arity validated by Callable constraint at compile-time; no check needed
```

**Checklist:**
- [ ] Remove `check_arity` function entirely
- [ ] Remove all `check_arity` calls (~50+ sites across all module functions)
- [ ] Object module: lines ~162, 191, 211, 229, 249, 266, 288
- [ ] Array module: lines ~414, 435, 456, 477, 500, 521, 544, 567, 590, 668, 714, 764, 793
- [ ] String module: lines ~832, 856, 881, 906, 936, 986, 1031, 1085, 1120
- [ ] Math module: lines ~1171, 1200, 1218, 1236, 1254, 1272, 1290, 1308, 1326, 1344, 1362
- [ ] Map module: lines ~1640, 1659, 1687, 1708, 1745, 1777, 1813, 1896, 1926, 1959, 2001, 2050
- [ ] Time module: lines ~2302, 2318, 2334, 2350, 2366
- [ ] Random module: lines ~1518, 1543, 1564, 1585
- [ ] Option/Result module: lines ~2368, 2420

---

### 4.17.2: Remove Array Homogeneity Checks (`collections.rs`)

**Current pattern (`array_elems`, lines 69-101):**
```rust
if self.type_exprs.eq(elem_ty, val_ty) {
    // ok
} else {
    Err(Error::type_err(span, "array elements must have the same type"))
}
```

**After cleanup:**
```rust
// Type checker ensures Array[T] elements are all T; no runtime check
```

**Checklist:**
- [ ] `array_elems()`: Remove type equality check (lines 83-97)
- [ ] `map_lit_entries()`: Remove key type homogeneity check (lines 205-215)
- [ ] `map_lit_entries()`: Remove value type homogeneity check (lines 219-228)
- [ ] Remove `TypeExprArena` tracking from `Value::Array` if no longer needed

---

### 4.17.3: Remove Array.map/filter Homogeneity Checks (`call.rs`)

**Current pattern (`array_map_rec`, lines 442-488):**
```rust
if fty == result_ty {
    // ok
} else {
    Err(Error::type_err(span,
        "Array.map: function produces heterogeneous results"))
}
```

**After cleanup:**
```rust
// Type checker infers Array.map : (Array[A], (A) -> B) -> Array[B]
// Result homogeneity is guaranteed statically
```

**Checklist:**
- [ ] `array_map_rec()`: Remove result type homogeneity check (lines 467-481)
- [ ] `range_map_rec()`: Remove result type homogeneity check (lines 411-424)
- [ ] `array_filter_rec()`: Remove similar checks if present
- [ ] `array_reduce()`: Verify no type checks needed

---

### 4.17.4: Remove Binary Operator Type Checks (`ops.rs`)

**Current pattern:**
```rust
fn binop_add(&mut self, lhs: ValueId, rhs: ValueId, span: Span) -> Result<Value> {
    match (self.arena.get(lhs), self.arena.get(rhs)) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        // ... more cases ...
        _ => Err(Error::type_err(span, "cannot add these types"))
    }
}
```

**After cleanup:**
```rust
fn binop_add(&mut self, lhs: ValueId, rhs: ValueId) -> Value {
    match (self.arena.get(lhs), self.arena.get(rhs)) {
        (Value::Int(a), Value::Int(b)) => Value::Int(a + b),
        (Value::Float(a), Value::Float(b)) => Value::Float(a + b),
        (Value::Int(a), Value::Float(b)) => Value::Float(*a as f64 + b.0),
        (Value::Float(a), Value::Int(b)) => Value::Float(a.0 + *b as f64),
        _ => unreachable!("type checker guarantees numeric operands")
    }
}
```

**Checklist:**
- [ ] `apply_unop()`: Remove error branches for `-` and `!` (lines 68-84)
- [ ] `binop_add()`: Remove error branch (lines 126-133)
- [ ] `binop_sub()`: Remove error branch
- [ ] `binop_mul()`: Remove error branch
- [ ] `binop_div()`: Remove error branch
- [ ] `binop_floor_div()`: Remove error branch
- [ ] `binop_mod()`: Remove error branch
- [ ] `binop_pow()`: Remove error branch
- [ ] `binop_cmp()`: Remove error branch for comparison operators
- [ ] Change return types from `Result<Value>` to `Value` where possible

---

### 4.17.5: Remove IF/Unit Checks (`control.rs`)

**Current pattern (`check_unit`, lines 317-335):**
```rust
fn check_unit(&self, val: ValueId, span: Span) -> Result<()> {
    let ty = self.type_of(val);
    if self.type_exprs.eq(ty, unit_ty) {
        Ok(())
    } else {
        Err(Error::type_err(span, "single-arm IF body must be Unit"))
    }
}
```

**After cleanup:**
```rust
// Type checker enforces: IF without ELSE must have body : Unit
// No runtime check needed
```

**Checklist:**
- [ ] Remove `check_unit()` function entirely
- [ ] `r#if()`: Remove `check_unit` call (lines 216-220)
- [ ] `if_with_bindings()`: Remove `check_unit` call (line 269)

---

### 4.17.6: Remove Unwrap/Coalesce Type Checks (`control.rs`)

**Current pattern (`unwrap`, lines 13-70):**
```rust
let is_option = |ty| base_type(ty) == Some(TypeId::OPTION);
let is_result = |ty| base_type(ty) == Some(TypeId::RESULT);

match val {
    Value::Tagged(ty, idx, _) if is_option(ty) => { ... }
    Value::Tagged(ty, idx, _) if is_result(ty) => { ... }
    _ => Err(Error::type_err(span, "can only unwrap Option or Result"))
}
```

**After cleanup:**
```rust
// Type checker ensures unwrap operand is Option[T] or Result[T, E]
let Value::Tagged(_, idx, payloads) = val else {
    unreachable!("type checker guarantees Option or Result")
};
```

**Checklist:**
- [ ] `unwrap()`: Remove `is_option`/`is_result` helper closures
- [ ] `unwrap()`: Remove type error branch (lines 62-68)
- [ ] `coalesce()`: Remove similar type checking logic (lines 81-170)
- [ ] Simplify to direct pattern matching on `Value::Tagged`

---

### 4.17.7: Remove Range Type Checks (`control.rs`)

**Current pattern (`range`, lines 402-439):**
```rust
let start = match self.arena.get(start_id) {
    Value::Int(n) => *n,
    _ => Err(Error::type_err(span, "range start must be Int"))?
};
```

**After cleanup:**
```rust
let Value::Int(start) = self.arena.get(start_id) else {
    unreachable!("type checker guarantees Int")
};
```

**Checklist:**
- [ ] `range()`: Remove start type check (lines 412-420)
- [ ] `range()`: Remove end type check (lines 423-431)

---

### 4.17.8: Remove Type Coercion Error Branches (`primitives.rs`, `types.rs`)

**Current pattern (`to_float`, lines 113-128):**
```rust
fn to_float(v: &Value) -> Result<f64> {
    match v {
        Value::Int(n) => Ok(*n as f64),
        Value::Float(f) => Ok(f.0),
        _ => Err(Error::type_err(span, "expected numeric"))
    }
}
```

**After cleanup:**
```rust
fn to_float(v: &Value) -> f64 {
    match v {
        Value::Int(n) => *n as f64,
        Value::Float(f) => f.0,
        _ => unreachable!("Numeric constraint guarantees Int or Float")
    }
}
```

**Checklist:**
- [ ] `to_float()`: Remove error branch, change return to `f64`
- [ ] `to_int()`: Remove error branch, change return to `i64`
- [ ] Update all call sites of `to_float`/`to_int` to remove `?`

---

### 4.17.9: Remove Module Function Type Checks (`primitives.rs`)

**Current pattern (100+ occurrences):**
```rust
let arr = ctx.arena.get_array(args[0])
    .ok_or_else(|| ctx.type_error("Array.length", "Array"))?;
```

**After cleanup:**
```rust
let Value::Array(_, elems) = ctx.arena.get(args[0]).unwrap() else {
    unreachable!("type checker guarantees Array")
};
```

**Checklist by module:**

**Object module:**
- [ ] `Object.keys`: Remove Object type check (line 167)
- [ ] `Object.values`: Remove Object type check (line 194)
- [ ] `Object.entries`: Remove Object type check (line 214)
- [ ] `Object.has`: Remove Object type check (line 232)
- [ ] `Object.lookup`: Remove Object type check
- [ ] `Object.insert`: Remove Object type check
- [ ] `Object.remove`: Remove Object type check

**Array module:**
- [ ] `Array.length`: Remove Array type check (line 419)
- [ ] `Array.head`: Remove Array type check (line 438)
- [ ] `Array.tail`: Remove Array type check (line 459)
- [ ] `Array.last`: Remove Array type check (line 480)
- [ ] `Array.init`: Remove Array type check (line 503)
- [ ] `Array.nth`: Remove Array type check (line 524)
- [ ] `Array.reverse`: Remove Array type check
- [ ] `Array.concat`: Remove Array type checks
- [ ] `Array.contains`: Remove Array type check
- [ ] `Array.map`: Remove Array/closure type checks (line 671)
- [ ] `Array.filter`: Remove Array/closure type checks (line 717)
- [ ] `Array.reduce`: Remove Array/closure type checks (line 767)
- [ ] `Array.find`: Remove type checks
- [ ] `Array.any`: Remove type checks
- [ ] `Array.all`: Remove type checks
- [ ] `Array.sort`: Remove type checks
- [ ] `Array.sort-by`: Remove type checks

**String module:**
- [ ] `String.length`: Remove String type check (line 835)
- [ ] `String.chars`: Remove String type check (line 859)
- [ ] `String.split`: Remove String type checks (line 939-942)
- [ ] `String.join`: Remove Array/String type checks (line 989, 993)
- [ ] `String.trim`: Remove String type check
- [ ] `String.starts-with`: Remove String type checks
- [ ] `String.ends-with`: Remove String type checks
- [ ] `String.contains`: Remove String type checks
- [ ] `String.replace`: Remove String type checks
- [ ] `String.to-upper`: Remove String type check
- [ ] `String.to-lower`: Remove String type check
- [ ] `String.pad-left`: Remove type checks
- [ ] `String.pad-right`: Remove type checks

**Math module:**
- [ ] All Math functions: Remove Float type checks
- [ ] `Math.abs`, `Math.floor`, `Math.ceil`, `Math.round`, etc.

**Map module:**
- [ ] `Map.length`: Remove Map type check (line 1643)
- [ ] `Map.keys`: Remove Map type check (line 1662)
- [ ] `Map.values`: Remove Map type check (line 1690)
- [ ] `Map.entries`: Remove Map type check (line 1711)
- [ ] `Map.has`: Remove Map type check (line 1748)
- [ ] `Map.lookup`: Remove Map type check (line 1780-1789)
- [ ] `Map.insert`: Remove Map type check (line 1816)
- [ ] `Map.remove`: Remove Map type check (line 1854)
- [ ] `Map.merge`: Remove Map type checks (line 1899, 1908)
- [ ] `Map.from-entries`: Remove Array type check (line 1948)

**Time module:**
- [ ] `Time.now`: No type checks needed
- [ ] `Time.parse`: Remove String type check
- [ ] `Time.format`: Remove Time/String type checks
- [ ] `Time.add-*`: Remove Time/Int type checks
- [ ] `Time.diff-*`: Remove Time type checks

**Random module:**
- [ ] `Random.int`: Remove Int type checks (line 1521)
- [ ] `Random.float`: Remove Float type checks (line 1546)
- [ ] `Random.choice`: Remove Array type check (line 1567)
- [ ] `Random.shuffle`: Remove Array type check

**Option/Result module:**
- [ ] `Option.unwrap-or`: Remove Option type check (line 2371)
- [ ] `Result.unwrap-or`: Remove Result type check (line 2423)
- [ ] `Option.map`: Remove type checks
- [ ] `Result.map`: Remove type checks
- [ ] `Result.map-err`: Remove type checks

---

### 4.17.10: Simplify Pattern Matching (`pattern.rs`)

**Checklist:**
- [ ] `check_variant_zero_arity()`: Remove runtime arity validation (lines 66-74)
- [ ] `check_variant()`: Simplify type/variant matching (lines 98-103)
- [ ] Pattern exhaustiveness is checked statically; remove runtime fallbacks

---

### 4.17.11: Simplify Type Coercion (`types.rs`)

**Checklist:**
- [ ] `coerce()`: Remove unsupported cast error branch (lines 160-169)
  - Keep: `AS` on `Storable` union remains fallible at runtime
- [ ] `try_convert()`: Remove unsupported conversion error branch (lines 228-238)
  - Keep: `READ` remains fallible at runtime
- [ ] `value_matches_type()`: May be removable if only used for runtime `IS` checks

---

### Notes

**Keep runtime checks for:**
- `AS` casts on `Storable` union (user may cast `Storable` to wrong concrete type)
- `READ` conversions (parsing can fail)
- Database operations returning `Storable` (need `IS`/`AS` for narrowing)

---

### 4.17.12: Test-Driven Removal Strategy

For each category of runtime checks, follow this process:

**Step 1: Write a failing test**
```rumps
; scripts/err_array_heterogeneous.rumps
; This should be caught by the type checker
LET arr: Array[Int] = [1, "two", 3]  ; ERROR: array elements must have same type
```

**Step 2: Create expected snapshot**
Via `cargo insta`; `miette` will produce a nice error, this is just for example

```
; snapshots/scripts__err_array_heterogeneous.snap
error: type mismatch
  --> err_array_heterogeneous.rumps:2:15
   |
 2 | LET arr: Array[Int] = [1, "two", 3]
   |                           ^^^^^ expected Int, got String
```

**Step 3: Run test**
- If type-checker catches it (test passes with static error): go immediately to Step 4 below
- If type-checker misses it (runtime error or no error): fix type-checker, repeat; then go to Step 4 once fixed

**Step 4: Remove runtime check**
Once the type-checker reliably catches the error, remove the corresponding runtime check code.

---

**Checklist for each check category:**

**Arity checks:**
- [ ] Write test: `Array.map(arr)` (missing second arg)
- [ ] Verify type-checker catches `ArityMismatch`
- [ ] Remove `check_arity` calls

**Array homogeneity:**
- [ ] Write test: `[1, "two", 3]`
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove `array_elems` type equality check

**Binary operators:**
- [ ] Write test: `"hello" + 5`
- [ ] Verify type-checker catches `NotNumeric` or `Mismatch`
- [ ] Remove operator error branches

**IF/Unit:**
- [ ] Write test: `LET x = IF TRUE { 42 }` (no ELSE, body not Unit)
- [ ] Verify type-checker catches the constraint
- [ ] Remove `check_unit` function

**Unwrap:**
- [ ] Write test: `42!` (unwrap on non-Option/Result)
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove unwrap type checks

**Range:**
- [ ] Write test: `"a".."z"` (non-Int range bounds)
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove range type checks

**Module functions:**
- [ ] Write test for each module: e.g., `Array.length("not an array")`
- [ ] Verify type-checker catches `Mismatch`
- [ ] Remove `.ok_or_else(|| ctx.type_error(...))` patterns

---

**Expected benefits:**
- Smaller binary size (less error handling code)
- Faster execution (no redundant checks)
- Cleaner code (pattern matches without error branches)
- Confidence: each removed check has a test proving the type-checker catches it

---

## Future Considerations: Database Primitives

The following primitives are not yet implemented but will require special type handling when added:

### `DATA`

Returns information about whether a node exists and has data/descendants. Returns a `DataStatus` type (not `Int`):

```rumps
DATA ^global(subscripts...)  ; -> DataStatus
DATA local(subscripts...)    ; -> DataStatus
```

The `DataStatus` type corresponds to `rumps_types::DataStatus`:

```rust
enum DataStatus {
    NoData = 0,        ; no value, no descendants
    HasValue = 1,      ; has value only
    HasDescendants = 10, ; has descendants only
    Both = 11,         ; has both value and descendants
}
```

**Casting to Int**: `DATA ... AS Int` always succeeds and evaluates to the `u8` representation:

```rumps
LET status = DATA ^PATIENT(123)
IF status IS DataStatus.HasValue { ... }

; Or get the numeric value
LET code: Int = DATA ^PATIENT(123) AS Int  ; 0, 1, 10, or 11
```

### `ORDER`

Returns the next/previous subscript key in sorted order. Type depends on subscript type:

```
ORDER(^global(subscripts...), direction) -> Option[Subscript]
```

Where `Subscript` is a union of valid subscript types (`Bool | Int | Float | Char | String | Json`). May need a dedicated `Subscript` type or return `Unknown` and require annotation.

### `COLLECT` (most complex)

This is the declarative iteration primitive:

```
COLLECT ^global(prefix...)
  WHERE predicate
  SELECT transform
  TAKE n
  -> ???
```

Key challenges:
- **Element type inference**: What type does each iteration yield?
- **Predicate typing**: The `WHERE` clause receives bound variables; what are their types?
- **Transform typing**: The `SELECT` clause transforms elements; output type depends on transform
- **Composability**: `COLLECT` is lazy/streaming; type must reflect this (e.g., `Stream[T]` or `Iterator[T]`)

### Approach: Union Types for Database Values

Database types are restricted (see `rumps_types`). We model this with a built-in union:

```rumps
UNION Storable = Bool | Int | Float | Char | String | Json
```

**`Subscript`** (key components) is similar but uses `Number` (f64) internally instead of separate `Int`/`Float`:
```rumps
UNION Subscript = Bool | Number | Char | String | Json
```

For typing:
- `GET local(k)` -> returns `Storable`
- `GET local(k)` used as `x + 1` -> narrow to `Int | Float`
- `GET local(k)` used as `x ++ y` -> narrow to `String`
- `GET local(k)` with no constraining usage -> remains `Storable`
- `ORDER` result -> `Option[Subscript]`
- `COLLECT` element values -> `Storable` (narrowed by `SELECT` usage)

Users can:
1. Let inference narrow the union from usage
2. Use `IS` to check the runtime type
3. Use `AS` to cast (runtime, may fail)

This is more precise than `Any` since we know exactly what the DB can store, and users have first-class tools (`IS`, `AS`) to work with unions.
