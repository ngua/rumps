//! Control flow expressions: `if`, `match`, `catch`, blocks, coalesce, unwrap.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId, MatchArm, PostfixOp, StmtId, TypePattern};
use crate::intern::{QualifiedName, StringId};
use crate::value::{Payload, TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl Interpreter<'_, '_> {
    /// Postfix operator implementation.
    ///
    /// Type checker guarantees operand satisfies the operator's constraints.
    pub(super) fn postfix(
        &mut self,
        op: PostfixOp,
        val: Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            PostfixOp::Unwrap => self.unwrap(val, span),
        }
    }

    /// Unwrap operator implementation (`!` postfix).
    ///
    /// Extracts the payload from `Option.Some` or `Result.Ok`; produces a
    /// runtime error for `Option.None` or `Result.Err(e)`.
    ///
    /// Type checker guarantees operand is `Option` or `Result`.
    /// `None`/`Err` remain runtime errors (value-level, not type-level).
    fn unwrap(&mut self, val: Value, span: Span) -> Result<Value> {
        let base = self
            .checked
            .types
            .to_type_id(val.repr)
            .or_else(|| self.checked.types.to_type_id(val.ty));
        match (base, &val.payload) {
            // Option.Some(v) -> v
            (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                Ok(vals
                    .first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("!", "Option.Some has payload")
                    }))
            }
            // Option.None -> runtime error (not type error)
            (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            // Result.Ok(v) -> v
            (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                Ok(vals
                    .first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("!", "Result.Ok has payload")
                    }))
            }
            // Result.Err(e) -> runtime error with stringified e
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, vals }) => {
                let err_val =
                    vals.first().and_then(|id| self.arena.value(*id).cloned());
                let err_msg = err_val
                    .map(|v| self.stringify_value(&v))
                    .unwrap_or_else(|| "unknown error".into());
                Err(Error::runtime(
                    span,
                    format!("cannot unwrap Result.Err: {err_msg}"),
                ))
            }
            // Type checker guarantees Option or Result
            _ => typechecked!("!", "Fallible"),
        }
    }

    /// Null-coalescing operator implementation.
    ///
    /// Unwraps `Option` or `Result` values, falling back to rhs on None/Err:
    /// - `Option.Some(v)` -> `v` (unwrapped)
    /// - `Option.None` -> evaluate and return rhs
    /// - `Result.Ok(v)` -> `v` (unwrapped)
    /// - `Result.Err(_)` -> evaluate and return rhs (error discarded)
    ///
    /// Type checker guarantees operand is `Option` or `Result`.
    pub(super) async fn coalesce(
        &mut self,
        left: Value,
        rhs: ExprId,
    ) -> Result<Value> {
        let base = self
            .checked
            .types
            .to_type_id(left.repr)
            .or_else(|| self.checked.types.to_type_id(left.ty));
        match (base, &left.payload) {
            // Option.Some(v) -> unwrap to v
            (Some(TypeId::OPTION), Payload::Variant { tag: 1, vals }) => {
                Ok(vals
                    .first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("??", "Option.Some has payload")
                    }))
            }
            // Option.None -> evaluate rhs
            (Some(TypeId::OPTION), Payload::Variant { tag: 0, .. }) => {
                self.eval(rhs).await
            }
            // Result.Ok(v) -> unwrap to v
            (Some(TypeId::RESULT), Payload::Variant { tag: 0, vals }) => {
                Ok(vals
                    .first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("??", "Result.Ok has payload")
                    }))
            }
            // Result.Err(_) -> evaluate rhs (error discarded)
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, .. }) => {
                self.eval(rhs).await
            }
            // Type checker guarantees Option or Result
            _ => typechecked!("??", "Fallible"),
        }
    }

    /// Evaluate a block expression.
    ///
    /// Executes statements, then evaluates the trailing expression (if any).
    /// Returns `Unit` if no trailing expression.
    pub(super) async fn block(
        &mut self,
        stmts: &[StmtId],
        tail: Option<ExprId>,
    ) -> Result<Value> {
        self.env.scopes.push();
        // Hoist local function declarations for forward references
        self.hoist_declarations(stmts).await?;
        let result = self.block_inner(stmts, tail).await;
        self.env.scopes.pop();
        result
    }

    /// Inner helper for block expression evaluation.
    #[async_recursion]
    async fn block_inner(
        &mut self,
        stmts: &[StmtId],
        tail: Option<ExprId>,
    ) -> Result<Value> {
        match stmts.split_first() {
            None => match tail {
                Some(e) => self.eval(e).await,
                None => Ok(self.value_from_meta(
                    Payload::Unit,
                    self.checked.types.meta_unit(),
                )),
            },
            Some((head, rest)) => {
                self.exec(*head).await?;
                self.block_inner(rest, tail).await
            }
        }
    }

    /// Evaluate an `if` expression.
    ///
    /// Type checking rules:
    /// - Single-arm `if` (no `else`): body must be `Unit`, whole expr is `Unit`
    /// - `if/else`: both branches must have matching types
    ///
    /// Special handling for `is` conditions with bindings: if the condition is
    /// `expr is Pattern(bindings)`, the bindings are only visible in the then
    /// branch, not in the else branch.
    pub(super) async fn r#if(
        &mut self,
        cond: ExprId,
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        // Check if condition is `Expr::Is` with bindings
        let cond_expr = self.ast.get_expr(cond).cloned();
        match cond_expr {
            Some(Expr::Is(
                expr,
                TypePattern::VariantBind(ref ty, var, names),
            )) => {
                self.if_with_bindings(expr, ty, var, &names, then_br, else_br)
                    .await
            }
            _ => {
                let cond_val = self.eval_payload(cond).await?;
                let cond_true = match cond_val {
                    Payload::Bool(b) => b,
                    _ => typechecked!("if condition", "Bool"),
                };

                match else_br {
                    Some(else_id) => {
                        // if/else: only evaluate the taken branch
                        // Type checking deferred to static analysis; we can't
                        // evaluate both branches at runtime (side effects).
                        if cond_true {
                            self.eval(then_br).await
                        } else {
                            self.eval(else_id).await
                        }
                    }
                    None => {
                        // Single-arm if: body must be Unit (side-effect only).
                        // Type checker guarantees body is Unit.
                        if cond_true {
                            self.eval_payload(then_br).await?;
                        }
                        Ok(self.value_from_meta(
                            Payload::Unit,
                            self.checked.types.meta_unit(),
                        ))
                    }
                }
            }
        }
    }

    /// Handle `if expr is Type.Variant(bindings) { then } else { else }`.
    ///
    /// Bindings are only visible in the then branch.
    /// Type checking: same rules as regular `if`.
    async fn if_with_bindings(
        &mut self,
        expr: ExprId,
        ty_name: &QualifiedName,
        var_name: StringId,
        names: &[StringId],
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        let val = self.eval(expr).await?;
        // Check if the value matches the variant
        let matched = self.check_variant(&val, ty_name, var_name)?;

        match else_br {
            Some(else_id) => {
                // if/else with bindings: only evaluate the taken branch
                if matched {
                    self.eval_with_variant_bindings(
                        &val.payload,
                        names,
                        then_br,
                    )
                    .await
                } else {
                    self.eval(else_id).await
                }
            }
            None => {
                // Single-arm if with bindings: body must be Unit.
                // Type checker guarantees body is Unit.
                if matched {
                    self.eval_with_variant_bindings(
                        &val.payload,
                        names,
                        then_br,
                    )
                    .await?;
                }
                Ok(self.value_from_meta(
                    Payload::Unit,
                    self.checked.types.meta_unit(),
                ))
            }
        }
    }

    /// Evaluate an expression with variant payload bindings in scope.
    ///
    /// Extracts payloads from `val`, validates arity against `names`,
    /// binds them in a new scope, evaluates `body`, then pops the scope.
    async fn eval_with_variant_bindings(
        &mut self,
        val: &Payload,
        names: &[StringId],
        body: ExprId,
    ) -> Result<Value> {
        let payloads = match val {
            Payload::Variant { vals, .. } => vals.clone(),
            _ => SmallVec::new(),
        };

        // Typechecker validates pattern arity matches variant definition
        if payloads.len() != names.len() {
            typechecked!("variant bind", "arity match");
        }

        self.env.scopes.push();
        self.bind_payloads(names, &payloads);
        let result = self.eval(body).await;
        self.env.scopes.pop();
        result
    }

    /// Evaluate a `match` expression.
    ///
    /// Evaluates the scrutinee once, then tries each arm in order. The first
    /// arm whose pattern matches (and whose guard, if any, is `true`) has its
    /// body evaluated. Returns error if no arm matches.
    pub(super) async fn r#match(
        &mut self,
        scrutinee: ExprId,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(scrutinee).await?;
        let val_id = self.add_value(val.clone(), span);
        self.try_match_arms(scrutinee, val_id, &val, arms, span)
            .await
    }

    /// Try each match arm in order until one matches.
    #[async_recursion]
    async fn try_match_arms(
        &mut self,
        scrutinee: ExprId,
        val_id: ValueId,
        val: &Value,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Value> {
        match arms.split_first() {
            // Typechecker validates exhaustiveness
            None => typechecked!("match", "exhaustive"),
            Some((arm, rest)) => {
                // Try to match the pattern
                match self.try_match_pattern(
                    scrutinee,
                    arm.pattern,
                    Some(val_id),
                    val,
                    span,
                )? {
                    None => {
                        self.try_match_arms(scrutinee, val_id, val, rest, span)
                            .await
                    }
                    Some(bindings) => {
                        // Pattern matched; check guard if present
                        self.env.scopes.push();
                        self.apply_bindings(&bindings);

                        let guard_ok = match arm.guard {
                            None => true,
                            Some(guard_expr) => {
                                let guard_val =
                                    self.eval_payload(guard_expr).await?;
                                match guard_val {
                                    Payload::Bool(b) => b,
                                    _ => typechecked!("match guard", "Bool"),
                                }
                            }
                        };

                        if guard_ok {
                            // Guard passed; evaluate body with bindings in scope
                            let result = self.eval(arm.body).await;
                            self.env.scopes.pop();
                            result
                        } else {
                            // Guard failed; pop scope and try next arm
                            self.env.scopes.pop();
                            self.try_match_arms(
                                scrutinee, val_id, val, rest, span,
                            )
                            .await
                        }
                    }
                }
            }
        }
    }

    /// Evaluate a range expression.
    ///
    /// Creates a lazy `Payload::Range` from start and end expressions.
    ///
    /// Type checker guarantees both bounds are `Int`.
    pub(super) async fn range(
        &mut self,
        start_id: ExprId,
        end_id: ExprId,
        inclusive: bool,
    ) -> Result<Payload> {
        let start_val = self.eval_payload(start_id).await?;
        let end_val = self.eval_payload(end_id).await?;

        let start = match &start_val {
            Payload::Int(n) => *n,
            _ => typechecked!("..", "Int"),
        };

        let end = match &end_val {
            Payload::Int(n) => *n,
            _ => typechecked!("..", "Int"),
        };

        Ok(Payload::Range {
            start,
            end,
            inclusive,
        })
    }

    /// Evaluate a `catch` expression.
    ///
    /// `expr catch e => handler` evaluates `expr`; if it raises a catchable
    /// runtime error, invokes `handler` with the `Error` value. Non-catchable
    /// errors (lex, parse, type) propagate.
    pub(super) async fn catch(
        &mut self,
        expr: ExprId,
        handler: ExprId,
        span: Span,
    ) -> Result<Value> {
        match self.eval(expr).await {
            Ok(val) => Ok(val),
            Err(e) => match e.runtime_variant() {
                Some((idx, msg)) => {
                    let h = self.eval_payload(handler).await?;
                    let msg_id = self.arena.intern(msg);
                    let payload_id = self.add_val(
                        Payload::String(msg_id),
                        self.checked.types.meta_string(),
                        span,
                    );
                    let err_val = Payload::Variant {
                        tag: idx,
                        vals: smallvec::smallvec![payload_id],
                    };
                    let arg_id = self.arena.add_typed(
                        err_val,
                        self.checked.types.meta_runtime_error(),
                        span,
                    );
                    match h {
                        Payload::Closure {
                            params, body, env, ..
                        } => {
                            self.invoke_closure(
                                &params,
                                body,
                                &env,
                                &[arg_id],
                                span,
                            )
                            .await
                        }
                        _ => typechecked!("catch handler", "Closure"),
                    }
                }
                None => Err(e),
            },
        }
    }

    /// Evaluate a `loop` expression.
    ///
    /// `loop seed (state, cont) => body` is a continuation-passing loop:
    /// - Evaluates `seed` to get initial state
    /// - Binds `state` and `cont` in scope for each iteration
    /// - If body returns `LoopContinue(new_state)`, loops with new state
    /// - Otherwise returns the body value
    ///
    /// I normally avoid `loop`, but Rust lacks tail-call optimization and the
    /// naive recursive implementation overflows the stack after ~5k iterations.
    /// Since this is tail-recursive (all state captured in `state_id`), using
    /// an explicit loop is safe and uses constant stack space.
    pub(super) async fn r#loop(
        &mut self,
        seed: ExprId,
        state_name_id: StringId,
        cont_name_id: StringId,
        body: ExprId,
        span: Span,
    ) -> Result<Value> {
        let init = self.eval(seed).await?;
        let mut state_id = self.add_value(init, span);

        loop {
            self.env.scopes.push();
            self.env.scopes.bind(state_name_id, state_id);

            let cont_id = self.add_payload(Payload::LoopContinuation, span);
            self.env.scopes.bind(cont_name_id, cont_id);

            let result = self.eval(body).await;
            self.env.scopes.pop();

            match result {
                Ok(Value {
                    payload: Payload::LoopContinue(new_state_id),
                    ..
                }) => {
                    state_id = new_state_id;
                }
                other => break other,
            }
        }
    }
}
