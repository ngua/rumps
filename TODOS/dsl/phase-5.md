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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DataExpr {
    pub(crate) var: VarRef,
}
```

##### 5.2.5.2 Update ExprKind

Add `ExprKind::Data(DataExpr)` variant.

##### 5.2.5.3 Parser Implementation

```rust
/// `DATA var_ref`
fn data_expr(
    stmt: impl Parser<Token, cst::Stmt, Error = ParseErr> + Clone + 'static,
) -> impl Parser<Token, cst::Expr, Error = ParseErr> {
    just(Token::Data)
        .ignore_then(Self::var_ref(stmt))
        .map_with_span(|var, span| {
            cst::Expr::new(cst::ExprKind::Data(cst::DataExpr { var }), span)
        })
}
```

### 5.2.6 AST

##### 5.2.6.1 Add AST Type

Add to `ast.rs`:

```rust
/// Data query expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DataExpr {
    pub(crate) var: VarRef,
}
```

##### 5.2.6.2 Update Expr Enum

Add `Expr::Data(DataExpr)` variant.

### 5.2.7 Lowering (CST -> AST)

```rust
cst::ExprKind::Data(data) => {
    let var = lower_var_ref(ast, data.var)?;
    Expr::Data(ast::DataExpr { var })
}
```

### 5.2.8 Builtin Type Registration

Follow the same pattern as `Ordering`. Register `DataStatus` as a builtin sum type:

##### 5.2.8.1 Type System (`typecheck/ty.rs`)

Add `Ty::DataStatus` variant to the `Ty` enum alongside other primitives:

```rust
pub(crate) enum Ty {
    // ... existing primitives ...
    Ordering,
    DataStatus,  // NEW
    FilePath,
    // ...
}
```

Update all match arms in `Ty` methods (`free_vars`, `occurs`, `apply`) to handle `DataStatus` same as other primitives.

##### 5.2.8.2 TypeId Constant (`value.rs`)

Add constant for the type ID:

```rust
impl TypeId {
    // ... existing constants ...
    pub(crate) const ORDERING: Self = Self(17);
    pub(crate) const DATA_STATUS: Self = Self(21);  // next available after REGEX(20)
    // ...
}
```

##### 5.2.8.3 Type Registry (`value.rs`, `register_builtins`)

Register as `TypeDef::Sum` with four nullary variants:

```rust
// DataStatus at index 21
let data_status_name = arena.intern("DataStatus");
let no_data = arena.intern("NoData");
let has_value = arena.intern("HasValue");
let has_descendants = arena.intern("HasDescendants");
let both = arena.intern("Both");

let data_status = self.register(
    TypeDef::Sum {
        name: data_status_name,
        type_params: SmallVec::new(),
        variants: smallvec::smallvec![
            VariantDef { name: no_data, idx: 0, arity: 0, payloads: SmallVec::new() },
            VariantDef { name: has_value, idx: 1, arity: 0, payloads: SmallVec::new() },
            VariantDef { name: has_descendants, idx: 2, arity: 0, payloads: SmallVec::new() },
            VariantDef { name: both, idx: 3, arity: 0, payloads: SmallVec::new() },
        ],
    },
    data_status_name,
);
(data_status == TypeId::DATA_STATUS).then_some(()).ok_or_else(|| ...)?;
```

**Note**: The variant `idx` values (`0`, `1`, `2`, `3`) are internal indices, NOT the MUMPS integer values. The `AS Int` conversion maps variant indices to MUMPS values (`0`, `1`, `10`, `11`).

##### 5.2.8.4 Interning (`value.rs`, `intern_ty`)

Add case for `Ty::DataStatus`:

```rust
Ty::DataStatus => self.named(TypeId::DATA_STATUS),
```

##### 5.2.8.5 Type Name Resolution (`typecheck/infer/convert.rs`)

Add to `parse_ty_name`:

```rust
"DataStatus" => Ty::DataStatus,
```

### 5.2.9 Typechecker

##### 5.2.9.1 DATA Expression

The `DATA` expression always returns `DataStatus`:

```rust
fn data(&mut self, data: &DataExpr, span: Span) -> TyId {
    // Typecheck the variable reference (any subscripted var is valid)
    self.var_ref(&data.var, span);

    // Always returns DataStatus
    self.ty(Ty::DataStatus)
}
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
pub enum Value {
    // ... existing variants ...
    DataStatus(rumps_types::DataStatus),
}
```

##### 5.2.10.2 DATA Evaluation

```rust
async fn data(&mut self, data: &DataExpr) -> Result<Value> {
    let (name, key) = self.resolve_var_ref(&data.var).await?;

    let status = match &self.txn {
        Some(txn) => txn.data(&name, &key).await?,
        None => self.db.data(&name, &key).await?,
    };

    Ok(Value::DataStatus(status))
}
```

##### 5.2.10.3 AS Int Conversion

```rust
fn coerce_as(&self, val: Value, target: &Ty, span: Span) -> Result<Value> {
    match (val, target) {
        // ... existing coercions ...
        (Value::DataStatus(s), Ty::Int) => Ok(Value::Int(s as i64)),
        // ...
    }
}
```

### 5.2.11 Tests

**NOTE**: Integration tests must use **locals** only; globals require `TRANSACTION` blocks which are not yet supported in RUMPS scripts.

- [ ] Parser tests for `DATA ^var` and `DATA local`
- [ ] Typechecker tests for `DataStatus` return type
- [ ] Typechecker tests for `AS Int` coercion
- [ ] Typechecker tests for `READ DataStatus` (fallible)
- [ ] Interpreter tests for all four status values
- [ ] Integration test script (`101_data_primitive.rumps`) using locals

### 5.2.12 Implementation Checklist

- [ ] **Lexer**: Add `Token::Data` keyword
- [ ] **CST**: Add `DataExpr` type
- [ ] **CST**: Add `ExprKind::Data` variant
- [ ] **Parser**: Implement `data_expr` parser
- [ ] **AST**: Add `DataExpr` type
- [ ] **AST**: Add `Expr::Data` variant
- [ ] **Lowering**: Convert `cst::DataExpr` to `ast::DataExpr`
- [ ] **Ty enum**: Add `Ty::DataStatus` variant to `typecheck/ty.rs`
- [ ] **Ty methods**: Update `free_vars`, `occurs`, `apply` for `DataStatus`
- [ ] **TypeId**: Add `TypeId::DATA_STATUS` constant
- [ ] **Type Registry**: Register `DataStatus` as `TypeDef::Sum` with 4 variants
- [ ] **Interning**: Add `Ty::DataStatus` case in `intern_ty`
- [ ] **Type Names**: Add `"DataStatus"` to `parse_ty_name`
- [ ] **Unification**: Add `(Ty::DataStatus, Ty::DataStatus)` case
- [ ] **Typechecker**: Infer `Ty::DataStatus` for `DATA` expressions
- [ ] **Coercion**: Register infallible `DataStatus -> Int` (`AS Int`)
- [ ] **Coercion**: Register fallible `Int -> DataStatus` (`READ DataStatus`)
- [ ] **Interpreter**: Add `Value::DataStatus(rumps_types::DataStatus)` variant
- [ ] **Interpreter**: Implement `DATA` evaluation via `Database::data`/`Transaction::data`
- [ ] **Interpreter**: Implement `AS Int` mapping variant idx to MUMPS values
- [ ] **Interpreter**: Implement `READ DataStatus` from Int
- [ ] **Tests**: Integration test script (`101_data_primitive.rumps`)

---

### Design Notes

#### Why a Builtin Enum?

Making `DataStatus` a proper enum type rather than just an integer provides:
1. **Type safety**: Can't accidentally mix with other integers
2. **Pattern matching**: `MATCH status { NoData => ..., HasValue => ..., ... }`
3. **Self-documenting**: Code reads clearly without magic numbers
4. **IDE support**: Autocomplete for variants

#### Infallible AS Int

The `AS Int` coercion is infallible (no `?` needed) because:
1. Every `DataStatus` variant has a defined integer value
2. The conversion cannot fail at runtime
3. This matches the Rust `#[repr(u8)]` semantics

#### Compatibility with MUMPS $DATA

The integer values (`0`, `1`, `10`, `11`) match traditional MUMPS `$DATA` semantics, allowing:
- Existing MUMPS patterns like `IF $DATA(x)>0` translate directly
- The "tens digit" represents descendants, "ones digit" represents value
