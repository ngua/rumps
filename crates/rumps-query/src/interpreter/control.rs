//! Control flow expressions: `IF`, `MATCH`, blocks, coalesce, unwrap.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId, MatchArm, StmtId, TypePattern};
use crate::io::IoContext;
use crate::value::{TypeId, Value};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Unwrap operator implementation (`!` postfix).
    ///
    /// Extracts the payload from `Option.Some` or `Result.Ok`; produces a
    /// runtime error for `Option.None` or `Result.Err(e)`.
    pub(super) fn unwrap(&self, val: Value, span: Span) -> Result<Value> {
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

        match &val {
            // Option.Some(v) -> v
            Value::Tagged(ty_expr, 1, payload) if is_option(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Option.Some missing payload")
                    })
            }
            // Option.None -> error
            Value::Tagged(ty_expr, 0, _) if is_option(*ty_expr) => {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            // Result.Ok(v) -> v
            Value::Tagged(ty_expr, 0, payload) if is_result(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Result.Ok missing payload")
                    })
            }
            // Result.Err(e) -> error with stringified e
            Value::Tagged(ty_expr, 1, payload) if is_result(*ty_expr) => {
                let err_msg = payload
                    .first()
                    .and_then(|id| self.arena.get(*id))
                    .map(|v| self.stringify(v))
                    .unwrap_or_else(|| "unknown error".into());
                Err(Error::runtime(span, format!("unwrap failed: {err_msg}")))
            }
            // Other types -> type error
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "`!` (unwrap) requires Option or Result; got {}",
                    val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            )),
        }
    }

    /// Null-coalescing operator implementation.
    ///
    /// Unwraps `Option` or `Result` values, falling back to rhs on None/Err:
    /// - `Option.Some(v)` -> `v` (unwrapped)
    /// - `Option.None` -> evaluate and return rhs
    /// - `Result.Ok(v)` -> `v` (unwrapped)
    /// - `Result.Err(_)` -> evaluate and return rhs (error discarded)
    /// - Other types -> type error
    #[async_recursion]
    pub(super) async fn coalesce(
        &mut self,
        left: Value,
        rhs: ExprId,
        span: Span,
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

        match &left {
            // Option.Some(v) -> unwrap to v
            Value::Tagged(ty_expr, 1, payload) if is_option(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Option.Some missing payload")
                    })
            }
            // Option.None -> evaluate rhs
            Value::Tagged(ty_expr, 0, _) if is_option(*ty_expr) => {
                self.eval(rhs).await
            }
            // Result.Ok(v) -> unwrap to v
            Value::Tagged(ty_expr, 0, payload) if is_result(*ty_expr) => {
                payload
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "Result.Ok missing payload")
                    })
            }
            // Result.Err(_) -> evaluate rhs (error discarded)
            Value::Tagged(ty_expr, 1, _) if is_result(*ty_expr) => {
                self.eval(rhs).await
            }
            // Other types -> type error
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "`??` requires Option or Result; got {}",
                    left.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            )),
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

    /// Evaluate an `IF` expression.
    ///
    /// Type checking rules:
    /// - Single-arm `IF` (no `ELSE`): body must be `Unit`, whole expr is `Unit`
    /// - `IF/ELSE`: both branches must have matching types
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
            Some(Expr::Is(expr, TypePattern::VariantBind(ty, var, names))) => {
                self.if_with_bindings(expr, &ty, &var, &names, then_br, else_br)
                    .await
            }
            _ => {
                let cond_val = self.eval(cond).await?;
                let cond_true = match cond_val {
                    Value::Bool(b) => b,
                    _ => {
                        let span = self.ast.expr_span(cond).unwrap_or_default();
                        Err(Error::runtime_type(
                            span,
                            "IF condition must be Bool",
                        ))?
                    }
                };

                match else_br {
                    Some(else_id) => {
                        // IF/ELSE: only evaluate the taken branch
                        // Type checking deferred to static analysis; we can't
                        // evaluate both branches at runtime (side effects).
                        if cond_true {
                            self.eval(then_br).await
                        } else {
                            self.eval(else_id).await
                        }
                    }
                    None => {
                        // Single-arm IF: body must be Unit (side-effect only)
                        // Only evaluate if condition is true; type check when evaluated.
                        if cond_true {
                            let then_val = self.eval(then_br).await?;
                            self.check_unit(
                                &then_val,
                                then_br,
                                Span::default(),
                            )?;
                        }
                        Ok(Value::Unit)
                    }
                }
            }
        }
    }

    /// Handle `IF expr is Type.Variant(bindings) { then } ELSE { else }`.
    ///
    /// Bindings are only visible in the then branch.
    /// Type checking: same rules as regular `IF`.
    #[async_recursion]
    async fn if_with_bindings(
        &mut self,
        expr: ExprId,
        ty_name: &str,
        var_name: &str,
        names: &[String],
        then_br: ExprId,
        else_br: Option<ExprId>,
    ) -> Result<Value> {
        let span = self.ast.expr_span(expr).unwrap_or_default();
        let val = self.eval(expr).await?;

        // Check if the value matches the variant
        let matched = self.check_variant(&val, ty_name, var_name, span)?;

        match else_br {
            Some(else_id) => {
                // IF/ELSE with bindings: only evaluate the taken branch
                if matched {
                    self.eval_with_variant_bindings(
                        &val, ty_name, var_name, names, then_br, span,
                    )
                    .await
                } else {
                    self.eval(else_id).await
                }
            }
            None => {
                // Single-arm IF with bindings: body must be Unit
                if matched {
                    let then_val = self
                        .eval_with_variant_bindings(
                            &val, ty_name, var_name, names, then_br, span,
                        )
                        .await?;
                    self.check_unit(&then_val, then_br, span)?;
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
        ty_name: &str,
        var_name: &str,
        names: &[String],
        body: ExprId,
        span: Span,
    ) -> Result<Value> {
        let payloads = match val {
            Value::Tagged(_, _, p) => p.clone(),
            _ => SmallVec::new(),
        };

        (payloads.len() == names.len())
            .then_some(())
            .ok_or_else(|| {
                Error::runtime(
                    span,
                    format!(
                        "`{ty_name}.{var_name}` has {} payload(s), \
                     but {} binding(s) provided",
                        payloads.len(),
                        names.len()
                    ),
                )
            })?;

        self.env.scopes.push();
        self.bind_payloads(names, &payloads, span);
        let result = self.eval(body).await;
        self.env.scopes.pop();
        result
    }

    /// Check that a value is `Unit`; error otherwise.
    fn check_unit(
        &mut self,
        val: &Value,
        expr: ExprId,
        fallback_span: Span,
    ) -> Result<()> {
        let ty = self.value_type_expr(val);
        let unit_ty = self.type_exprs.named(TypeId::UNIT);
        if self.type_exprs.eq(ty, unit_ty) {
            Ok(())
        } else {
            let span = self.ast.expr_span(expr).unwrap_or(fallback_span);
            let ty_name =
                val.type_name(&self.registry, &self.type_exprs, &self.arena);
            Err(Error::runtime_type(
                span,
                format!("single-arm IF body must be Unit; got {ty_name}"),
            ))
        }
    }

    /// Evaluate a `MATCH` expression.
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
            None => Err(Error::runtime(span, "non-exhaustive match")),
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
                                    _ => {
                                        let g_span = self
                                            .ast
                                            .expr_span(guard_expr)
                                            .unwrap_or_default();
                                        Err(Error::runtime_type(
                                            g_span,
                                            "MATCH guard must be Bool",
                                        ))?
                                    }
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
    /// Both must evaluate to integers.
    #[async_recursion]
    pub(super) async fn range(
        &mut self,
        start_id: ExprId,
        end_id: ExprId,
        inclusive: bool,
        span: Span,
    ) -> Result<Value> {
        let start_val = self.eval(start_id).await?;
        let end_val = self.eval(end_id).await?;

        let start = match &start_val {
            Value::Int(n) => *n,
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "range start must be Int; got {}",
                    start_val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            ))?,
        };

        let end = match &end_val {
            Value::Int(n) => *n,
            _ => Err(Error::runtime_type(
                span,
                format!(
                    "range end must be Int; got {}",
                    end_val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena
                    )
                ),
            ))?,
        };

        Ok(Value::Range {
            start,
            end,
            inclusive,
        })
    }
}
