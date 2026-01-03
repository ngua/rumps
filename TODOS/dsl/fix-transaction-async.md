# Fix Transaction Async: Per-Transaction Context via TxnId

## Problem

Currently, the interpreter holds a single `txn: Option<Transaction>` field. When
evaluating a `TRANSACTION { ... }` block, it sets `self.txn = Some(txn)`, evaluates
the body, then clears it. All DB intrinsics check `self.txn`.

This forces sequential transaction execution. Even though the underlying Rust API
supports concurrent transactions via `db.transaction(|tx| async move { ... }).await?`,
we cannot express this in the query language. Each `TRANSACTION` block should yield
like a normal async expression, but currently they block on a single shared context.

**Goal**: Each `TRANSACTION` block should have its own isolated context, enabling
proper async semantics where transactions can run concurrently.

## Solution: Transaction ID System

Assign a unique `TxnId` to each `TRANSACTION` block during typechecking. Store
this ID in the AST and use a `HashMap<TxnId, Transaction>` at runtime instead of
a single `Option<Transaction>`.

---

## Phase 1: Define `TxnId` and Update AST

### 1.1 Add `TxnId` type

**File**: `crates/rumps-query/src/ast.rs`

```rust
/// Unique identifier for a `TRANSACTION` block.
///
/// Assigned during typechecking; used at runtime to look up the active
/// transaction in a `HashMap<TxnId, Transaction>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TxnId(u32);

impl TxnId {
    pub(crate) const fn new(id: u32) -> Self {
        Self(id)
    }
}
```

### 1.2 Add `TxnId` to `TransactionExpr`

```rust
pub(crate) struct TransactionExpr {
    /// Unique ID for this transaction block (assigned during typecheck).
    pub(crate) id: Option<TxnId>,
    pub(crate) stmts: Vec<StmtId>,
    pub(crate) expr: Option<ExprId>,
    pub(crate) modifiers: TransactionModifiers,
}
```

Parser emits `id: None`; typecheck fills it in.

### 1.3 Add `Option<TxnId>` to DB-related AST nodes

All DB operations need to know their enclosing transaction (or `None` if outside).

**Expressions**:

```rust
pub(crate) enum Expr {
    // Reads; use transaction if present, else direct DB access
    Get(DbRef, Option<TxnId>),
    Data(DbRef, Option<TxnId>),
    Order(DbRef, Option<TxnId>),
    Query(DbRef, Option<TxnId>),

    // Writes; `TxnId` required for globals (enforced by typecheck)
    Set(DbRef, ExprId, Option<TxnId>),
    Kill(DbRef, Option<TxnId>),

    // ... other variants unchanged ...
}
```

**Statements**:

```rust
pub(crate) enum Stmt {
    Set(DbRef, ExprId, Option<TxnId>),
    Kill(DbRef, Option<TxnId>),
    // ... other variants unchanged ...
}
```

### 1.4 Add `set_stmt` method to `Ast`

Currently only `set_expr` exists. Add:

```rust
impl Ast {
    /// Replace a statement in place.
    pub(crate) fn set_stmt(&mut self, id: StmtId, s: Stmt) {
        if let Some(slot) = self.stmts.get_mut(id.0) {
            *slot = s
        }
    }
}
```

### 1.5 Update parser/lowering to emit `None` for new fields

**File**: `crates/rumps-query/src/parser/lower.rs`

All DB expressions and statements emit `None` for `TxnId`:

```rust
cst::ExprKind::Get(dbref) => {
    let dbref = lower_db_ref(ast, dbref)?;
    Expr::Get(dbref, None)
}

cst::ExprKind::Set(dbref, val) => {
    let dbref = lower_db_ref(ast, dbref)?;
    let val_id = lower_expr(ast, val)?;
    Expr::Set(dbref, val_id, None)
}

// ... similar for Kill, Data, Order, Query ...
```

```rust
cst::StmtKind::Set(dbref, val) => {
    let dbref = lower_db_ref(ast, dbref)?;
    let val_id = lower_expr(ast, val)?;
    Stmt::Set(dbref, val_id, None)
}

cst::StmtKind::Kill(dbref) => {
    let dbref = lower_db_ref(ast, dbref)?;
    Stmt::Kill(dbref, None)
}
```

---

## Phase 2: Update Typecheck to Assign IDs

### 2.1 Change `InferCtx` to track current transaction

**File**: `crates/rumps-query/src/typecheck/infer.rs`

```rust
pub(crate) struct InferCtx<'a> {
    /// Mutable AST for populating `TxnId` fields.
    pub(super) ast: &'a mut Ast,

    /// Current transaction ID, if inside a `TRANSACTION` block.
    /// Was: `in_transaction: bool`
    pub(super) in_transaction: Option<TxnId>,

    /// Counter for generating unique `TxnId` values.
    next_txn_id: u32,

    // ... other fields unchanged ...
}
```

### 2.2 Change `check` signature to take `&mut Ast`

**File**: `crates/rumps-query/src/typecheck.rs`

```rust
pub(crate) fn check(
    ast: &mut Ast,  // Was: &Ast
    stmts: &[StmtId],
    // ... rest unchanged ...
) -> crate::Result<(Vec<regex::Regex>, HashMap<ExprId, u32>)>
```

### 2.3 Assign `TxnId` when entering a transaction

**File**: `crates/rumps-query/src/typecheck/infer/expr.rs`

The call site in `expr_inner` already has access to the `TransactionExpr`; we just need
to also pass the `ExprId` so we can update the AST:

```rust
// In expr_inner match:
Expr::Transaction(txn) => self.transaction(id, txn, span),
```

```rust
/// Typecheck a transaction block; assigns a unique `TxnId`.
///
/// The `txn` is cloned and updated with the assigned ID, then written
/// back to the AST via `set_expr`.
pub(super) fn transaction(
    &mut self,
    id: ExprId,
    txn: &TransactionExpr,
    span: Span,
) -> Ty {
    // Reject nested transactions
    if self.in_transaction.is_some() {
        self.error(TypeError::Custom {
            msg: "nested transactions are not supported".to_string(),
            span,
        });
        // Continue with a fresh ID anyway to allow further inference
    }

    // Assign unique ID
    let txn_id = TxnId::new(self.next_txn_id);
    self.next_txn_id += 1;

    // Set transaction context
    let prev = self.in_transaction.replace(txn_id);
    self.env.push_scope();

    // Typecheck body
    txn.stmts.iter().for_each(|&stmt_id| self.stmt(stmt_id));
    let inner_ty = txn.expr.map_or(Ty::Unit, |expr_id| self.expr(expr_id));

    // Typecheck timeout modifier
    txn.modifiers.timeout.iter().for_each(|&timeout_id| {
        let timeout_ty = self.expr(timeout_id);
        self.unify(timeout_ty, Ty::Int, span);
    });

    self.env.pop_scope();
    self.in_transaction = prev;

    // Update AST with assigned ID
    let updated = TransactionExpr {
        id: Some(txn_id),
        stmts: txn.stmts.clone(),
        expr: txn.expr,
        modifiers: txn.modifiers,
    };
    self.ast.set_expr(id, Expr::Transaction(updated));

    Ty::Result(Box::new(inner_ty), Box::new(Ty::String))
}
```

### 2.4 Populate `TxnId` on DB operations

**File**: `crates/rumps-query/src/typecheck/infer/stmt.rs`

**Important**: `Expr::Set`/`Expr::Kill` and `Stmt::Set`/`Stmt::Kill` currently share the
same `set()` and `kill()` validation methods. Since they need different AST mutations
(`set_expr` vs `set_stmt`), we keep the validation logic shared and do mutations at call sites.

**File**: `crates/rumps-query/src/typecheck/infer/stmt.rs`

The `set()` and `kill()` methods keep their current signatures (no `StmtId`/`ExprId`);
they just do validation. AST mutation happens at the call site:

```rust
// In stmt() match:
Some(Stmt::Set(ref dbref, value, _)) => {
    self.set(dbref, value, span);
    self.ast.set_stmt(id, Stmt::Set(dbref.clone(), value, self.in_transaction));
}

Some(Stmt::Kill(ref dbref, _)) => {
    self.kill(dbref, span);
    self.ast.set_stmt(id, Stmt::Kill(dbref.clone(), self.in_transaction));
}
```

**File**: `crates/rumps-query/src/typecheck/infer/expr.rs`

Similarly for expression forms; `@SET` and `@KILL` in expression contexts:

```rust
// In expr_inner match:
Expr::Set(ref dbref, value, _) => {
    self.set(dbref, value, span);
    self.ast.set_expr(id, Expr::Set(dbref.clone(), value, self.in_transaction));
    Ty::Result(Box::new(Ty::Unit), Box::new(Ty::String))
}

Expr::Kill(ref dbref, _) => {
    self.kill(dbref, span);
    self.ast.set_expr(id, Expr::Kill(dbref.clone(), self.in_transaction));
    Ty::Result(Box::new(Ty::Unit), Box::new(Ty::String))
}
```

For read-only DB expressions (`GET`, `DATA`, `ORDER`, `QUERY`), same pattern:

```rust
Expr::Get(ref dbref, _) => {
    let subs = match dbref {
        DbRef::Local(_, s) | DbRef::Global(_, s) => s,
    };
    self.check_subscript_elems(subs, span);
    self.ast.set_expr(id, Expr::Get(dbref.clone(), self.in_transaction));
    self.option_storable()
}

// Similar for Data, Order, Query
```

---

## Phase 3: Update Interpreter Runtime

### 3.1 Replace `txn: Option<Transaction>` with `HashMap`

**File**: `crates/rumps-query/src/interpreter.rs`

```rust
use std::collections::HashMap;
use crate::ast::TxnId;

pub(crate) struct Interpreter<'a, I: IoContext> {
    // Was: txn: Option<Transaction>
    txns: HashMap<TxnId, Transaction>,

    // ... other fields unchanged ...
}

impl<'a, I: IoContext> Interpreter<'a, I> {
    pub(crate) fn new(/* ... */) -> Self {
        Self {
            txns: HashMap::new(),
            // ...
        }
    }
}
```

### 3.2 Update transaction evaluation

**File**: `crates/rumps-query/src/interpreter/transaction.rs`

```rust
pub(super) async fn transaction(
    &mut self,
    txn_expr: &TransactionExpr,
    span: Span,
) -> Result<Value> {
    let id = txn_expr.id.unwrap_or_else(|| {
        typechecked!("transaction", "TxnId assigned")
    });

    let mut builder = self.db.build_transaction();
    // ... apply modifiers ...

    let result = self
        .execute_txn_body(id, builder, &txn_expr.stmts, txn_expr.expr, timeout_ms, span)
        .await;

    match result {
        Ok(val) => Ok(self.make_result_ok(val, span)),
        Err(e) => Ok(self.make_result_err(&e.to_string(), span)),
    }
}

async fn execute_txn_body(
    &mut self,
    id: TxnId,
    builder: TransactionBuilder,
    stmts: &[StmtId],
    expr: Option<ExprId>,
    timeout_ms: Option<u64>,
    span: Span,
) -> Result<Value> {
    let txn = builder
        .start()
        .await
        .map_err(|e| Error::runtime(span, format!("transaction start: {e}")))?;

    // Insert into map
    self.txns.insert(id, txn.clone());

    self.env.scopes.push();
    let body_result = match timeout_ms {
        Some(ms) => txn.timed(ms, self.execute_txn_stmts(stmts, expr)).await,
        None => self.execute_txn_stmts(stmts, expr).await,
    };
    self.env.scopes.pop();

    // Remove from map
    self.txns.remove(&id);

    match body_result {
        Ok(val) => {
            txn.commit()
                .await
                .map_err(|e| Error::runtime(span, format!("commit: {e}")))?;
            Ok(val)
        }
        Err(e) => {
            txn.rollback()
                .await
                .map_err(|e2| Error::runtime(span, format!("rollback: {e2}")))?;
            Err(e)
        }
    }
}
```

### 3.3 Update DB intrinsics to use `TxnId`

**File**: `crates/rumps-query/src/interpreter/db.rs`

```rust
pub(super) async fn get(
    &mut self,
    dbref: &DbRef,
    txn_id: Option<TxnId>,
    span: Span,
) -> Result<Value> {
    let (name, subs) = dbref.split();
    let key = self.build_key(subs).await?;

    let opt_val = match txn_id.and_then(|id| self.txns.get(&id)) {
        Some(txn) => txn.get(&name, &key).await,
        None => self.db.get(&name, &key).await,
    }
    .map_err(|e| Error::runtime(span, format!("GET failed: {e}")))?;

    // ... rest unchanged ...
}

pub(super) async fn set(
    &mut self,
    dbref: &DbRef,
    expr_id: ExprId,
    txn_id: Option<TxnId>,
    span: Span,
) -> Result<Value> {
    let (name, subs) = dbref.split();
    let key = self.build_key(subs).await?;
    let val = self.eval(expr_id).await?;
    let storage_val = self.store(&val)?;

    let res = if name.is_global() {
        let txn = txn_id
            .and_then(|id| self.txns.get(&id))
            .unwrap_or_else(|| typechecked!("global SET", "transaction context"));
        txn.set(&name, &key, storage_val)
            .await
            .map_err(|e| e.to_string())
    } else {
        self.db
            .set(&name, &key, storage_val)
            .await
            .map_err(|e| e.to_string())
    };

    // ... rest unchanged ...
}

// Similar updates for kill, data, order, query
```

### 3.4 Update `eval` to pass `TxnId`

**File**: `crates/rumps-query/src/interpreter.rs`

```rust
pub(crate) async fn eval(&mut self, id: ExprId) -> Result<Value> {
    let span = self.ast.expr_span(id).unwrap_or_default();
    let expr = self.ast.get_expr(id).ok_or_else(/* ... */)?;

    match expr.clone() {
        Expr::Get(ref dbref, txn_id) => self.get(dbref, txn_id, span).await,
        Expr::Set(ref dbref, val, txn_id) => self.set(dbref, val, txn_id, span).await,
        Expr::Kill(ref dbref, txn_id) => self.kill(dbref, txn_id, span).await,
        Expr::Data(ref dbref, txn_id) => self.data(dbref, txn_id, span).await,
        Expr::Order(ref dbref, txn_id) => self.order(dbref, txn_id, span).await,
        Expr::Query(ref dbref, txn_id) => self.query(dbref, txn_id, span).await,
        // ... other cases unchanged ...
    }
}
```

### 3.5 Update statement execution similarly

Pass `TxnId` from `Stmt::Set` and `Stmt::Kill` to the DB methods.

---

## Checklist

### Phase 1: AST Changes
- [ ] Add `TxnId` struct to `ast.rs`
- [ ] Add `id: Option<TxnId>` to `TransactionExpr`
- [ ] Add `Option<TxnId>` field to `Expr::Get`, `Expr::Set`, `Expr::Kill`, `Expr::Data`, `Expr::Order`, `Expr::Query`
- [ ] Add `Option<TxnId>` field to `Stmt::Set`, `Stmt::Kill`
- [ ] Add `set_stmt` method to `Ast`
- [ ] Update parser/lower to emit `None` for new fields
- [ ] Update all pattern matches on affected variants (will cause compile errors that guide you)

### Phase 2: Typecheck Changes
- [ ] Change `InferCtx.ast` from `&'a Ast` to `&'a mut Ast`
- [ ] Change `in_transaction: bool` to `in_transaction: Option<TxnId>`
- [ ] Add `next_txn_id: u32` counter to `InferCtx`
- [ ] Update `check()` signature to take `&mut Ast`
- [ ] Update `InferCtx::new()` to take `&'a mut Ast`
- [ ] Update `expr_inner` to pass `id` to `transaction(id, txn, span)`
- [ ] Update `transaction()` to assign ID and mutate AST
- [ ] Update `stmt()` match for `Stmt::Set`/`Stmt::Kill`: call validation, then `set_stmt()`
- [ ] Update `expr_inner` match for `Expr::Set`/`Expr::Kill`: call validation, then `set_expr()`
- [ ] Update `expr_inner` match for `Expr::Get`/`Data`/`Order`/`Query`: inline subscript check, then `set_expr()`
- [ ] Keep `set()`/`kill()` methods unchanged (just validation, no AST mutation)

### Phase 3: Interpreter Changes
- [ ] Replace `txn: Option<Transaction>` with `txns: HashMap<TxnId, Transaction>`
- [ ] Update `transaction()` to insert/remove from map
- [ ] Update `get()` to take and use `Option<TxnId>`
- [ ] Update `set()` to take and use `Option<TxnId>`
- [ ] Update `kill()` to take and use `Option<TxnId>`
- [ ] Update `data()` to take and use `Option<TxnId>`
- [ ] Update `order()` to take and use `Option<TxnId>`
- [ ] Update `query()` to take and use `Option<TxnId>`
- [ ] Update `eval()` match arms to pass `TxnId`
- [ ] Update statement execution to pass `TxnId`

### Testing
- [ ] Existing transaction tests still pass
- [ ] Add test: nested transaction error still works
- [ ] Add test: global write outside transaction error still works
- [ ] Add test: reads inside transaction use transaction context
- [ ] Add test: reads outside transaction use direct DB

---

## Notes

1. **No parallel execution yet**: This change enables proper async semantics but does
   not implement parallel transaction execution. That would require additional work
   (e.g., a parallel tuple evaluation mode).

2. **AST mutation in typecheck**: The typecheck phase will now mutate the AST to
   populate `TxnId` fields. This follows the existing pattern in `resolve.rs`.

3. **Backwards compatibility**: The AST changes are internal; no user-facing syntax
   changes.
