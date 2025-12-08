# COLLECT and Writes: Design Analysis

## Problem Statement

Looking at `TODOS/dsl.md`, the DSL design shows `COLLECT` streams that include writes:

```rumps
; INTO pattern - write collected results to a global
COLLECT ^DATA
  SELECT value
  INTO ^PROCESSED  ; Global variable (requires transaction)

; FOREACH with nested transaction
COLLECT ^ORDER(id,"ITEMS")
  FOREACH item => {
    TRANSACTION {
      SET ^INVENTORY(item.id,"COUNT") = ^INVENTORY(item.id,"COUNT") - item.qty
    } ON CONFLICT ROLLBACK TO process-items
  }
```

However, the Rust implementations (`Database::collects` and `Transaction::collects`) are read-only operations. The callbacks receive `(&Key, &Option<Value>)` and can only extract data, not modify it.

**Question**: Do we need to pass a transaction context into the `COLLECT` callbacks to allow writes?

## Analysis: The Current Design Is Correct

Looking more carefully at the DSL syntax, **`INTO` and `FOREACH` are terminal operations** that *consume* the stream—they are not transformations within it.

```
COLLECT ^RAW-DATA           ┐
  WHERE key[0] >= last-id   │ Read-only stream
  MAP transform-record      │ production
  SELECT {...}              ┘
  INTO ^PROCESSED-DATA      ← Terminal write operation (consumes stream)
```

The `COLLECT...WHERE...SELECT` chain produces a read-only stream. `INTO` and `FOREACH` are sinks that consume that stream and perform side effects.

**The `collects()` methods don't need to support writes—they produce streams.** The DSL interpreter handles `INTO` and `FOREACH` as separate terminal operations that:

1. Consume the stream (via `StreamExt::try_collect()` or iteration)
2. Perform writes using `Transaction::set()`

### What the DSL Interpreter Would Do

The DSL interpreter maintains a transaction context. User-defined functions that call `SET ^GLOBAL(...)` are evaluated by the interpreter, which translates them to `txn.set(...)` calls.

```rust
// COLLECT ^DATA SELECT value INTO ^PROCESSED
//
// Interpreter has `txn: Transaction` (Clone is cheap - Arc fields)

let results: Vec<(Key, Value)> = txn
    .collects(&src_name, None, pred, extract)
    .await?
    .try_collect()
    .await?;

// INTO terminal operation
let txn_ref = &txn;
stream::iter(results)
    .map(Ok)
    .try_for_each(|(k, v)| async move {
        txn_ref.set(&dst_name, &k, v).await
    })
    .await?;
```

**Key point**: The `FOREACH` callback in the DSL *does* need transaction context. The interpreter provides this—when evaluating user code like `process-task` that contains `SET ^GLOBAL(...)`, the interpreter has the transaction in scope and uses it.

This works because the interpreter controls execution. User DSL code doesn't directly call Rust functions; the interpreter evaluates it and makes the appropriate `txn.set()` calls.

### Nested Transactions (Savepoints)

This is a separate concern. The DSL will need `SAVEPOINT` support, but that's orthogonal to `COLLECT`. The interpreter wraps the inner `FOREACH` body in a nested transaction scope.

## Alternatives Considered

### Alternative A: Pass Transaction to Callbacks (Rejected)

Modify `collects` to pass `&Transaction` to the `extract` callback:

```rust
txn.collects(
    &name,
    None,
    |k, v| ...,           // pred
    |k, v, txn| {         // extract with txn access
        txn.set(...)?;
        Some(result)
    },
).await?;
```

**Problems**:
- Callback would need to be async (for `txn.set()`)
- Signature becomes very complex
- Conflates reading and writing in one operation
- Breaks the functional streaming model

### Alternative B: Add Convenience Methods to Transaction

Add `for_each` and `collect_into` methods to `Transaction`:

```rust
impl Transaction {
    /// FOREACH pattern - execute async action for each matching entry.
    pub async fn for_each<P, A, Fut>(
        &self,
        name: &Name,
        start: Option<&Key>,
        pred: P,
        action: A,
    ) -> Result<()>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync + Clone,
        A: Fn(&Transaction, &Key, &Option<Value>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<()>> + Send,
    { ... }

    /// INTO pattern - collect and write to destination.
    pub async fn collect_into<P, F>(
        &self,
        src: &Name,
        dst: &Name,
        start: Option<&Key>,
        pred: P,
        transform: F,
    ) -> Result<usize>
    where
        P: Fn(&Key, &Option<Value>) -> bool + Send + Sync + Clone,
        F: Fn(&Key, &Option<Value>) -> Option<(Key, Value)> + Send + Sync,
    { ... }
}
```

**Usage**:

```rust
// FOREACH with writes
txn.for_each(
    &global!("TASKS"),
    None,
    |k, _| k.len() == 2,
    |txn, k, v| async move {
        txn.set(&global!("PROCESSED"), k, v.clone().unwrap()).await
    },
).await?;

// INTO pattern
txn.collect_into(
    &global!("RAW"),
    &global!("CLEAN"),
    None,
    |_, v| v.is_some(),
    |k, v| Some((k.clone(), transform(v))),
).await?;
```

**Assessment**: Nice for Rust ergonomics, but not strictly necessary. The DSL interpreter can compose existing primitives.

### Alternative C: Fluent Builder Pattern

```rust
txn.collect(&global!("DATA"))
    .after(&start_key)
    .filter(|k, v| k.len() == 2)
    .map(|k, v| (k.clone(), v.clone().unwrap()))
    .into(&global!("PROCESSED"))  // terminal - does writes
    .await?;
```

**Assessment**: Mirrors DSL syntax nicely but requires a `CollectBuilder<'a>` struct. More complexity than needed.

## Recommendation

**Don't change `collects`.** The current read-only design is clean and correct.

The DSL interpreter layer handles:
- `INTO ^GLOBAL` → consume stream, batch write via transaction
- `FOREACH fn` → consume stream, call closure (which can do writes if it has txn access)
- Savepoints → separate transaction management concern

If Rust-side ergonomics become important later, Alternative B (convenience methods) is the right approach—low risk, solves the problem, and can be added incrementally.

---

*Created: 2024-12-07*
