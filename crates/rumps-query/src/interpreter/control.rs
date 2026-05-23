//! Control flow expressions: `if`, `match`, `catch`, blocks, coalesce, unwrap.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{
    AstTypeExprId, Expr, ExprId, MatchArm, PostfixOp, StmtId, TypePattern,
};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::value::{TypeId, Value};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
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
        let is_option = |ty_expr| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::OPTION)
        };
        let is_result = |ty_expr| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::RESULT)
        };

        // Unwrap Union/Newtype wrappers to find the inner Option/Result
        let unwrapped = self.unwrap_value_recursive(&val);
        let v = unwrapped.as_ref().unwrap_or(&val);

        match v {
            // Option.Some(v) -> v
            Value::Tagged(ty_expr, 1, payload) if is_option(*ty_expr) => {
                Ok(payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("!", "Option.Some has payload")
                    }))
            }
            // Option.None -> runtime error (not type error)
            Value::Tagged(ty_expr, 0, _) if is_option(*ty_expr) => {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            // Result.Ok(v) -> v
            Value::Tagged(ty_expr, 0, payload) if is_result(*ty_expr) => {
                Ok(payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("!", "Result.Ok has payload")
                    }))
            }
            // Result.Err(e) -> runtime error with stringified e
            Value::Tagged(ty_expr, 1, payload) if is_result(*ty_expr) => {
                let err_val =
                    payload.first().and_then(|id| self.arena.get(*id).cloned());
                let err_msg = err_val
                    .map(|v| self.stringify(&v))
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
    #[async_recursion]
    pub(super) async fn coalesce(
        &mut self,
        left: Value,
        rhs: ExprId,
    ) -> Result<Value> {
        // Helper to check if type expression has a given base type
        let is_option = |ty_expr| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::OPTION)
        };
        let is_result = |ty_expr| {
            self.type_exprs
                .base_type(ty_expr)
                .is_some_and(|t| t == TypeId::RESULT)
        };

        // Unwrap Union/Newtype wrappers to find the inner Option/Result
        let unwrapped = self.unwrap_value_recursive(&left);
        let v = unwrapped.as_ref().unwrap_or(&left);

        match v {
            // Option.Some(v) -> unwrap to v
            Value::Tagged(ty_expr, 1, payload) if is_option(*ty_expr) => {
                Ok(payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    // Payload should always exist for Some
                    .unwrap_or(Value::Unit))
            }
            // Option.None -> evaluate rhs
            Value::Tagged(ty_expr, 0, _) if is_option(*ty_expr) => {
                self.eval(rhs).await
            }
            // Result.Ok(v) -> unwrap to v
            Value::Tagged(ty_expr, 0, payload) if is_result(*ty_expr) => {
                Ok(payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    // Payload should always exist for Ok
                    .unwrap_or(Value::Unit))
            }
            // Result.Err(_) -> evaluate rhs (error discarded)
            Value::Tagged(ty_expr, 1, _) if is_result(*ty_expr) => {
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
    #[async_recursion]
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
                None => Ok(Value::Unit),
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
    #[async_recursion]
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
                let cond_val = self.eval(cond).await?;
                let cond_true = match cond_val {
                    Value::Bool(b) => b,
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
                            self.eval(then_br).await?;
                        }
                        Ok(Value::Unit)
                    }
                }
            }
        }
    }

    /// Handle `if expr is Type.Variant(bindings) { then } else { else }`.
    ///
    /// Bindings are only visible in the then branch.
    /// Type checking: same rules as regular `if`.
    #[async_recursion]
    async fn if_with_bindings(
        &mut self,
        expr: ExprId,
        ty_name: &QualifiedName,
        var_name: StringId,
        names: &[StringId],
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        let span = self.ast.expr_span(expr).unwrap_or_default();
        let val = self.eval(expr).await?;

        // Check if the value matches the variant
        let matched = self.check_variant(&val, ty_name, var_name, span)?;

        match else_br {
            Some(else_id) => {
                // if/else with bindings: only evaluate the taken branch
                if matched {
                    self.eval_with_variant_bindings(&val, names, then_br, span)
                        .await
                } else {
                    self.eval(else_id).await
                }
            }
            None => {
                // Single-arm if with bindings: body must be Unit.
                // Type checker guarantees body is Unit.
                if matched {
                    self.eval_with_variant_bindings(&val, names, then_br, span)
                        .await?;
                }
                Ok(Value::Unit)
            }
        }
    }

    /// Evaluate an expression with variant payload bindings in scope.
    ///
    /// Extracts payloads from `val`, validates arity against `names`,
    /// binds them in a new scope, evaluates `body`, then pops the scope.
    #[async_recursion]
    async fn eval_with_variant_bindings(
        &mut self,
        val: &Value,
        names: &[StringId],
        body: ExprId,
        span: Span,
    ) -> Result<Value> {
        // Unwrap Union/Newtype to find Tagged
        let unwrapped = self.unwrap_value_recursive(val);
        let v = unwrapped.as_ref().unwrap_or(val);

        let payloads = match v {
            Value::Tagged(_, _, p) => p.clone(),
            _ => SmallVec::new(),
        };

        // Typechecker validates pattern arity matches variant definition
        if payloads.len() != names.len() {
            typechecked!("variant bind", "arity match");
        }

        self.env.scopes.push();
        self.bind_payloads(names, &payloads, span);
        let result = self.eval(body).await;
        self.env.scopes.pop();
        result
    }

    /// Evaluate a `match` expression.
    ///
    /// Evaluates the scrutinee once, then tries each arm in order. The first
    /// arm whose pattern matches (and whose guard, if any, is `true`) has its
    /// body evaluated. Returns error if no arm matches.
    #[async_recursion]
    pub(super) async fn r#match(
        &mut self,
        scrutinee: ExprId,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Value> {
        let val = self.eval(scrutinee).await?;
        self.try_match_arms(&val, arms, span).await
    }

    /// Try each match arm in order until one matches.
    #[async_recursion]
    async fn try_match_arms(
        &mut self,
        val: &Value,
        arms: &[MatchArm],
        span: Span,
    ) -> Result<Value> {
        match arms.split_first() {
            // Typechecker validates exhaustiveness
            None => typechecked!("match", "exhaustive"),
            Some((arm, rest)) => {
                // Try to match the pattern
                match self.try_match_pattern(arm.pattern, val, span)? {
                    None => self.try_match_arms(val, rest, span).await,
                    Some(bindings) => {
                        // Pattern matched; check guard if present
                        self.env.scopes.push();
                        self.apply_bindings(&bindings, span);

                        let guard_ok = match arm.guard {
                            None => true,
                            Some(guard_expr) => {
                                let guard_val = self.eval(guard_expr).await?;
                                match guard_val {
                                    Value::Bool(b) => b,
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
                            self.try_match_arms(val, rest, span).await
                        }
                    }
                }
            }
        }
    }

    /// Evaluate a range expression.
    ///
    /// Creates a lazy `Value::Range` from start and end expressions.
    ///
    /// Type checker guarantees both bounds are `Int`.
    #[async_recursion]
    pub(super) async fn range(
        &mut self,
        start_id: ExprId,
        end_id: ExprId,
        inclusive: bool,
        _span: Span,
    ) -> Result<Value> {
        let start_val = self.eval(start_id).await?;
        let end_val = self.eval(end_id).await?;

        let start = match &start_val {
            Value::Int(n) => *n,
            _ => typechecked!("..", "Int"),
        };

        let end = match &end_val {
            Value::Int(n) => *n,
            _ => typechecked!("..", "Int"),
        };

        Ok(Value::Range {
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
    #[async_recursion]
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
                    let h = self.eval(handler).await?;
                    let msg_id = self.arena.intern(msg);
                    let payload_id =
                        self.arena.add(Value::String(msg_id), span);
                    let err_ty = self.type_exprs.named(TypeId::ERROR);
                    let err_val = Value::Tagged(
                        err_ty,
                        idx,
                        smallvec::smallvec![payload_id],
                    );
                    let arg_id = self.arena.add(err_val, span);
                    match h {
                        Value::Closure {
                            params,
                            ret,
                            body,
                            env,
                        } => {
                            self.invoke_closure(
                                &params,
                                ret,
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

    /// Evaluate a `forever` loop expression.
    ///
    /// `forever seed (state, cont) => body` is a continuation-passing loop:
    /// - Evaluates `seed` to get initial state
    /// - Binds `state` and `cont` in scope for each iteration
    /// - If body returns `LoopContinue(new_state)`, loops with new state
    /// - Otherwise returns the body value
    ///
    /// I normally avoid `loop`, but Rust lacks tail-call optimization and the
    /// naive recursive implementation overflows the stack after ~5k iterations.
    /// Since this is tail-recursive (all state captured in `state_id`), using
    /// an explicit loop is safe and uses constant stack space.
    pub(super) async fn forever(
        &mut self,
        seed: ExprId,
        state_param: (StringId, Option<AstTypeExprId>),
        cont_param: (StringId, Option<AstTypeExprId>),
        body: ExprId,
        span: Span,
    ) -> Result<Value> {
        let init = self.eval(seed).await?;
        let mut state_id = self.arena.add(init, span);

        let state_name_id = state_param.0;
        let cont_name_id = cont_param.0;

        loop {
            self.env.scopes.push();
            self.env.scopes.bind(state_name_id, state_id);

            let cont_id = self.arena.add(Value::ForeverContinuation, span);
            self.env.scopes.bind(cont_name_id, cont_id);

            let result = self.eval(body).await;
            self.env.scopes.pop();

            match result {
                Ok(Value::LoopContinue(new_state_id)) => {
                    state_id = new_state_id;
                }
                other => break other,
            }
        }
    }
}
