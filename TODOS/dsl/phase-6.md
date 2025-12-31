# Phase 6: Transaction Support

This phase adds transaction blocks to the RUMPS query language, enabling writes to global variables with ACID guarantees.

**Prerequisites**: Phase 5 (database primitives) complete.

**Testing**: Each sub-phase requires integration tests (`.rumps` script + snapshot).

---

## Overview

RUMPS requires explicit transactions for all writes to persistent globals. This phase adds:

1. **Basic transaction blocks**: `TRANSACTION { ... }`
2. **Transaction as expression**: Returns `Result[T, String]` where `T` is the final expression type
3. **Modifiers via contextual parsing**: `ON CONFLICT`, `WITH TIMEOUT`, etc.
4. **Error handling**: Propagate storage layer errors to the query language

### What TransactionBuilder Supports

After analyzing `rumps-storage/src/transaction.rs`, the `TransactionBuilder` provides:

| Method                          | Description                   |
|---------------------------------|-------------------------------|
| `conflict(ConflictStrategy)`    | Conflict resolution strategy  |
| `timeout(u64)`                  | Timeout in milliseconds       |
| `priority(TransactionPriority)` | Transaction priority          |
| `isolation(IsolationLevel)`     | Isolation level               |
| `retries(u32)`                  | Number of retries on conflict |

#### ConflictStrategy Enum

```rust
pub enum ConflictStrategy {
    Abort,       // Default; fail on conflict
    Retry(u32),  // Retry up to N times
    Skip,        // Skip transaction on conflict
    Overwrite,   // Last-write-wins
}
```

#### TransactionPriority Enum

```rust
pub enum TransactionPriority {
    Low,
    Normal,  // Default
    High,
}
```

#### IsolationLevel Enum

```rust
pub enum IsolationLevel {
    SnapshotIsolation,  // Default; only option currently
}
```

### What Is NOT Yet Supported (Storage Layer)

The following features from `TODOS/dsl.md` are NOT currently supported by `rumps-storage`:

| Feature                                    | Status        | Notes                                           |
|--------------------------------------------|---------------|-------------------------------------------------|
| `ON CONFLICT DO block`                     | Not supported | Would require custom error handler              |
| `WITH ISOLATION SERIALIZABLE`              | Not supported | Only `SnapshotIsolation` exists                 |
| `WITH ISOLATION READ-COMMITTED`            | Not supported | Only `SnapshotIsolation` exists                 |
| `SAVEPOINT name`                           | Not supported | No checkpoint/rollback methods on `Transaction` |
| Nested `TRANSACTION` blocks                | Not supported | Would require savepoint support                 |
| `Txn.id`, `Txn.start-time`, `Txn.op-count` | Not supported | Context variables not exposed                   |

These features may be added to `rumps-storage` in future phases if needed.

### What Is NOT Yet Supported (Language)

The following are general language features not yet implemented:

| Feature             | Status        | Notes                                              |
|---------------------|---------------|----------------------------------------------------|
| `CATCH e => { ... }`| Not supported | General `Result[T, E]` error handling; not storage-specific |
| `FINALLY { ... }`   | Not supported | Cleanup block for any expression; not storage-specific      |

These are orthogonal to transaction support and could be added as a separate language feature.

---

## 6.1: Basic Transaction Block

The minimal transaction block without modifiers.

### 6.1.1 Syntax

```rumps
; Basic transaction block
TRANSACTION {
    $SET ^PATIENT(123, "NAME") = "John"
    $SET ^PATIENT(123, "AGE") = 30
}

; Transaction as expression
LET result = TRANSACTION {
    $SET ^DATA(1) = "value"
    $GET ^DATA(1)
}
; result is Result[Option[Storable], String]
```

**Grammar**:
```
txn_expr := TRANSACTION block
block    := { stmt* [expr] }
```

### 6.1.2 Expression Semantics

`TRANSACTION { ... }` is an **expression** that:

1. Creates a new transaction via `TransactionBuilder`
2. Executes the block body with `self.txn = Some(txn)`
3. Returns `Result[T, String]` where:
   - `T` is the type of the trailing expression (or `Unit` if none)
   - On success: `Result.Ok(value)`
   - On failure: `Result.Err(error_message)`

The `String` error type captures the storage layer error message.

### 6.1.3 Lexer

Add `TRANSACTION` as a keyword:

```rust
// In token.rs, Token enum:
Transaction,  // new keyword

// In Token::keyword():
"TRANSACTION" => Some(Self::Transaction),

// In Token::fmt():
Self::Transaction => write!(f, "TRANSACTION"),
```

### 6.1.4 Parser / CST

##### 6.1.4.1 Add Transaction Types

Add to `parser/cst.rs`:

```rust
/// Transaction block expression.
#[derive(Clone, Debug)]
pub(crate) struct TransactionExpr {
    /// Statements in the transaction body.
    pub(crate) stmts: Vec<Stmt>,
    /// Optional trailing expression (return value).
    pub(crate) expr: Option<Box<Expr>>,
    /// Transaction modifiers (added in phase 6.2).
    pub(crate) modifiers: TransactionModifiers,
}

/// Transaction configuration modifiers.
#[derive(Clone, Debug, Default)]
pub(crate) struct TransactionModifiers {
    pub(crate) conflict: Option<ConflictModifier>,
    pub(crate) timeout: Option<Box<Expr>>,
    pub(crate) priority: Option<PriorityModifier>,
    pub(crate) isolation: Option<IsolationModifier>,
}

/// Conflict resolution strategy.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ConflictModifier {
    Abort,
    Retry(u32),
    Skip,
    Overwrite,
}

/// Transaction priority.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PriorityModifier {
    Low,
    Normal,
    High,
}

/// Isolation level.
#[derive(Clone, Copy, Debug)]
pub(crate) enum IsolationModifier {
    Snapshot,
}
```

##### 6.1.4.2 Update ExprKind

Add to `ExprKind`:

```rust
/// Transaction block expression.
Transaction(Box<TransactionExpr>),
```

##### 6.1.4.3 Parser Implementation

```rust
/// `TRANSACTION { stmts... [expr] }`
fn transaction_expr(
    stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone + 'static,
) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
    just(Token::Transaction)
        .ignore_then(Self::block_body(stmt))
        .map_with_span(|(stmts, expr), span| {
            cst::Expr::new(
                cst::ExprKind::Transaction(Box::new(cst::TransactionExpr {
                    stmts,
                    expr: expr.map(Box::new),
                    modifiers: cst::TransactionModifiers::default(),
                })),
                span,
            )
        })
}
```

Add to `expr` combinator alternatives:
```rust
Self::transaction_expr(stmt.clone()),
```

### 6.1.5 AST

##### 6.1.5.1 Add AST Types

Add to `ast.rs`:

```rust
/// Transaction block expression.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransactionExpr {
    /// Statements in the transaction body.
    pub(crate) stmts: Vec<StmtId>,
    /// Optional trailing expression (return value).
    pub(crate) expr: Option<ExprId>,
    /// Transaction modifiers.
    pub(crate) modifiers: TransactionModifiers,
}

/// Transaction configuration modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TransactionModifiers {
    pub(crate) conflict: Option<ConflictStrategy>,
    pub(crate) timeout: Option<ExprId>,
    pub(crate) priority: Option<TransactionPriority>,
    pub(crate) isolation: Option<IsolationLevel>,
}

/// Re-export storage types for AST use.
pub(crate) use rumps_storage::{
    ConflictStrategy, IsolationLevel, TransactionPriority,
};
```

**Note**: We reuse `ConflictStrategy`, `TransactionPriority`, and `IsolationLevel` from `rumps_storage` since they match exactly.

##### 6.1.5.2 Update Expr Enum

Add to `Expr`:

```rust
/// Transaction block expression.
Transaction(TransactionExpr),
```

### 6.1.6 Lowering (CST -> AST)

Add to `parser/lower.rs`:

```rust
cst::ExprKind::Transaction(txn) => {
    let stmts = txn.stmts.into_iter()
        .map(|s| lower_stmt(ast, s))
        .collect::<Result<Vec<_>>>()?;
    let expr = txn.expr.map(|e| lower_expr(ast, *e)).transpose()?;
    let modifiers = lower_txn_modifiers(ast, txn.modifiers)?;
    Expr::Transaction(TransactionExpr { stmts, expr, modifiers })
}

fn lower_txn_modifiers(
    ast: &mut Ast,
    m: cst::TransactionModifiers,
) -> Result<TransactionModifiers> {
    let conflict = m.conflict.map(|c| match c {
        cst::ConflictModifier::Abort => ConflictStrategy::Abort,
        cst::ConflictModifier::Retry(n) => ConflictStrategy::Retry(n),
        cst::ConflictModifier::Skip => ConflictStrategy::Skip,
        cst::ConflictModifier::Overwrite => ConflictStrategy::Overwrite,
    });
    let timeout = m.timeout.map(|e| lower_expr(ast, *e)).transpose()?;
    let priority = m.priority.map(|p| match p {
        cst::PriorityModifier::Low => TransactionPriority::Low,
        cst::PriorityModifier::Normal => TransactionPriority::Normal,
        cst::PriorityModifier::High => TransactionPriority::High,
    });
    let isolation = m.isolation.map(|i| match i {
        cst::IsolationModifier::Snapshot => IsolationLevel::SnapshotIsolation,
    });
    Ok(TransactionModifiers { conflict, timeout, priority, isolation })
}
```

### 6.1.7 Typechecker

##### 6.1.7.1 Transaction Expression

Add to `typecheck/infer/expr.rs`:

```rust
Expr::Transaction(txn) => self.transaction(txn, span),
```

##### 6.1.7.2 Implementation

```rust
fn transaction(&mut self, txn: &TransactionExpr, span: Span) -> TyId {
    // Enter new scope for transaction body
    self.env.push_scope();

    // Typecheck all statements
    txn.stmts.iter().for_each(|&stmt_id| {
        self.stmt(stmt_id);
    });

    // Typecheck trailing expression or default to Unit
    let inner_ty = txn.expr.map_or_else(
        || self.ty(Ty::Unit),
        |expr_id| self.infer(expr_id),
    );

    // Typecheck modifiers
    txn.modifiers.timeout.iter().for_each(|&timeout_id| {
        let timeout_ty = self.infer(timeout_id);
        let int_ty = self.ty(Ty::Int);
        self.unify(timeout_ty, int_ty, span);
    });

    self.env.pop_scope();

    // Return Result[T, String]
    let string_ty = self.ty(Ty::String);
    self.ty(Ty::Result(Box::new(inner_ty), Box::new(string_ty)))
}
```

**Note**: Transaction body creates a new scope so local `LET` bindings don't leak out.

### 6.1.8 Interpreter

##### 6.1.8.1 Transaction Evaluation

Add to `interpreter.rs`:

```rust
Expr::Transaction(txn) => self.transaction(txn, span).await,
```

##### 6.1.8.2 Implementation

```rust
/// Evaluate a transaction block expression.
///
/// Returns `Result[T, String]` where `T` is the trailing expression type.
async fn transaction(
    &mut self,
    txn: &TransactionExpr,
    span: Span,
) -> Result<Value> {
    use rumps_storage::TransactionBuilder;

    // Check we're not already in a transaction
    if self.txn.is_some() {
        // Nested transactions not supported
        let err_msg = "nested transactions are not supported";
        let err_id = self.arena.intern(err_msg);
        let type_id = self.type_exprs.result_string_ty();
        Ok(self.make_result_err(type_id, err_id))
    } else {
        // Build transaction with modifiers
        let mut builder = self.db.build_transaction();

        if let Some(conflict) = txn.modifiers.conflict {
            builder = builder.conflict(conflict);
        }
        if let Some(timeout_id) = txn.modifiers.timeout {
            let timeout_val = self.eval(timeout_id).await?;
            let timeout_ms = self.as_int(&timeout_val)?;
            builder = builder.timeout(timeout_ms as u64);
        }
        if let Some(priority) = txn.modifiers.priority {
            builder = builder.priority(priority);
        }
        if let Some(isolation) = txn.modifiers.isolation {
            builder = builder.isolation(isolation);
        }

        // Execute transaction
        let result = self.execute_transaction(builder, &txn.stmts, txn.expr, span).await;

        // Convert to Result[T, String]
        let type_id = self.type_exprs.result_string_ty();
        match result {
            Ok(val) => Ok(self.make_result_ok(type_id, val)),
            Err(e) => {
                let err_msg = e.to_string();
                let err_id = self.arena.intern(&err_msg);
                Ok(self.make_result_err(type_id, err_id))
            }
        }
    }
}

/// Execute transaction body and return the result value or error.
async fn execute_transaction(
    &mut self,
    builder: TransactionBuilder,
    stmts: &[StmtId],
    expr: Option<ExprId>,
    span: Span,
) -> Result<Value> {
    // Start the transaction
    let txn = builder.start().await
        .map_err(|e| Error::runtime(span, format!("failed to start transaction: {e}")))?;

    // Set transaction context
    self.txn = Some(txn.clone());

    // Enter new scope for local bindings
    self.env.push();

    // Execute statements
    let stmt_result = self.stmts(stmts).await;

    // Evaluate trailing expression if statements succeeded
    let body_result = match stmt_result {
        Err(e) => Err(e),
        Ok(()) => match expr {
            None => Ok(Value::Unit),
            Some(expr_id) => self.eval(expr_id).await,
        },
    };

    // Pop scope
    self.env.pop();

    // Commit or rollback based on result
    // NOTE: Do NOT use `expect` in actual implementation
    let txn = self.txn.take().expect("transaction should exist");
    match body_result {
        Ok(val) => {
            txn.commit().await
                .map_err(|e| Error::runtime(span, format!("commit failed: {e}")))?;
            Ok(val)
        }
        Err(e) => {
            // Rollback on error; ignore rollback errors
            let _ = txn.rollback().await;
            Err(e)
        }
    }
}
```

##### 6.1.8.3 Helper Methods

Add helper methods for constructing `Result` values:

```rust
/// Make a `Result.Ok(val)` value.
fn make_result_ok(&mut self, type_id: TypeExprId, val: Value) -> Value {
    let val_id = self.arena.add(val, Span::default());
    Value::Tagged(type_id, 0, smallvec![val_id])  // Ok is variant 0
}

/// Make a `Result.Err(msg)` value.
fn make_result_err(&mut self, type_id: TypeExprId, msg_id: StringId) -> Value {
    let msg_val = Value::String(msg_id);
    let msg_val_id = self.arena.add(msg_val, Span::default());
    Value::Tagged(type_id, 1, smallvec![msg_val_id])  // Err is variant 1
}
```

### 6.1.9 Tests

Create `crates/rumps-query/scripts/110_transaction_basic.rumps`:

```rumps
; Test: Basic TRANSACTION block
; Tests transaction execution and Result return type

; === Transaction with no trailing expression ===
LET r1 = TRANSACTION {
    $SET ^data(1) = "one"
    $SET ^data(2) = "two"
}
$OUTPUT r1 IS Result.Ok(_)
; Expected: true

; === Transaction with trailing expression ===
LET r2 = TRANSACTION {
    $SET ^data(3) = "three"
    $GET ^data(3)
}
$OUTPUT r2 IS Result.Ok(_)
; Expected: true

; Unwrap and check value
MATCH r2 {
    Result.Ok(v) => { $OUTPUT v }
    Result.Err(_) => { $OUTPUT "error" }
}
; Expected: Option.Some("three")

; === Read committed data ===
; After commit, data should be visible
LET v1 = $GET ^data(1)
$OUTPUT v1
; Expected: Option.Some("one")

; === Transaction scope ===
; LET bindings inside transaction should not leak
LET r3 = TRANSACTION {
    LET inner = "inside"
    $SET ^data(4) = inner
}
; `inner` should not be accessible here

; === Multiple transactions ===
LET r4 = TRANSACTION {
    $SET ^data(5) = "five"
}
LET r5 = TRANSACTION {
    $SET ^data(6) = "six"
}
$OUTPUT (r4 IS Result.Ok(_)) AND (r5 IS Result.Ok(_))
; Expected: true

$OUTPUT "Complete!"
```

### 6.1.10 Implementation Checklist

- [ ] **Lexer**: Add `Token::Transaction` keyword
- [ ] **Token Display**: Add display for `TRANSACTION`
- [ ] **CST**: Add `TransactionExpr`, `TransactionModifiers`, modifier enums
- [ ] **CST**: Add `ExprKind::Transaction` variant
- [ ] **Parser**: Implement `transaction_expr` parser
- [ ] **Parser**: Add to `expr` alternatives
- [ ] **AST**: Add `TransactionExpr`, `TransactionModifiers` types
- [ ] **AST**: Re-export storage types (`ConflictStrategy`, etc.)
- [ ] **AST**: Add `Expr::Transaction` variant
- [ ] **Lowering**: Convert `cst::TransactionExpr` to `ast::TransactionExpr`
- [ ] **Lowering**: Convert modifier enums
- [ ] **Typechecker**: Infer `Result[T, String]` for transaction expressions
- [ ] **Typechecker**: Enter/exit scope for transaction body
- [ ] **Typechecker**: Validate timeout modifier is `Int`
- [ ] **Interpreter**: Implement `transaction` method
- [ ] **Interpreter**: Implement `execute_transaction` helper
- [ ] **Interpreter**: Add `make_result_ok`, `make_result_err` helpers
- [ ] **Interpreter**: Check for nested transaction (error)
- [ ] **Tests**: Integration test script (`110_transaction_basic.rumps`)

---

## 6.2: Transaction Modifiers

Extend transaction blocks with configuration modifiers using contextual parsing (same pattern as `$OUTPUT`).

### 6.2.1 Syntax

```rumps
; Conflict resolution
TRANSACTION {
    $SET ^DATA(k) = v
} ON CONFLICT ABORT           ; Default; fail on conflict

TRANSACTION {
    $SET ^DATA(k) = v
} ON CONFLICT RETRY 3         ; Retry up to 3 times

TRANSACTION {
    $SET ^DATA(k) = v
} ON CONFLICT SKIP            ; Skip transaction on conflict

TRANSACTION {
    $SET ^DATA(k) = v
} ON CONFLICT OVERWRITE       ; Last-write-wins

; Timeout
TRANSACTION {
    $SET ^DATA(k) = v
} WITH TIMEOUT 5000           ; 5 second timeout (milliseconds)

; Priority
TRANSACTION {
    $SET ^DATA(k) = v
} WITH PRIORITY HIGH          ; High priority
TRANSACTION {
    $SET ^DATA(k) = v
} WITH PRIORITY LOW           ; Low priority

; Isolation (only SNAPSHOT currently supported)
TRANSACTION {
    $SET ^DATA(k) = v
} WITH ISOLATION SNAPSHOT     ; Snapshot isolation (default)

; Combined modifiers
TRANSACTION {
    $SET ^DATA(k) = v
} ON CONFLICT RETRY 3 WITH TIMEOUT 5000 WITH PRIORITY HIGH
```

**Grammar**:
```
txn_expr   := TRANSACTION block [modifiers]
modifiers  := modifier*
modifier   := conflict_mod | timeout_mod | priority_mod | isolation_mod
conflict_mod   := ON CONFLICT (ABORT | RETRY int | SKIP | OVERWRITE)
timeout_mod    := WITH TIMEOUT expr
priority_mod   := WITH PRIORITY (LOW | NORMAL | HIGH)
isolation_mod  := WITH ISOLATION SNAPSHOT
```

### 6.2.2 Contextual Identifiers

Like `$OUTPUT`, modifier keywords are NOT global keywords:

| Contextual Identifier                 | Context                                               |
|---------------------------------------|-------------------------------------------------------|
| `ON`                                  | After `TRANSACTION { ... }`                           |
| `CONFLICT`                            | After `ON`                                            |
| `ABORT`, `RETRY`, `SKIP`, `OVERWRITE` | After `ON CONFLICT`                                   |
| `WITH`                                | After `TRANSACTION { ... }` or after another modifier |
| `TIMEOUT`, `PRIORITY`, `ISOLATION`    | After `WITH`                                          |
| `LOW`, `NORMAL`, `HIGH`               | After `WITH PRIORITY`                                 |
| `SNAPSHOT`                            | After `WITH ISOLATION`                                |

**Case-insensitivity**: Like RUMPS keywords, contextual identifiers must _also_ be matched case-insensitively. The following are all equivalent:

```rumps
TRANSACTION { ... } ON CONFLICT RETRY 3
TRANSACTION { ... } on conflict retry 3
TRANSACTION { ... } On Conflict Retry 3
```

These remain usable as variable names:

```rumps
LET on = 1         ; OK
LET conflict = 2   ; OK
LET timeout = 3    ; OK
LET with = 4       ; OK
```

Note this is exactly how `$OUTPUT` modifiers work.

### 6.2.3 Parser Implementation

```rust
/// Parse transaction modifiers (contextual identifiers).
fn transaction_modifiers(
    expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone + 'static,
) -> impl chumsky::Parser<Token, cst::TransactionModifiers, Error = ParseErr> + Clone {
    // ON CONFLICT (ABORT | RETRY n | SKIP | OVERWRITE)
    let conflict = Self::ctx_ident("ON")
        .ignore_then(Self::ctx_ident("CONFLICT"))
        .ignore_then(choice((
            Self::ctx_ident("ABORT").to(cst::ConflictModifier::Abort),
            Self::ctx_ident("RETRY")
                .ignore_then(select! { Token::Int(n) => n as u32 })
                .map(cst::ConflictModifier::Retry),
            Self::ctx_ident("SKIP").to(cst::ConflictModifier::Skip),
            Self::ctx_ident("OVERWRITE").to(cst::ConflictModifier::Overwrite),
        )));

    // WITH TIMEOUT expr
    let timeout = Self::ctx_ident("WITH")
        .ignore_then(Self::ctx_ident("TIMEOUT"))
        .ignore_then(expr.clone())
        .map(Box::new);

    // WITH PRIORITY (LOW | NORMAL | HIGH)
    let priority = Self::ctx_ident("WITH")
        .ignore_then(Self::ctx_ident("PRIORITY"))
        .ignore_then(choice((
            Self::ctx_ident("LOW").to(cst::PriorityModifier::Low),
            Self::ctx_ident("NORMAL").to(cst::PriorityModifier::Normal),
            Self::ctx_ident("HIGH").to(cst::PriorityModifier::High),
        )));

    // WITH ISOLATION SNAPSHOT
    let isolation = Self::ctx_ident("WITH")
        .ignore_then(Self::ctx_ident("ISOLATION"))
        .ignore_then(Self::ctx_ident("SNAPSHOT").to(cst::IsolationModifier::Snapshot));

    // Collect all modifiers
    conflict
        .or_not()
        .then(timeout.or_not())
        .then(priority.or_not())
        .then(isolation.or_not())
        .map(|(((conflict, timeout), priority), isolation)| {
            cst::TransactionModifiers { conflict, timeout, priority, isolation }
        })
}

/// Updated transaction_expr with modifiers.
fn transaction_expr(
    stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone + 'static,
    expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone + 'static,
) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
    just(Token::Transaction)
        .ignore_then(Self::block_body(stmt))
        .then(Self::transaction_modifiers(expr))
        .map_with_span(|((stmts, inner_expr), modifiers), span| {
            cst::Expr::new(
                cst::ExprKind::Transaction(Box::new(cst::TransactionExpr {
                    stmts,
                    expr: inner_expr.map(Box::new),
                    modifiers,
                })),
                span,
            )
        })
}
```

### 6.2.4 Tests

Create `crates/rumps-query/scripts/111_transaction_modifiers.rumps`:

```rumps
; Test: Transaction modifiers
; Tests ON CONFLICT, WITH TIMEOUT, WITH PRIORITY

; === ON CONFLICT ABORT (default) ===
LET r1 = TRANSACTION {
    $SET ^data(1) = "one"
} ON CONFLICT ABORT
$OUTPUT r1 IS Result.Ok(_)
; Expected: true

; === ON CONFLICT SKIP ===
LET r2 = TRANSACTION {
    $SET ^data(2) = "two"
} ON CONFLICT SKIP
$OUTPUT r2 IS Result.Ok(_)
; Expected: true

; === ON CONFLICT OVERWRITE ===
LET r3 = TRANSACTION {
    $SET ^data(3) = "three"
} ON CONFLICT OVERWRITE
$OUTPUT r3 IS Result.Ok(_)
; Expected: true

; === ON CONFLICT RETRY n ===
LET r4 = TRANSACTION {
    $SET ^data(4) = "four"
} ON CONFLICT RETRY 3
$OUTPUT r4 IS Result.Ok(_)
; Expected: true

; === WITH TIMEOUT ===
LET r5 = TRANSACTION {
    $SET ^data(5) = "five"
} WITH TIMEOUT 5000
$OUTPUT r5 IS Result.Ok(_)
; Expected: true

; === WITH PRIORITY ===
LET r6 = TRANSACTION {
    $SET ^data(6) = "six"
} WITH PRIORITY HIGH
$OUTPUT r6 IS Result.Ok(_)
; Expected: true

LET r7 = TRANSACTION {
    $SET ^data(7) = "seven"
} WITH PRIORITY LOW
$OUTPUT r7 IS Result.Ok(_)
; Expected: true

; === WITH ISOLATION SNAPSHOT ===
LET r8 = TRANSACTION {
    $SET ^data(8) = "eight"
} WITH ISOLATION SNAPSHOT
$OUTPUT r8 IS Result.Ok(_)
; Expected: true

; === Combined modifiers ===
LET r9 = TRANSACTION {
    $SET ^data(9) = "nine"
} ON CONFLICT RETRY 3 WITH TIMEOUT 5000 WITH PRIORITY HIGH
$OUTPUT r9 IS Result.Ok(_)
; Expected: true

; === Contextual identifiers as variables ===
LET on = "on"
LET conflict = "conflict"
LET timeout = 1000
LET with = "with"
LET priority = "priority"
LET isolation = "isolation"

$OUTPUT on ++ " " ++ conflict
; Expected: on conflict

$OUTPUT "Complete!"
```

### 6.2.5 Implementation Checklist

- [ ] **Parser**: Implement `transaction_modifiers` combinator
- [ ] **Parser**: Use contextual identifier matching (`ctx_ident`)
- [ ] **Parser**: Support `ON CONFLICT` variants
- [ ] **Parser**: Support `WITH TIMEOUT expr`
- [ ] **Parser**: Support `WITH PRIORITY` variants
- [ ] **Parser**: Support `WITH ISOLATION SNAPSHOT`
- [ ] **Interpreter**: Apply modifiers to `TransactionBuilder`
- [ ] **Tests**: Integration test script (`111_transaction_modifiers.rumps`)

---

## 6.3: Global Writes Require Transaction

Enforce that writes to globals (`$SET ^NAME(...)`) require an active transaction.

### 6.3.1 Current Behavior

The interpreter already checks for a transaction on global writes (see `interpreter/db.rs`):

```rust
if name.is_global() {
    match self.txn.as_ref() {
        Some(txn) => { /* write */ }
        None => Err(Error::runtime(span, "global SET requires a transaction")),
    }
}
```

### 6.3.2 Type Checker Enhancement

Add a warning or error during type checking when `$SET ^NAME(...)` is used outside a transaction context.

This requires tracking whether we're inside a `TRANSACTION` block during type inference. Add a flag to the inference context:

```rust
struct InferContext {
    // ... existing fields ...
    in_transaction: bool,
}
```

Set `in_transaction = true` when entering a transaction expression, and warn/error on global `$SET`/`$KILL` when `in_transaction = false`.

### 6.3.3 Tests

Create `crates/rumps-query/scripts/112_transaction_required.rumps`:

```rumps
; Test: Global writes require transaction

; === Local writes work outside transaction ===
$SET local-data(1) = "local"
$OUTPUT $GET local-data(1)
; Expected: Option.Some("local")

; === Global writes inside transaction succeed ===
LET r1 = TRANSACTION {
    $SET ^GLOBAL(1) = "global"
    $GET ^GLOBAL(1)
}
MATCH r1 {
    Result.Ok(v) => { $OUTPUT v }
    Result.Err(e) => { $OUTPUT "Error: " ++ e }
}
; Expected: Option.Some("global")

; === $KILL inside transaction ===
LET r2 = TRANSACTION {
    $SET ^TEMP(1) = "temp"
    $KILL ^TEMP(1)
    $GET ^TEMP(1)
}
MATCH r2 {
    Result.Ok(v) => { $OUTPUT v }
    Result.Err(_) => { $OUTPUT "error" }
}
; Expected: Option.None

$OUTPUT "Complete!"
```

**Note**: Testing global writes *outside* a transaction would produce a runtime error. Since errors terminate the script, this should be tested in a separate script that expects failure, or using the error handling pattern.

### 6.3.4 Implementation Checklist

- [ ] **Typechecker**: Add `in_transaction` flag to context
- [ ] **Typechecker**: Set flag on entering `Transaction` expression
- [ ] **Typechecker**: Warn on global `$SET`/`$KILL` outside transaction
- [ ] **Tests**: Integration test script (`112_transaction_required.rumps`)

---

## 6.4: Error Handling Patterns

Document recommended patterns for handling transaction failures.

### 6.4.1 Pattern: Match on Result

```rumps
LET result = TRANSACTION {
    $SET ^DATA(key) = value
    $GET ^DATA(key)
}

MATCH result {
    Result.Ok(v) => {
        $OUTPUT "Success: " ++ (v AS String)
    }
    Result.Err(e) => {
        $OUTPUT "Failed: " ++ e TO ERROR
    }
}
```

### 6.4.2 Pattern: Unwrap with Default

```rumps
LET val = TRANSACTION {
    $SET ^COUNTER(1) = ($GET ^COUNTER(1) ?? 0) + 1
    $GET ^COUNTER(1)
} ?? Option.None
; On transaction failure, val = Option.None
```

### 6.4.3 Pattern: Retry Wrapper

```rumps
; Define a retry helper using FOREVER
FUN with-retry[T] (action: () -> Result[T, String], max-attempts: Int): Result[T, String] {
    FOREVER { attempt: 1, last-err: "" } (st, cont) => {
        IF st.attempt > max-attempts {
            Result.Err("Max attempts exceeded: " ++ st.last-err)
        } ELSE {
            MATCH action() {
                Result.Ok(v) => Result.Ok(v)
                Result.Err(e) => cont({ attempt: st.attempt + 1, last-err: e })
            }
        }
    }
}

; Use with a transaction
LET result = with-retry(() => TRANSACTION {
    $SET ^DATA(k) = v
    $GET ^DATA(k)
}, 3)
```

### 6.4.4 Tests

Create `crates/rumps-query/scripts/113_transaction_errors.rumps`:

```rumps
; Test: Transaction error handling patterns

; === Match on Result ===
LET r1 = TRANSACTION {
    $SET ^data(1) = "success"
}
LET msg1 = MATCH r1 {
    Result.Ok(_) => "ok"
    Result.Err(e) => "err: " ++ e
}
$OUTPUT msg1
; Expected: ok

; === Coalesce on failure ===
LET r2 = TRANSACTION {
    $SET ^data(2) = "val"
    $GET ^data(2)
}
LET val2 = r2 ?? Option.None
$OUTPUT val2
; Expected: Option.Some("val")

; === Check is Result.Ok ===
LET r3 = TRANSACTION {
    $SET ^data(3) = "test"
}
$OUTPUT r3 IS Result.Ok(_)
; Expected: true

$OUTPUT "Complete!"
```

### 6.4.5 Implementation Checklist

- [ ] **Documentation**: Add error handling patterns to language guide
- [ ] **Tests**: Integration test script (`113_transaction_errors.rumps`)

---

## Design Notes

### Why `Result[T, String]` Instead of Exceptions?

RUMPS follows a functional, explicit error handling philosophy:

1. **Explicitness**: Transaction failure is visible in the type
2. **Composability**: Results can be chained, matched, coalesced
3. **No hidden control flow**: No `try`/`catch` blocks
4. **Matches Rust**: Mirrors the Rust storage layer's `Result` type

### Why Contextual Parsing for Modifiers?

Adding `ON`, `CONFLICT`, `WITH`, `TIMEOUT`, etc., as global keywords would:

1. Break existing code using these as variable names
2. Pollute the keyword namespace
3. Make the language harder to learn

Contextual parsing (like SQL's `AS` or Python's `async`) keeps the keyword set minimal.

### Nested Transactions

Nested transactions are explicitly not supported because:

1. `rumps-storage` doesn't have savepoint support
2. MVCC semantics become complex with nested commits
3. Most use cases are better served by single larger transactions

If needed, savepoint support could be added to `Transaction` in a future phase.

### Future Extensions

The following could be added if `rumps-storage` is extended:

| Feature              | Requires                         |
|----------------------|----------------------------------|
| `SAVEPOINT name`     | `Transaction::savepoint(name)`   |
| `ROLLBACK TO name`   | `Transaction::rollback_to(name)` |
| `CATCH e => { ... }` | Custom error handler in builder  |
| `FINALLY { ... }`    | Always-run cleanup in builder    |
| `Txn.id`             | Expose transaction ID in context |

---

## Summary

| Phase | Feature                                         | Complexity    |
|-------|-------------------------------------------------|---------------|
| 6.1   | Basic `TRANSACTION { ... }` block               | Medium        |
| 6.2   | Modifiers (`ON CONFLICT`, `WITH TIMEOUT`, etc.) | Medium        |
| 6.3   | Enforce globals require transaction             | Low           |
| 6.4   | Error handling patterns                         | Documentation |

Total estimated test scripts: 4 (110, 111, 112, 113)
