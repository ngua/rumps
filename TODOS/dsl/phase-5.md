# Phase 5

## 5.1: Extended OUTPUT Statement

This phase extends the `OUTPUT` statement with formatting and target options.

**Prerequisites**: Phase 4 (type system) complete.

**Testing**: Each feature requires integration tests (`.rumps` script + snapshot).

**IMPORTANT**: All I/O operations MUST use `tokio`. This includes `OUTPUT` to stderr and file targets. Use `tokio::io::stderr` and `tokio::fs::write` (or similar async APIs), NOT the synchronous `std::io` equivalents.

---

### 5.1. Extended OUTPUT Statement

The current `OUTPUT expr` statement writes to stdout. We extend it with:
- **Format modifiers**: `JSON` (convert to JSON before output)
- **Target modifiers**: `TO ERROR` (stderr), `TO FILE "path"` (file)
- **Combinations**: `OUTPUT x JSON TO FILE "path"`

#### 5.1.1 Syntax

```rumps
; Current (unchanged)
OUTPUT x

; Format: convert to JSON first
OUTPUT x JSON

; Target: output to stderr
OUTPUT x TO ERROR

; Target: output to file
OUTPUT x TO FILE "/tmp/out.txt"
OUTPUT x TO FILE path-var        ; FilePath variable

; Combined: format + target
OUTPUT x JSON TO ERROR
OUTPUT x JSON TO FILE "/tmp/out.json"
```

**Grammar**:
```
output_stmt := OUTPUT expr [format] [target]
format      := JSON
target      := TO ERROR | TO FILE expr
```

#### 5.1.2 Contextual Identifiers (Not Keywords)

**Critical requirement**: `TO`, `JSON`, `ERROR`, `FILE` must NOT be global keywords. They should only be recognized specially in the `OUTPUT` context. This preserves backward compatibility:

```rumps
; These must continue to work:
LET json = { "a": 1 }          ; `json` as variable name
LET to = 10                     ; `to` as variable name
LET error = "something failed"  ; `error` as variable name
LET file = "/path/to/file"      ; `file` as variable name

; These are the new OUTPUT modifiers:
OUTPUT data JSON                ; `JSON` recognized after OUTPUT expr
OUTPUT data TO ERROR            ; `TO ERROR` recognized after OUTPUT expr
```

**Implementation approach**: In the parser, after consuming `OUTPUT expr`, optionally match identifiers whose value (case-insensitive) equals `"JSON"`, `"TO"`, `"ERROR"`, or `"FILE"`. Use `filter_map` on `Token::Ident(s)` to check the identifier string.

#### 5.1.3 Lexer

No changes required. `TO`, `JSON`, `ERROR`, `FILE` remain as regular identifiers (`Token::Ident`).

#### 5.1.4 Parser / CST

##### 5.1.4.1 Add Output Modifier Types

Add to `parser/cst.rs`:

```rust
/// Implemented
```

##### 5.1.4.2 Update StmtKind

Change `StmtKind::Output(Expr)` to `StmtKind::Output(OutputStmt)`.

##### 5.1.4.3 Parser Implementation

**NOTE**: As with keywords, `OUTPUT` modifiers must be case-insensitive. E.g. `output x json to error`

Update `output_stmt` in `parser.rs`:

```rust
/// Implemented
```

**Parsing order matters**: The format (`JSON`) must come before the target (`TO ...`) to avoid ambiguity. `OUTPUT x TO ERROR` should not try to parse `TO` as a format.

#### 5.1.5 AST

##### 5.1.5.1 Add AST Types

Add to `ast.rs`:

```rust
/// Implemented
```

##### 5.1.5.2 Update Stmt Enum

Change `Stmt::Output(ExprId)` to `Stmt::Output(OutputStmt)`.

#### 5.1.6 Lowering (CST -> AST)

Update `parser/lower.rs` to convert `cst::OutputStmt` to `ast::OutputStmt`:

```rust
/// Implemented
```

#### 5.1.7 Typechecker

Update `typecheck/infer/stmt.rs`:

```rust
// Implemented
```

#### 5.1.8 Interpreter

Update `interpreter.rs`:

```rust
// Implemented
```

##### 5.1.8.1 IoContext Extensions

The `IoContext` trait needs `stderr` and `write_file` methods.

**NOTE**: All implementations MUST use `tokio` async I/O:
- `stderr`: use `tokio::io::stderr()` with `AsyncWriteExt`
- `write_file`: use `tokio::fs::write()` or `tokio::fs::File`

```rust
// Implemented
```

#### 5.1.9 Tests

- [x] Integration test script (`100_output_extended.rumps`)

#### 5.1.10 Implementation Checklist

- [x] **CST**: Add `OutputFormat`, `OutputTarget`, `OutputStmt` types
- [x] **CST**: Update `StmtKind::Output` to use `OutputStmt`
- [x] **Parser**: Implement contextual identifier matching (`ctx_ident`)
- [x] **Parser**: Update `output_stmt` to parse format and target
- [x] **AST**: Add `OutputFormat`, `OutputTarget`, `OutputStmt` types
- [x] **AST**: Update `Stmt::Output` to use `OutputStmt`
- [x] **Lowering**: Update CST->AST conversion for `Output`
- [x] **Typechecker**: Add format (`Jsonable`) and target (`FilePath | String`) constraints
- [x] **Interpreter**: Implement format application (stringify vs JSON)
- [x] **Interpreter**: Implement target routing (stdout/stderr/file)
- [x] **IoContext**: Add `stderr` method
- [x] **IoContext**: Add `write_file` method
- [x] **Tests**: Integration test script (`100_output_extended.rumps`)

---

### Design Notes

#### Why Contextual Identifiers?

Adding `TO`, `JSON`, `ERROR`, `FILE` as global keywords would:
1. Break existing code using these as variable names
2. Pollute the keyword namespace unnecessarily
3. Make the language less predictable

By parsing them contextually (only after `OUTPUT expr`), we:
1. Keep the keyword set minimal
2. Allow natural variable naming

#### Parser Approach

The parser uses a lookahead pattern:
1. Parse `OUTPUT expr` as before
2. Optionally match `Ident("JSON")` for format
3. Optionally match `Ident("TO")` followed by target

This is straightforward with chumsky's combinators and `filter_map`.

#### Alternative: Special Tokens

An alternative would be to add `Token::Json`, `Token::To`, etc., and make them "soft keywords" that the lexer emits only in certain states. This is more complex and not necessary for our use case.

---

## 5.2: DATA Primitive

The `DATA` primitive queries the existence status of a node in a global or local variable. It wraps `Database::data` and `Transaction::data`.

**Prerequisites**: Phase 4 (type system) complete.

---

### 5.2.1 Syntax

```rumps
; Query data status of a global
DATA ^PATIENT(123)

; Query data status of a local
DATA patients(123)

; Use in expressions
MATCH DATA patients(123) {
  NoData => { ... }
  HasValue => { ... }
  HasDescendants => { ... }
  Both => { ... }
}

LET status = DATA patients(123)
IF (DATA ^PATIENT(123)) AS Int > 0 {
    OUTPUT "Node exists"
}

```

### 5.2.2 DataStatus Builtin Type

Add a builtin enum type `DataStatus` with variants matching the Rust `rumps_types::DataStatus`:

```rumps
TYPE DataStatus = NoData | HasValue | HasDescendants | Both
```

| Variant         | Int Value | Meaning                          |
|-----------------|-----------|----------------------------------|
| `NoData`        | `0`       | No value, no descendants         |
| `HasValue`      | `1`       | Has value only                   |
| `HasDescendants`| `10`      | Has descendants only             |
| `Both`          | `11`      | Has both value and descendants   |

The integer values match MUMPS `$DATA` semantics:
- `0`: Node does not exist
- `1`: Node has a value but no descendants
- `10`: Node has descendants but no value
- `11`: Node has both value and descendants

### 5.2.3 Infallible Conversion to Int

`DataStatus` must support infallible conversion via `AS Int`:

```rumps
LET status = DATA ^PATIENT(123)
LET n: Int = status AS Int   ; Always succeeds

```

This mirrors the Rust `#[repr(u8)]` on `DataStatus`.

**NOTE**: `(x: Int) READ DataStatus` must be supported as well (inverse). This is **fallible**.

### 5.2.4 Lexer

Add `DATA` keyword to the lexer:

```rust
Token::Data  // new keyword
```

### 5.2.5 Parser / CST

##### 5.2.5.1 Add Data Expression

Add to `parser/cst.rs`:

```rust
/// Data query expression.
```

##### 5.2.5.2 Update ExprKind

Add `ExprKind::Data(DataExpr)` variant.

##### 5.2.5.3 Parser Implementation

```rust
/// `DATA var_ref`
```

### 5.2.6 AST

##### 5.2.6.1 Add AST Type

Add to `ast.rs`:

```rust
/// Data query expression.
```

##### 5.2.6.2 Update Expr Enum

Add `Expr::Data(DataExpr)` variant.

### 5.2.7 Lowering (CST -> AST)

```rust
///
```

### 5.2.8 Builtin Type Registration

Follow the same pattern as `Ordering`. Register `DataStatus` as a builtin sum type:

##### 5.2.8.1 Type System (`typecheck/ty.rs`)

Add `Ty::DataStatus` variant to the `Ty` enum alongside other primitives:

```rust
///
```

Update all match arms in `Ty` methods (`free_vars`, `occurs`, `apply`) to handle `DataStatus` same as other primitives.

##### 5.2.8.2 TypeId Constant (`value.rs`)

Add constant for the type ID:

```rust
///
```

##### 5.2.8.3 Type Registry (`value.rs`, `register_builtins`)

Register as `TypeDef::Sum` with four nullary variants:

```rust
// DataStatus at index 21
```

**Note**: The variant `idx` values (`0`, `1`, `2`, `3`) are internal indices, NOT the MUMPS integer values. The `AS Int` conversion maps variant indices to MUMPS values (`0`, `1`, `10`, `11`).

##### 5.2.8.4 Interning (`value.rs`, `intern_ty`)

Add case for `Ty::DataStatus`:

```rust
///
```

##### 5.2.8.5 Type Name Resolution (`typecheck/infer/convert.rs`)

Add to `parse_ty_name`:

```rust
///
```

### 5.2.9 Typechecker

##### 5.2.9.1 DATA Expression

The `DATA` expression always returns `DataStatus`:

```rust
///
```

##### 5.2.9.2 Unification (`typecheck/unify.rs`)

Add `DataStatus` to the trivial unification cases:

```rust
| (Ty::Ordering, Ty::Ordering)
| (Ty::DataStatus, Ty::DataStatus)  // NEW
| (Ty::FilePath, Ty::FilePath)
```

##### 5.2.9.3 Infallible AS Int Coercion

Register infallible coercion from `DataStatus` to `Int`. In `typecheck/infer/convert.rs`:

```rust
// DataStatus -> Int is infallible
(Ty::DataStatus, Ty::Int) => true,
```

This allows `status AS Int` without `?` or error handling.

##### 5.2.9.4 Fallible Int -> DataStatus (READ)

The inverse `(x: Int) READ DataStatus` is **fallible** since only `0`, `1`, `10`, `11` are valid:

```rust
// Int -> DataStatus is fallible (only 0, 1, 10, 11 valid)
(Ty::Int, Ty::DataStatus) => /* fallible, requires ? */
```

### 5.2.10 Interpreter

##### 5.2.10.1 Value Representation

Add `DataStatus` variant to runtime values:

```rust
///
```

##### 5.2.10.2 DATA Evaluation

```rust
///
```

##### 5.2.10.3 AS Int Conversion

```rust
///
```

### 5.2.11 Tests

**NOTE**: Integration tests must use **locals** only; globals require `TRANSACTION` blocks which are not yet supported in RUMPS scripts.

- [x] Parser tests for `DATA ^var` and `DATA local`
- [x] Integration test script (`101_data_primitive.rumps`) using locals

### 5.2.12 Implementation Checklist

- [x] **Lexer**: Add `Token::Data` keyword
- [x] **CST**: Add `DataExpr` type
- [x] **CST**: Add `ExprKind::Data` variant
- [x] **Parser**: Implement `data_expr` parser
- [x] **AST**: Add `DataExpr` type
- [x] **AST**: Add `Expr::Data` variant
- [x] **Lowering**: Convert `cst::DataExpr` to `ast::DataExpr`
- [x] **Ty enum**: Add `Ty::DataStatus` variant to `typecheck/ty.rs`
- [x] **Ty methods**: Update `free_vars`, `occurs`, `apply` for `DataStatus`
- [x] **TypeId**: Add `TypeId::DATA_STATUS` constant
- [x] **Type Registry**: Register `DataStatus` as `TypeDef::Sum` with 4 variants
- [x] **Interning**: Add `Ty::DataStatus` case in `intern_ty`
- [x] **Type Names**: Add `"DataStatus"` to `parse_ty_name`
- [x] **Unification**: Add `(Ty::DataStatus, Ty::DataStatus)` case
- [x] **Typechecker**: Infer `Ty::DataStatus` for `DATA` expressions
- [x] **Coercion**: Register infallible `DataStatus -> Int` (`AS Int`)
- [x] **Coercion**: Register fallible `Int -> DataStatus` (`READ DataStatus`)
- [x] **Interpreter**: Implement `DATA` evaluation via `Database::data`/`Transaction::data`
- [x] **Interpreter**: Implement `AS Int` mapping variant idx to MUMPS values
- [x] **Interpreter**: Implement `READ DataStatus` from Int
- [x] **Exhaustiveness**: Add `Ty::DataStatus` case to `check_exhaustiveness`
- [x] **Tests**: Integration test script (`101_data_primitive.rumps`)

---

### Design Notes

#### Why a Builtin Enum?

Making `DataStatus` a proper enum type rather than just an integer provides:
1. **Type safety**: Can't accidentally mix with other integers
2. **Pattern matching**: `MATCH status { NoData => ..., HasValue => ..., ... }`
3. **Self-documenting**: Code reads clearly without magic numbers
4. **IDE support**: Autocomplete for variants

#### Infallible AS Int

The `AS Int` coercion is infallible because:
1. Every `DataStatus` variant has a defined integer value
2. The conversion cannot fail at runtime
3. This matches the Rust `#[repr(u8)]` semantics

#### Compatibility with MUMPS $DATA

The integer values (`0`, `1`, `10`, `11`) match traditional MUMPS `$DATA` semantics, allowing:
- Existing MUMPS patterns like `IF $DATA(x)>0` translate directly
- The "tens digit" represents descendants, "ones digit" represents value

---

## 5.3: ORDER Primitive

The `ORDER` primitive returns the next subscript at a given level in sorted order. It wraps `Database::order` and `Transaction::order`.

**Prerequisites**: Phase 4 (type system) complete.

---

### 5.3.1 Syntax

```rumps
; Get the next subscript after "foo" at level 1 of patients
LET next = ORDER patients("foo")

; Get the first subscript at level 1 (no "after" value)
LET first = ORDER patients()

; Nested: next subscript at level 2 under key 123
LET next = ORDER ^PATIENT(123, "A")

; Use in expressions
MATCH ORDER data(key) {
  Some(k) => { OUTPUT k }
  None => { OUTPUT "no more keys" }
}

; Iterate all keys at a level
LET k = ORDER patients()
WHILE k IS Some {
  OUTPUT k!
  SET k = ORDER patients(k!)
}
```

### 5.3.2 Subscript Builtin Union Type

The `Subscript` builtin union type already exists:

```rumps
UNION Subscript = Bool | Int | Float | Char | String | Json
```

This was added to support union-typed arrays (e.g., `Array[Subscript]`). It matches `rumps_types::Subscript` in the storage layer.

**Note**: `Int` and `Float` both map to `Subscript::Number(f64)` in storage; they are unified at the storage layer.

### 5.3.3 ORDER Returns `Option[Subscript]`

Following MUMPS `$ORDER` semantics, `ORDER` returns the **next subscript** at the given level, not a full key path:

```rumps
; If patients has keys (1), (2), (10):
ORDER patients()      ; => Some(1)
ORDER patients(1)     ; => Some(2)
ORDER patients(2)     ; => Some(10)
ORDER patients(10)    ; => None
```

### 5.3.4 Lexer

Add `ORDER` keyword to the lexer:

```rust
Token::Order  // new keyword
```

### 5.3.5 Parser / CST

##### 5.3.5.1 Add Order Expression

Add to `parser/cst.rs`:

```rust
/// Order query expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrderExpr {
    pub(crate) var: VarRef,
}
```

##### 5.3.5.2 Update ExprKind

Add `ExprKind::Order(OrderExpr)` variant.

##### 5.3.5.3 Parser Implementation

```rust
/// `ORDER var_ref`
fn order_expr(
    stmt: impl Parser<Token, cst::Stmt, Error = ParseErr> + Clone + 'static,
) -> impl Parser<Token, cst::Expr, Error = ParseErr> {
    just(Token::Order)
        .ignore_then(Self::var_ref(stmt))
        .map_with_span(|var, span| {
            cst::Expr::new(cst::ExprKind::Order(cst::OrderExpr { var }), span)
        })
}
```

### 5.3.6 AST

##### 5.3.6.1 Add AST Type

Add to `ast.rs`:

```rust
/// Order query expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrderExpr {
    pub(crate) var: VarRef,
}
```

##### 5.3.6.2 Update Expr Enum

Add `Expr::Order(OrderExpr)` variant.

### 5.3.7 Lowering (CST -> AST)

```rust
cst::ExprKind::Order(order) => {
    let var = lower_var_ref(ast, order.var)?;
    Expr::Order(ast::OrderExpr { var })
}
```

### 5.3.8 Typechecker

##### 5.3.8.1 ORDER Expression

The `ORDER` expression returns `Option[Subscript]`:

```rust
fn order(&mut self, order: &OrderExpr, span: Span) -> TyId {
    // Typecheck the variable reference subscripts
    self.var_ref(&order.var, span);

    // Returns Option[Subscript]
    let subscript = self.ty(Ty::Named(TypeId::SUBSCRIPT, vec![]));
    self.ty(Ty::Option(Box::new(subscript)))
}
```

##### 5.3.8.2 Subscript Constraint for Subscripts

Subscripts in variable references should be constrained to `Subscript` type. This may require updating `var_ref` type checking to unify each subscript with `Subscript`.

### 5.3.9 Interpreter

##### 5.3.9.1 ORDER Evaluation

```rust
async fn order(&mut self, order: &OrderExpr) -> Result<Value> {
    let (name, key) = self.resolve_var_ref(&order.var).await?;

    let opt_sub = match &self.txn {
        Some(txn) => txn.order(&name, &key).await?,
        None => self.db.order(&name, &key).await?,
    };

    opt_sub.map_or_else(
        || Ok(self.none()),
        |sub| self.subscript_to_value(sub).map(|v| self.some(v)),
    )
}
```

##### 5.3.9.2 Subscript to Value Conversion

Add conversion from `rumps_types::Subscript` to runtime `Value`:

```rust
fn subscript_to_value(&mut self, sub: Subscript) -> Result<Value> {
    Ok(match sub {
        Subscript::Boolean(b) => Value::Bool(b),
        Subscript::Number(n) => {
            // Check if it's a whole number
            let f = n.into_inner();
            if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
                Value::Int(f as i64)
            } else {
                Value::Float(n)
            }
        }
        Subscript::Char(c) => Value::Char(c),
        Subscript::String(s) => Value::String(self.arena.intern(&s)),
        Subscript::Json(j) => Value::Json(j),
    })
}
```

##### 5.3.9.3 Fix Json Subscript Support in `convert.rs`

Currently `convert.rs` `subscript()` does NOT support `Value::Json`, but `rumps_types::Subscript` does include `Json`. Update to support it:

```rust
pub(crate) fn subscript(&self, v: &Value) -> Result<Subscript> {
    match v {
        Value::Bool(b) => Ok(Subscript::Boolean(*b)),
        Value::Int(i) => Ok(Subscript::Number(OrderedFloat(*i as f64))),
        Value::Float(f) => Ok(Subscript::Number(*f)),
        Value::Char(c) => Ok(Subscript::String(c.to_string())),
        Value::String(id) => { /* existing */ }
        Value::Json(j) => Ok(Subscript::Json(j.clone())),  // NEW: add Json support
        _ => Err(...)
    }
}
```

This fixes the mismatch between `rumps_types::Subscript` (which includes `Json`) and the runtime conversion (which previously rejected it).

### 5.3.10 Tests

**NOTE**: Integration tests must use **locals** only; globals require `TRANSACTION` blocks.

- [ ] Parser tests for `ORDER local` and `ORDER ^global`
- [ ] Integration test script (`103_order_primitive.rumps`) using locals

### 5.3.11 Implementation Checklist

- [x] **Lexer**: Add `Token::Order` keyword
- [x] **CST**: Add `ExprKind::Order` variant
- [x] **Parser**: Implement `order_expr` parser
- [x] **AST**: Add `Expr::Order` variant
- [x] **Lowering**: Convert CST `Order` to AST `Order`
- [x] **TypeId**: Add `TypeId::SUBSCRIPT` constant
- [x] **Type Registry**: Register `Subscript` as `TypeDef::Union` with 6 members
- [x] **Type Names**: Add `"Subscript"` to `parse_ty_name` (via registry lookup)
- [x] **Typechecker**: Infer `Option[Subscript]` for `ORDER` expressions
- [x] **Interpreter**: Implement `ORDER` evaluation via `Database::order`/`Transaction::order`
- [x] **Interpreter**: Add `subscript_to_value` conversion
- [x] **Interpreter**: Fix `convert.rs` `subscript()` to support `Json` values
- [x] **Tests**: Integration test script (`103_order_primitive.rumps`)

---

### Design Notes

#### Why MUMPS `$ORDER` Semantics?

MUMPS `$ORDER` returns the next subscript at a given level. This is intuitive for tree iteration:

1. **Familiarity**: MUMPS users expect `$ORDER` to return a single value
2. **Simplicity**: No need to extract elements from an array
3. **Iteration**: Natural `WHILE` loop pattern works cleanly

If full key path iteration is needed, a separate `QUERY` primitive (matching MUMPS `$QUERY`) can be added later.

#### Subscript Union vs Storable

`Subscript` and `Storable` are currently identical:

| Union       | Members                                |
|-------------|----------------------------------------|
| `Storable`  | `Bool \| Int \| Float \| Char \| String \| Json` |
| `Subscript` | `Bool \| Int \| Float \| Char \| String \| Json` |

They are semantically distinct:
- `Storable`: values that can be stored in the database
- `Subscript`: values that can be used as subscripts in keys

They may diverge if storage adds new types that aren't valid subscripts.

#### Number Representation

`Subscript::Number(f64)` at the storage layer represents both `Int` and `Float`. When converting back to runtime values, we check if the number is a whole number and return `Int` if so. This preserves the user's likely intent when they used `ORDER` with integer subscripts
