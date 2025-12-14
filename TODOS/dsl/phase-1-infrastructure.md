# Phase 1: Query Language Infrastructure

This document tracks the first phase of implementing the RUMPS query language: setting up the foundational infrastructure for lexing, parsing, and interpreting.

**NOTE**: Maximum visibility in general should be `pub(crate)`. Only very few things need to be `pub`. Use `pub(crate)` or keep private.

## Goals

Build a minimal working vertical slice that can:
1. Lex source code into tokens
2. Parse tokens into an AST
3. Interpret the AST to produce results

The initial subset should support:
- Literals (integers, floats, strings, booleans)
- Variables (locals only initially; globals require DB integration)
- Basic expressions (arithmetic, comparison, logical ops)
- `SET` statements
- `OUTPUT` statements
- Comments (`;` to end of line; this is the ONLY use of semicolon)

## Crate Structure

```
crates/rumps-query/
  src/
    lib.rs           # Public API
    error.rs         # Error types with spans
    span.rs          # Source location tracking
    token.rs         # Token definitions
    lexer.rs         # Lexer implementation
    ast.rs           # AST node definitions
    parser.rs        # Parser implementation
    value.rs         # Runtime values
    env.rs           # Variable environments
    interpreter.rs   # AST walker/evaluator
```

## Phase 1 Tasks

### 1. Crate Setup
- [x] Create `Cargo.toml` with dependencies:
  - `chumsky` (parsing)
  - `smallvec` (inline storage for small collections; type params, variants, payloads)
    - **NOTE**: `rumps-storage` also uses this; move to workspace dep with correct features enabled, then update `rumps-storage/Cargo.toml`
  - `rumps-types` (shared types)
  - `rumps-storage` (database access)
  - `tokio` (async runtime; interpreter is async; **NOTE**: workspace dep)
- [x] Set up module structure in `lib.rs`
- [x] Add crate to workspace in root `Cargo.toml`

### 2. Error and Span Types
- [x] Define `Span` type for source locations:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct Span {
      pub start: u32,  // byte offset
      pub end: u32,    // byte offset (exclusive)
  }
  ```
  Using `u32` limits source files to ~4GB, which is plenty. Chumsky's `SimpleSpan` can convert to/from this.
- [x] Define `Error` enum with variants for lex/parse/runtime errors:
  ```rust
  pub enum Error {
      Lex { span: Span, msg: String },
      Parse { span: Span, msg: String, expected: Vec<String> },
      Runtime { span: Option<Span>, msg: String },  // span from AST/Value if available
  }
  ```
- [x] Implement `Display` for errors with span info (line/column computed from source on demand)

### 3. Token Definition

**NOTE**: This is a minimal subset for Phase 1. See `TODOS/dsl.md` for the full language specification including all keywords, operators, and constructs to be implemented in later phases.

- [x] Define `Token` enum with:
  - Keywords (Phase 1 subset): `LET`, `SET`, `KILL`, `OUTPUT`, `IF`, `ELSE`, `AND`, `OR`, `NOT`, `TRUE`, `FALSE`
    - Future keywords (see `dsl.md`): `COLLECT`, `WHERE`, `SELECT`, `FILTER`, `MAP`, `TAKE`, `SKIP`, `INTO`, `TRANSACTION`, `FUN`, `MATCH`, `TYPE`, `IMPORT`, `NAMESPACE`, `CATCH`, `HANDLE`, `TRY`, `FINALLY`, `THROW`, `FOREACH`, `PARALLEL`, `GROUP`, `BY`, `SORT`, `JOIN`, `AGGREGATE`, `COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `REDUCE`, `REVERSE`, `WHILE`, `SAVEPOINT`, `ROLLBACK`, `WITH`, `ISOLATION`, `TIMEOUT`, `PRIORITY`, `ON`, `CONFLICT`, `RETRY`, `ABORT`, `SKIP`, `OVERWRITE`, `DO`, `AS`, `TO`, `FILE`, `ERROR`, `HEADERS`, `SEPARATOR`, (`ROOT`, `ELEMENT` if we ever do XML output?)
  - Literals (Phase 1 subset): `Int(i64)`, `Float(f64)`, `String(String)` (no `Bool` token; booleans come from `TRUE`/`FALSE` keywords, converted to `Literal(Value::Bool(...))` by the parser)
    - Future literals (see `dsl.md`): record literals (`{ id: 123, name: "John" }`; unquoted keys), JSON object literals (`{ "id": 123 }`; quoted keys), array literals (`[1, 2, 3]`), regex literals (`/pattern/`), template strings with interpolation (`"Hello {name}"`), range literals (`1..10`)
  - Identifiers: `Ident(String)`, `Global(String)` (prefixed with `^`)
  - Operators (Phase 1 subset): arithmetic (`+`, `-`, `*`, `/`, `//`, `%`), comparison (`==`, `!=`, `<`, `>`, `<=`, `>=`), logical (`&&`, `||`, `!`), assignment (`=`), concat (`++`)
    - Future operators (see `dsl.md`): `??`, `?.`, `|>`, `..`, `...`, `is`, `as`, `^` or `**` (power), `matches`, `~`, `contains`, `->`, `->>`, `#>`, `#>>`, `@>`, `<@`, `?`, `?|`, `?&`, `#-`, `@`
  - Punctuation: `(`, `)`, `{`, `}`, `[`, `]`, `,`, `:`, `.`, `..`
  - Comments: `;` starts a comment to end of line (not a token; stripped by lexer)
  - Special: `Newline`, `Indent`, `Dedent`, `Eof`

### 4. Lexer Implementation
- [x] Implement whitespace handling (preserve for train-case detection)
- [x] Implement indentation tracking (emit `Indent`/`Dedent` tokens for multi-line expressions)
- [x] Implement comment skipping (`;` to newline)
- [x] Implement string literal parsing (with escape sequences)
- [x] Implement number parsing (int and float)
- [x] Implement identifier parsing (including train-case)
- [x] Handle train-case vs subtraction:
  - `my-var` (no spaces) = identifier
  - `a - b` (spaces) = subtraction
- [x] Implement keyword recognition:
  - Case-insensitive (`SET` = `set` = `Set`)
  - No abbreviations; full keyword names only (no `S` for `SET`, etc.)
- [x] Implement operator tokenization (handle multi-char: `==`, `!=`, `>=`, `<=`, `++`, `&&`, `||`)

### 5. AST Definition

**NOTE**: This is a minimal subset for Phase 1. See `TODOS/dsl.md` for full expression and statement types including `COLLECT` streams, `MATCH` expressions, `FUN` definitions, `TRANSACTION` blocks, sum type definitions, etc.

Uses **arena allocation** with indices instead of `Box<Expr>` for cache-friendliness and to avoid deep pointer chains. See Design Decisions for rationale.

- [ ] Define arena types:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct ExprId(u32);

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct StmtId(u32);

  pub struct Ast {
      exprs: Vec<Expr>,
      expr_spans: Vec<Span>,  // parallel; indexed by ExprId
      stmts: Vec<Stmt>,
      stmt_spans: Vec<Span>,  // parallel; indexed by StmtId
  }
  ```
  Spans are stored in parallel vectors rather than inline (e.g., `Vec<(Expr, Span)>`) for cache efficiency; the interpreter rarely needs spans during normal execution, only for error reporting.
- [ ] Define `Expr` enum (references use `ExprId`):
  - `Literal(Value)`
  - `Local(String)` (local variable)
  - `Global(String, SmallVec<[ExprId; 4]>)` (global with subscripts; implicit GET — `^PATIENT(123)` alone is a valid expression that evaluates to its value, no explicit `GET(...)` wrapper required)
  - `Binary(ExprId, BinOp, ExprId)`
  - `Unary(UnOp, ExprId)`
  - `Call(String, SmallVec<[ExprId; 4]>)` (function call; most have 0-4 args)
  - `Object(Vec<(String, ExprId)>)` (record literal)
  - `Array(Vec<ExprId>)` (array literal)
  - `Index(ExprId, ExprId)` (array/object access)
  - `Field(ExprId, String)` (`.field` access)
- [ ] Define `Stmt` enum (references use `ExprId`/`StmtId`):
  - `Let(String, ExprId)` (lexical binding; sync, not subscriptable)
  - `Set(String, ExprId)` (local B-tree assignment)
  - `SetGlobal(String, SmallVec<[ExprId; 4]>, ExprId)` (global assignment)
  - `Kill(String, SmallVec<[ExprId; 4]>)` (delete)
  - `Output(ExprId)` (print)
  - `If(ExprId, Vec<StmtId>, Option<Vec<StmtId>>)`
  - `Block(Vec<StmtId>)`
  - `Expr(ExprId)` (expression statement)
- [ ] Define `BinOp` and `UnOp` enums
- [ ] Implement `Ast` methods: `add_expr(&mut self, e: Expr) -> ExprId`, `add_stmt(&mut self, s: Stmt) -> StmtId`, `get_expr(&self, id: ExprId) -> &Expr`, `get_stmt(&self, id: StmtId) -> &Stmt`

### 6. Parser Implementation
- [ ] Set up chumsky parser structure (two-phase: lexer output -> AST)
- [ ] Implement expression parser with precedence:
  1. Primary: literals, identifiers, parenthesized, arrays, objects
  2. Postfix: field access (`.`), index (`[...]`), call (`(...)`)
  3. Unary: `NOT`, `!`, `-`
  4. Multiplicative: `*`, `/`, `//`, `%`
  5. Additive: `+`, `-`, `++`
  6. Comparison: `<`, `>`, `<=`, `>=`, `==`, `!=`
  7. Logical AND: `AND`, `&&`
  8. Logical OR: `OR`, `||`
  9. Null coalesce: `??`
- [ ] Implement statement parser
- [ ] Implement block parser (`{` statements `}`)
- [ ] Add error recovery and helpful messages

### 7. Value Types and Type Registry

Uses **arena allocation** (like the AST) for cache efficiency and to avoid `Box` in recursive structures. Strings are **interned** to avoid duplication and enable O(1) comparison. A **type registry** enables runtime type validation, `is` checks, and clear error messages.

- [ ] Define arena, interning, and type registry:
  ```rust
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct ValueId(u32);

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub struct StringId(u32);

  #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
  pub struct TypeId(u32);

  pub struct ValueArena {
      values: Vec<Value>,
      value_spans: Vec<Span>,   // parallel; for error messages
      strings: Vec<String>,     // intern table
      string_map: HashMap<String, StringId>,
  }

  pub struct TypeRegistry {
      defs: Vec<TypeDef>,
      by_name: HashMap<StringId, TypeId>,
  }

  pub enum TypeDef {
      Builtin(BuiltinType),
      Sum {
          name: StringId,
          type_params: SmallVec<[StringId; 2]>,  // e.g., ["T", "E"] for Result[T, E]
          variants: SmallVec<[VariantDef; 4]>,
      },
  }

  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum BuiltinType { Bool, Int, Float, String, Array, Object }
  // Note: Option and Result are built-in sum types, not BuiltinType variants

  pub struct VariantDef {
      name: StringId,
      idx: u8,
      arity: u8,  // payload count
  }

  // For type annotations (not stored in values; used for validation)
  pub enum TypeExpr {
      Named(TypeId),
      App(TypeId, SmallVec<[TypeExpr; 2]>),  // e.g., Result[Int, String]
  }
  ```
- [ ] Define `Value` enum (references use `ValueId`/`TypeId`):
  - `Bool(bool)`
  - `Int(i64)`
  - `Float(f64)`
  - `String(StringId)` (interned)
  - `Array(Vec<ValueId>)`
  - `Object(HashMap<StringId, ValueId>)`
  - `Tagged(TypeId, u8, SmallVec<[ValueId; 2]>)` (sum type, variant index, payloads)
  - Note: No `None` variant; use `Option.None` via `Tagged(OPTION_TYPE_ID, 0, [])`
- [ ] Implement `TypeRegistry` methods:
  - `get_def(TypeId) -> &TypeDef`
  - `type_name(TypeId) -> StringId` (for error messages)
  - `variant_name(TypeId, u8) -> StringId` (for error messages)
  - `lookup(StringId) -> Option<TypeId>`
- [ ] Register built-in types at startup:
  - Primitives via `BuiltinType`: Bool, Int, Float, String, Array, Object
  - Sum types: `Option` (variants: `None`, `Some`), `Result` (variants: `Ok`, `Err`)
- [ ] Implement `Display` for values (uses registry for Tagged variant names)
- [ ] Implement type coercion rules (conservative: into strings, between numerics)

### 8. Environment

The `Environment` tracks lexical scope for `LET` bindings and callable names. `SET` variables (both local and global) go through the `Database`.

- [ ] Define `Scopes` as a stack for lexical `LET` bindings:
  ```rust
  pub struct Scopes {
      stack: Vec<HashMap<StringId, ValueId>>,  // Stack of frames; top = current
  }
  ```
  Uses `StringId`/`ValueId` from the arena for efficiency. Stack-based avoids `Box` allocations.
- [ ] Define `Environment` struct with:
  - `scopes: Scopes` (lexical scope stack for `LET` bindings)
  - `primitives: HashMap<String, PrimitiveFn>` (built-in functions)
  - Future: `functions: HashMap<String, FunDef>` (user-defined functions)
  - Future: `namespaces: HashMap<String, Namespace>` (e.g., `String`, `Math`)
  - Future: `types: HashMap<String, TypeDef>` (sum types, type aliases)
- [ ] Implement `Scopes` methods:
  - `bind(&mut self, name: StringId, val: ValueId)` (add `LET` binding to current frame)
  - `lookup(&self, name: StringId) -> Option<ValueId>` (search stack top-down)
  - `push(&mut self)` (enter nested scope)
  - `pop(&mut self)` (exit scope)
- [ ] Define `PrimitiveFn` type (async fn pointer or enum)
- [ ] Register built-in primitives (Phase 1 subset: maybe just `GET` as a function?)
- [ ] Implement name lookup with scope chain (for nested scopes later)

### 9. Interpreter

**IMPORTANT**: The interpreter must be **async** because `Database`/`Transaction` methods are async. All evaluation and execution methods return futures.

- [ ] Define `Interpreter` struct with:
  - `ast: &Ast` (the parsed AST)
  - `env: &Environment` (primitives, functions, namespaces, types)
  - `db: Database` (owned; for all variable operations)
  - `txn: Option<Transaction>` (active transaction, if any)

**Why owned `Database`?**
- The interpreter is the natural owner when running `rumps path/to/db script.rumps`
- `PRAGMA` directives require `Database::reconfigure` (mutable access)
- `Database` is cheap to clone (internal `Arc`), so ownership has very low perf cost
- [ ] Implement async evaluation:
  ```rust
  async fn eval_expr(&mut self, id: ExprId) -> Result<Value>
  async fn exec_stmt(&mut self, id: StmtId) -> Result<()>
  async fn exec(&mut self, stmts: &[StmtId]) -> Result<()>
  ```
- [ ] Implement binary operations with type checking
- [ ] Implement `OUTPUT` (print to stdout)
- [ ] Implement `LET` (sync; `env.scopes.bind(...)`)
- [ ] Implement `SET` for locals (async; `db.set_local(...)`)
- [ ] Implement `SET` for globals (async; requires `txn.set(...)`)
- [ ] Implement `GET` for locals (async; `db.get_local(...)`)
- [ ] Implement `GET` for globals (async; `db.get(...)` or `txn.get(...)`)

### 10. Integration Tests
- [ ] Test: lex simple expressions
- [ ] Test: parse and evaluate arithmetic
- [ ] Test: SET and variable lookup
- [ ] Test: OUTPUT with string interpolation (if implemented)
- [ ] Test: IF/ELSE branches
- [ ] Test: comparison and logical operators

## Design Decisions

### Arena Allocation for AST

Instead of using `Box<Expr>` and `Box<Stmt>` for recursive AST nodes, we use **arena allocation with indices**:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StmtId(u32);

pub struct Ast {
    exprs: Vec<Expr>,
    expr_spans: Vec<Span>,
    stmts: Vec<Stmt>,
    stmt_spans: Vec<Span>,
}
```

**Why?**
- **Cache-friendly**: All nodes contiguous in memory; no pointer chasing
- **Smaller**: `ExprId`/`StmtId` are 4 bytes vs 8-byte pointers
- **No recursive drop**: Avoids stack overflow on deeply nested ASTs
- **Easy traversal**: Can iterate all nodes directly via the arena
- **Simpler cloning**: IDs are `Copy`; no need for `Rc` or deep clones

**Spans**: Stored in parallel vectors rather than inline. The interpreter rarely needs spans during execution; they're only accessed for error reporting.

**Usage**: Parser builds AST by calling `ast.add_expr(...)` / `ast.add_stmt(...)`, which return IDs. Interpreter looks up nodes via `ast.get_expr(id)` / `ast.get_stmt(id)`.

### Arena Allocation for Values

Runtime values also use arena allocation for the same reasons as AST nodes. Additionally, strings are **interned** (deduplicated) for O(1) comparison and memory efficiency.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueId(u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StringId(u32);

pub struct ValueArena {
    values: Vec<Value>,
    value_spans: Vec<Span>,
    strings: Vec<String>,
    string_map: HashMap<String, StringId>,
}
```

**Why intern strings?**
- Many values share identical keys (e.g., `"name"`, `"id"` in objects)
- Comparison becomes integer comparison (O(1) vs O(n))

**Value spans**: Enable error messages like "type mismatch: expected Int, got String (value created at line 5)".

### Type Registry for Runtime Validation

A `TypeRegistry` maps `TypeId` to type definitions, enabling runtime type validation (`is` operator), `Type.of(value)`, `MATCH` exhaustiveness, and clear error messages.

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TypeId(u32);

pub struct TypeRegistry {
    defs: Vec<TypeDef>,
    by_name: HashMap<StringId, TypeId>,
}

pub enum TypeDef {
    Builtin(BuiltinType),
    Sum {
        name: StringId,
        type_params: SmallVec<[StringId; 2]>,
        variants: SmallVec<[VariantDef; 4]>,
    },
}

pub struct VariantDef { name: StringId, idx: u8, arity: u8 }

// BuiltinType covers primitives only; Option/Result are sum types
pub enum BuiltinType { Bool, Int, Float, String, Array, Object }

// For annotations only; not stored in values
pub enum TypeExpr {
    Named(TypeId),
    App(TypeId, SmallVec<[TypeExpr; 2]>),  // e.g., Result[Int, String]
}
```

**No `Value::None`**: Instead of a special `None` primitive, we use `Option.None` represented as `Tagged(OPTION_TYPE_ID, 0, [])`. This is consistent with `Option` being a proper sum type. `GET(...)` returns `Option.Some(v)` or `Option.None`; `??` operates on `Option` types.

**Why `TypeId` instead of `StringId` for sum types?**
- O(1) type comparison (compare `u32` vs string lookup)
- Enables exhaustiveness checking (registry knows all variants)
- Clear error messages via `type_name()`/`variant_name()` lookups

**Type applications** (`Array[Int]`, `Result[T, E]`): Represented as `TypeExpr::App(base, params)`. Values store only the base `TypeId`; type parameters live in annotations and are validated against actual payload types at runtime. This avoids monomorphization (type explosion) while preserving full type checking.

**Why `SmallVec`?** Most type-related collections are small:
- Type params: 0-2 (`Option[T]`, `Result[T, E]`)
- Variants: 2-4 (`Option`: 2, `Result`: 2, typical enums: 3-5)
- Payloads: 0-2 (`None`: 0, `Ok(v)`: 1)

`SmallVec` stores these inline, avoiding heap allocation for the common case.

### Async Interpreter

The interpreter **must be async** because the `Database` and `Transaction` APIs are async:

```rust
// Database methods are async
async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>
async fn transaction<F, Fut>(&self, f: F) -> Result<T>

// Transaction methods are async
async fn get(&self, name: &Name, key: &Key) -> Result<Option<Value>>
async fn set(&self, name: &Name, key: &Key, value: Value) -> Result<()>
```

This means:
- `eval_expr` and `exec_stmt` are `async fn`
- All variable access (both locals and globals) goes through async `Database` methods
- Transactions in the DSL map to `db.transaction(...)` calls
- Tests use `#[tokio::test]`

Even locals use async methods (`db.set_local(...)`, `db.get_local(...)`), so nearly everything in the interpreter is async. Only pure computations (arithmetic, string ops) are truly sync, but wrapped in async for uniformity.

**Why locals are async too**: Locals support full subscripting (`SET x(1,2,3) = "value"`), making them tree-structured just like globals. They use the same B-tree implementation (in-memory only, not persisted), which is inherently async. Distinguishing "simple" locals (`SET x = 1`) from subscripted locals would add interpreter complexity for marginal gain; the real bottleneck in a query language is disk I/O for globals, not async overhead on immediately-ready futures.

### Train-Case Handling
The lexer needs to be whitespace-aware for `-`:
- `my-var` -> single `Ident("my-var")` token
- `a - b` -> `Ident("a")`, `Minus`, `Ident("b")`

Rule: `-` followed immediately by `[a-zA-Z]` is part of an identifier.

### Case Sensitivity
- **Keywords**: Case-insensitive (`SET` = `set` = `Set`)
- **Identifiers**: Case-sensitive (`myVar` != `MyVar`)

### Statement Termination and Indentation

**Newlines always terminate statements/expressions.** This applies uniformly, even inside `{}` blocks:

```rumps
TRANSACTION {
  SET ^P(123) = 1   ; newline terminates this statement
  SET ^X(321) = 2   ; newline terminates this statement
}
```

**Why braces if newlines are significant?** Braces delimit logical blocks (`TRANSACTION`, `FUN`, `IF` bodies); newlines terminate statements. These are orthogonal concerns. Go uses the same model (automatic semicolon insertion at newlines, braces for blocks). Alternatives like `DO...END` are less ergonomic since editors auto-pair `{}` but not keyword pairs.

**Continuation requires explicit indentation.** The lexer emits `Indent` and `Dedent` tokens based on indentation changes:

```rumps
COLLECT ^DATA
  WHERE key[0] > 100    ; Indent emitted before WHERE (continuation)
  SELECT value          ; Same indent level, no extra token
  OUTPUT                ; Same indent level
                        ; Dedent emitted when returning to base level
```

This allows the parser to group continuation lines without requiring explicit line-continuation characters.

Indentation rules:
- Track an indentation stack
- On newline, compare next line's indentation to current level
- If deeper: emit `Indent`, push level (this is a continuation)
- If shallower: emit `Dedent` for each level popped
- If same: no special token (just `Newline`)

### String Interpolation
Defer to Phase 2. For now, strings are plain literals.

### Globals
Defer to Phase 2. For now, focus on locals only (no DB integration needed).

## Success Criteria

The following program should work:

```rumps
; LET for simple bindings (sync, no subscripts)
LET x = 10
LET y = 20
LET sum = x + y
OUTPUT sum

; SET for B-tree locals (async, subscriptable)
SET z = 100
SET z(1, "ABC") = 30

; Comparison and branching
IF sum > 25 {
  OUTPUT "Large sum"
} ELSE {
  OUTPUT "Small sum"
}
```

