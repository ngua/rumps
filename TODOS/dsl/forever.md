# FOREVER Expression

## Overview

`FOREVER` is a functional looping construct using continuation-passing style. It provides a way to express loops without `break`, `return`, or mutable state.

**Key design principle**: The continuation parameter is a **pseudo-function** with special semantics; it's not a real function call, but a control flow signal.

---

## Syntax

```rumps
; Basic form
FOREVER seed (state, continue) => body

; With type annotations
FOREVER (seed: T) (state: T, continue: (T) -> R) -> R => body

; Examples
LET result = FOREVER 1 (x, f) => {
  IF x < 10 {
    f(x + 1)
  } ELSE {
    "done"
  }
}

; Infinite loop (outputs forever)
FOREVER Unit (_, f) => {
  @OUTPUT "tick"
  f(Unit)
}
```

---

## Semantics

1. **Seed**: The initial state value, evaluated once before the loop starts.

2. **State parameter**: Bound to the current state at each iteration.

3. **Continuation parameter**: A pseudo-function `f`. When "called" with a value:
   - The argument is evaluated (side effects happen)
   - The loop continues with the argument as the new state
   - **Importantly**: `f(...)` does not "return" in the normal sense

4. **Exit condition**: If the body evaluates to a value without calling the continuation, the loop terminates and that value is returned.

5. **Return type**: Inferred from the type of the non-continuation exit path. If no exit (infinite loop), the type is the seed type.

---

## Special Continuation Semantics

The continuation `f` is **not** a regular function. Key differences:

| Aspect | Regular Function | Continuation `f` |
|--------|------------------|------------------|
| Returns a value | Yes | No; causes iteration |
| Is a first-class value | Yes | No; only callable in FOREVER body |
| Argument evaluation | Normal | Normal, but controls loop state |
| Can be passed around | Yes | No |

### Why "pseudo-function"?

This allows natural syntax like:

```rumps
FOREVER Unit (x, f) => {
  f(@OUTPUT x)   ; OUTPUT executes, returns Unit, loop continues with Unit
}
```

The argument to `f` is evaluated for its side effects, and its value becomes the next state.

---

## Implementation Plan

### Phase 1: Make `@OUTPUT` Expressionable

Currently `OUTPUT` is only a `Stmt`. For `f(@OUTPUT x)` to work, we need `Expr::Output`.

#### 1.1 AST Changes (`ast.rs`)

Add `Expr::Output(OutputStmt)` variant:

```rust
/// Output expression: `@OUTPUT expr [JSON] [TO target]`.
///
/// Executes the output side effect and evaluates to `Unit`.
/// This allows `@OUTPUT` in expression contexts like `f(@OUTPUT x)`.
Output(OutputStmt),
```

#### 1.2 Parser Changes (`parser.rs`)

Allow `@OUTPUT` in expression position. Currently `output_stmt` creates `StmtKind::Output`. Add `output_expr` that creates `ExprKind::Output` and integrate into primary expression parsing.

Parsing strategy:
- In expression context: `@OUTPUT expr ...` parses as `Expr::Output`
- In statement context: `@OUTPUT expr ...` as before (can use the same underlying parser)

**Alternative**: Keep OUTPUT as statement-only, but allow blocks in continuation argument position:


```rumps
f({ 
  @OUTPUT x 
  Unit 
})
```

This is more verbose but avoids adding `Expr::Output`. **Recommendation**: Add `Expr::Output` for cleaner syntax.

**NOTE**: The same was done to `@KILL` and `@SET` as part of the implementation

#### 1.3 Typechecker Changes (`typecheck/infer/expr.rs`)

`Expr::Output` has type `Unit`. The inner expression must be `Stringable` (or `Jsonable` for JSON format), same as statement version.

#### 1.4 Interpreter Changes (`interpreter.rs`)

`Expr::Output` evaluates the inner expression, performs the output side effect, and returns `Value::Unit`.

---

### Phase 2: Lexer and Token Changes

#### 2.1 Add `FOREVER` Keyword (`token.rs`)

```rust
Token::Forever  // FOREVER keyword
```

Add to `keyword()` function for case-insensitive matching.

---

### Phase 3: AST for FOREVER

#### 3.1 Add AST Type (`ast.rs`)

```rust
/// A forever (loop) expression: `FOREVER seed (state, cont) => body`.
///
/// - `seed`: Initial state expression
/// - `state_param`: State parameter name with optional type
/// - `cont_param`: Continuation parameter name with optional type
/// - `body`: Loop body expression
///
/// Semantics:
/// - Evaluate seed to get initial state
/// - Bind state_param to current state
/// - Bind cont_param to a pseudo-function
/// - Evaluate body
/// - If body calls cont_param(new_state), loop with new state
/// - If body evaluates without calling cont_param, return body's value
Forever {
    seed: ExprId,
    state_param: (String, Option<AstTypeExprId>),
    cont_param: (String, Option<AstTypeExprId>),
    body: ExprId,
},
```

Note: We don't store a return type annotation; the continuation "function" type annotation implies it.

---

### Phase 4: Parser / CST

#### 4.1 CST Types (`parser/cst.rs`)

```rust
/// Forever expression components.
#[derive(Debug, Clone)]
pub(crate) struct ForeverExpr {
    pub(crate) seed: Box<Expr>,
    pub(crate) state_param: (String, Option<TypeExpr>),
    pub(crate) cont_param: (String, Option<TypeExpr>),
    pub(crate) body: Box<Expr>,
}
```

Add `ExprKind::Forever(ForeverExpr)` variant.

#### 4.2 Parser Implementation (`parser.rs`)

Grammar:

```
forever_expr := FOREVER seed_expr param_list '=>' body_expr

; Two forms of seed:
seed_expr := expr                     ; untyped: FOREVER 1
           | '(' expr ':' type ')'    ; typed: FOREVER (1: Int)

param_list := '(' param ',' param ')' ; (state, cont)

param := IDENT [':' type]             ; optional type annotation
```

Key parsing points:
- Lookahead for `(expr : type)` vs `(params)` is tricky
- Typed seed must be parenthesized: `FOREVER ([] : Array[Int]) (xs, f) => ...`
- Untyped seed is just an expression: `FOREVER 1 (x, f) => ...`

Parser implementation sketch:

```rust
fn forever_expr(
    expr: impl Parser<...>,
) -> impl Parser<...> {
    just(Token::Forever)
        .ignore_then(Self::opt_newlines())
        .ignore_then(expr.clone())  // seed
        .then_ignore(Self::opt_newlines())
        .then(Self::forever_params())  // (state, cont)
        .then_ignore(Self::opt_newlines())
        .then_ignore(just(Token::FatArrow))
        .then_ignore(Self::opt_newlines())
        .then(expr)  // body
        .map_with_span(|((seed, params), body), span| {
            cst::Expr::new(
                cst::ExprKind::Forever(cst::ForeverExpr {
                    seed: Box::new(seed),
                    state_param: params.0,
                    cont_param: params.1,
                    body: Box::new(body),
                }),
                span,
            )
        })
}

fn forever_params() -> impl Parser<...> {
    // (state [: Type], cont [: Type])
    just(Token::LParen)
        .ignore_then(Self::forever_param())
        .then_ignore(just(Token::Comma))
        .then_ignore(Self::opt_newlines())
        .then(Self::forever_param())
        .then_ignore(just(Token::RParen))
}

fn forever_param() -> impl Parser<...> {
    Self::ident()
        .then(
            just(Token::Colon)
                .ignore_then(Self::type_expr())
                .or_not()
        )
}
```

---

### Phase 5: Lowering (`parser/lower.rs`)

Convert `cst::ForeverExpr` to `ast::Expr::Forever`:

```rust
cst::ExprKind::Forever(forever) => {
    let seed = self.lower_expr(*forever.seed)?;
    let state_param = self.lower_param(forever.state_param)?;
    let cont_param = self.lower_param(forever.cont_param)?;
    let body = self.lower_expr(*forever.body)?;

    Expr::Forever {
        seed,
        state_param,
        cont_param,
        body,
    }
}
```

---

### Phase 6: Typechecker

#### 6.1 Type Inference (`typecheck/infer/expr.rs`)

```rust
fn forever(
    &mut self,
    seed: ExprId,
    state_param: &(String, Option<AstTypeExprId>),
    cont_param: &(String, Option<AstTypeExprId>),
    body: ExprId,
    span: Span,
) -> TyId {
    // 1. Infer seed type
    let seed_ty = self.expr(seed);

    // 2. State param type: annotation or seed_ty
    let state_ty = state_param.1
        .map(|ann| self.resolve_type_expr(ann))
        .unwrap_or(seed_ty);

    // 3. Unify state_ty with seed_ty
    self.unify(seed_ty, state_ty, span);

    // 4. Enter scope for body
    self.enter_scope();

    // 5. Bind state param
    self.bind(&state_param.0, state_ty);

    // 6. Continuation param type: (StateType) -> BodyType
    //    But we don't know BodyType yet; use a fresh type variable
    let body_ty_var = self.fresh_ty_var();
    let cont_ty = self.ty(Ty::Fn(vec![state_ty], Box::new(body_ty_var)));

    // 7. Bind continuation param (for type checking purposes)
    self.bind(&cont_param.0, cont_ty);

    // 8. Infer body type
    let actual_body_ty = self.expr(body);

    // 9. Unify body type with the return type of continuation
    self.unify(actual_body_ty, body_ty_var, span);

    self.exit_scope();

    // 10. The FOREVER expression has the same type as the body's exit type
    actual_body_ty
}
```

**Special consideration**: The continuation call `f(x)` should type-check as returning `body_ty`, even though at runtime it doesn't return (it restarts the loop). This is handled by step 6-9 above.

#### 6.2 Continuation Type Annotation

If the user writes:

```rumps
FOREVER seed (state: S, cont: (S) -> R) => body
```

The continuation type `(S) -> R` implies:
- State must be `S`
- Body must return `R`

The typechecker validates these constraints.

---

### Phase 7: Interpreter

#### 7.1 Continuation Detection Strategy

Two approaches:

**Approach A: Special `Continue` value**

The continuation is bound as a special `Value::Continue` marker. When evaluated, calls to it produce `Value::LoopContinue(new_state_id)` instead of normal return.

```rust
pub enum Value {
    // ... existing variants ...

    /// Continuation marker for FOREVER loops.
    ///
    /// Not a real callable; interpreter detects calls to this and
    /// produces `LoopContinue` instead of normal function call.
    ForeverContinuation,

    /// Loop continuation signal with new state.
    ///
    /// Not a normal value; only produced by calling a `ForeverContinuation`.
    LoopContinue(ValueId),
}
```

When the interpreter evaluates a `Call` where the callee is `ForeverContinuation`, it:
1. Evaluates the argument
2. Returns `Value::LoopContinue(arg_id)`

The `forever()` implementation checks for this and loops.

**Approach B: Name-based detection**

Store the continuation parameter name and detect calls to it by name matching. Simpler but fragile if the name is shadowed.

**Recommendation**: Approach A is cleaner and safer.

#### 7.2 Interpreter Implementation

```rust
/// Evaluate a `FOREVER` expression.
///
/// Implements the loop via tail recursion (or iteration with while-let).
#[async_recursion]
pub(super) async fn forever(
    &mut self,
    seed: ExprId,
    state_param: &(StringId, Option<TypeExprId>),
    cont_param: &(StringId, Option<TypeExprId>),
    body: ExprId,
    span: Span,
) -> Result<Value> {
    // Evaluate seed
    let mut state = self.eval(seed).await?;
    let mut state_id = self.arena.add(state.clone(), span);

    // Loop until body doesn't call continuation
    loop {
        // Push scope for this iteration
        self.env.scopes.push();

        // Bind state param
        self.env.scopes.bind(state_param.0, state_id);

        // Bind continuation as special marker
        let cont_id = self.arena.add(Value::ForeverContinuation, span);
        self.env.scopes.bind(cont_param.0, cont_id);

        // Evaluate body
        let result = self.eval(body).await?;

        // Pop scope
        self.env.scopes.pop();

        // Check if result is a continuation signal
        match result {
            Value::LoopContinue(new_state_id) => {
                // Continue loop with new state
                state = self.arena.get(new_state_id)
                    .cloned()
                    .ok_or_else(|| Error::runtime(span, "invalid loop state"))?;
                state_id = new_state_id;
            }
            _ => {
                // Body returned without calling continuation; exit loop
                break Ok(result);
            }
        }
    }
}
```

#### 7.3 Call Handling for Continuation

In `call.rs`, when the callee is `ForeverContinuation`:

```rust
Value::ForeverContinuation => {
    // Evaluate the single argument
    (args.len() == 1).then_some(())
        .ok_or_else(|| Error::runtime(span, "continuation takes exactly 1 argument"))?;

    let arg = self.eval(args[0]).await?;
    let arg_id = self.arena.add(arg, span);

    Ok(Value::LoopContinue(arg_id))
}
```

This is added to the `call_value` match.

---

### Phase 8: Error Handling

#### 8.1 Type Errors

- Seed type must match state param annotation (if provided)
- Body type must match continuation return type annotation (if provided)
- All exit paths must have compatible types

#### 8.2 Runtime Errors

- Calling continuation with wrong arity (must be exactly 1 argument)
- Calling continuation outside of FOREVER body (if somehow escaped; shouldn't be possible with current design)

---

### Phase 9: Tests

#### 9.1 Script Tests

Create `scripts/104_forever.rumps`:

```rumps
; Basic loop until condition
LET sum = FOREVER 0 (acc, next) => {
  IF acc < 10 {
    next(acc + 1)
  } ELSE {
    acc
  }
}
@OUTPUT sum  ; 10

; Countdown
LET countdown = FOREVER 5 (n, cont) => {
  IF n > 0 {
    @OUTPUT n
    cont(n - 1)
  } ELSE {
    "blast off"
  }
}
@OUTPUT countdown  ; "blast off"

; With type annotations
LET typed = FOREVER (0: Int) (x: Int, f: (Int) -> String) => {
  IF x < 3 {
    f(x + 1)
  } ELSE {
    "done"
  }
}
@OUTPUT typed  ; "done"

; Building a list (using OUTPUT as expression)
LET trace = FOREVER ([], 1) ((list, n), cont) => {
  IF n <= 3 {
    cont((Array.push(n, list), n + 1))
  } ELSE {
    list
  }
}
@OUTPUT trace  ; [1, 2, 3]
```

Note: The last example uses a tuple as state, which is a common pattern.

#### 9.2 Error Tests

```rumps
; Mismatched types
FOREVER 1 (x, f) => {
  IF x < 10 {
    f("oops")  ; Error: expected Int, got String
  } ELSE {
    x
  }
}

; Wrong arity
FOREVER 1 (x, f) => {
  f(1, 2)  ; Error: continuation takes 1 argument
}
```

---

## Implementation Checklist

### Phase 1: `@OUTPUT` as Expression
- [ ] **AST**: Add `Expr::Output(OutputStmt)` variant
- [ ] **Parser/CST**: Add `ExprKind::Output`
- [ ] **Parser**: Allow `@OUTPUT` in expression position
- [ ] **Lowering**: Handle `ExprKind::Output`
- [ ] **Typechecker**: `Expr::Output` has type `Unit`
- [ ] **Interpreter**: Evaluate `Expr::Output` with side effect, return `Unit`
- [ ] **Tests**: Script test for `@OUTPUT` in expression context

### Phase 2: Token
- [ ] **Lexer/Token**: Add `Token::Forever` keyword

### Phase 3: AST
- [ ] **AST**: Add `Expr::Forever { seed, state_param, cont_param, body }`

### Phase 4: Parser
- [ ] **CST**: Add `ForeverExpr` type
- [ ] **CST**: Add `ExprKind::Forever` variant
- [ ] **Parser**: Implement `forever_expr` parser
- [ ] **Parser**: Integrate into primary expression parsing

### Phase 5: Lowering
- [ ] **Lowering**: Convert `cst::Forever` to `ast::Forever`

### Phase 6: Typechecker
- [ ] **Typechecker**: Implement `forever()` type inference
- [ ] **Typechecker**: Handle continuation type annotation validation

### Phase 7: Interpreter
- [ ] **Value**: Add `Value::ForeverContinuation` marker
- [ ] **Value**: Add `Value::LoopContinue(ValueId)` signal
- [ ] **Interpreter**: Implement `forever()` evaluation with loop
- [ ] **Call handling**: Detect continuation calls, return `LoopContinue`

### Phase 8: Tests
- [ ] **Tests**: Basic FOREVER scripts (`104_forever.rumps`)
- [ ] **Tests**: Error cases (type mismatches, arity errors)
- [ ] **Tests**: Complex patterns (tuple state, nested loops)

---

## Design Notes

### Why Not Real CPS?

Traditional CPS would make the continuation a real function that captures the loop restart. This has issues:

1. **Stack growth**: Each continuation call adds a frame (unless tail-call optimized)
2. **Complexity**: Requires proper tail-call elimination or trampolining
3. **First-class continuation**: Allows escaping continuations, which is complex

By making the continuation a pseudo-function, we:
1. Use a simple loop in the interpreter (no stack growth)
2. Keep the syntax familiar (looks like a function call)
3. Avoid the complexity of first-class continuations

### Why Allow `@OUTPUT` as Expression?

We probably want to allow e.g. `f(@OUTPUT x)` syntax. This requires `@OUTPUT` to be:
1. Evaluable (produces `Unit`)
2. Usable in argument position

Making `@OUTPUT` an expression is a small language change with benefits:
- Cleaner `FOREVER` syntax
- Useful in other contexts (e.g., `let _ = @OUTPUT x`)
- Consistent with functional style (everything is an expression)

### Tuple State Pattern

A common pattern is using a tuple as state:

```rumps
FOREVER (0, []) ((count, list), next) => {
  IF count < 5 {
    next((count + 1, Array.push(count, list)))
  } ELSE {
    list
  }
}
```

This is natural with destructuring in the parameter position.

### Infinite Loops

`FOREVER` supports infinite loops by always calling the continuation:

```rumps
FOREVER Unit (_, next) => {
  ; do something forever
  next(Unit)
}
```

Such loops have type `Unit` (or the seed type). The typechecker should handle this gracefully.

---

## Future Extensions

### Named FOREVER

For nested loops, allow naming:

```rumps
FOREVER outer 0 (i, next_i) => {
  FOREVER inner 0 (j, next_j) => {
    IF j < 3 {
      next_j(j + 1)
    } ELSE IF i < 3 {
      next_i(i + 1)  ; Exit inner, continue outer
    } ELSE {
      "done"
    }
  }
}
```

This would require the continuation name to be scoped properly.

### Early Exit with Value

Allow exiting from any point without reaching the end:

```rumps
FOREVER 0 (n, next, exit) => {
  IF n == 42 {
    exit("found it")  ; Early return
  } ELSE {
    next(n + 1)
  }
}
```

This adds an `exit` continuation in addition to `next`.
