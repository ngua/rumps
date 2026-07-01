//! Control flow expressions: `if`, `match`, `catch`, blocks, and unwrap.

use std::ops::ControlFlow;

use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId, MatchArm, PostfixOp, StmtId, TypePattern};
use crate::intern::StringId;
use crate::value::{Payload, TypeId, Value};
use crate::{ClassId, Error, Result, Span};

impl Interpreter<'_, '_> {
    /// Postfix operator implementation.
    ///
    /// Type checker guarantees operand satisfies the operator's constraints.
    pub(super) async fn postfix(
        &mut self,
        op: PostfixOp,
        val: Value,
        span: Span,
    ) -> Result<Value> {
        match op {
            PostfixOp::Unwrap => self.unwrap(val, span).await,
        }
    }

    /// Unwrap operator implementation (`!` postfix).
    ///
    /// Extracts the payload from `Option.Some` or `Result.Ok`; produces a
    /// runtime error for `Option.None` or `Result.Err(e)`.
    ///
    /// Type checker guarantees operand is `Option` or `Result`.
    /// `None`/`Err` remain runtime errors (value-level, not type-level).
    async fn unwrap(&mut self, val: Value, span: Span) -> Result<Value> {
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
            // Result.Err(e) -> runtime error.
            (Some(TypeId::RESULT), Payload::Variant { tag: 1, vals }) => {
                let base = "cannot unwrap Result.Err";
                let method = self.arena.intern("display");
                let msg = match vals.first().copied() {
                    Some(vid)
                        if self.arena.value(vid).is_some_and(|v| {
                            self.has_class_instance(
                                ClassId::DISPLAY,
                                method,
                                v.ty,
                            )
                        }) =>
                    {
                        let val = self
                            .dispatch_class_method_value(
                                super::class::Dispatch::internal(
                                    ClassId::DISPLAY,
                                    method,
                                    SmallVec::from_slice(&[vid]),
                                    None,
                                    span,
                                ),
                            )
                            .await?;
                        match val.payload {
                            Payload::String(sid) => {
                                let s = self.arena.get_str(sid).unwrap_or("");
                                format!("{base}: {s}")
                            }
                            _ => typechecked!(
                                "Result.Err display",
                                "Display:display returned String"
                            ),
                        }
                    }
                    Some(_) | None => base.to_owned(),
                };
                Err(Error::runtime(span, msg))
            }
            // Type checker guarantees Option or Result
            _ => typechecked!("!", "Fallible"),
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

        let mut it = stmts.iter();
        let result = async {
            while let Some(stmt) = it.next() {
                self.exec(*stmt).await?;
            }

            match tail {
                Some(e) => self.eval(e).await,
                None => Ok(self.value_from_meta(
                    Payload::Unit,
                    self.checked.types.meta_unit(),
                )),
            }
        }
        .await;

        self.env.scopes.pop();
        result
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
                let val = self.eval(expr).await?;
                let matched = self.check_variant(&val, ty, var)?;

                if matched {
                    let vals = match &val.payload {
                        Payload::Variant { vals, .. } => vals.clone(),
                        _ => SmallVec::new(),
                    };

                    // Typechecker validates pattern arity matches variant definition.
                    if vals.len() != names.len() {
                        typechecked!("variant bind", "arity match");
                    }

                    self.env.scopes.push();
                    self.bind_payloads(&names, &vals);
                    let result = self.eval(then_br).await;
                    self.env.scopes.pop();

                    match else_br {
                        Some(_) => result,
                        None => {
                            // Single-arm `if` with bindings; body must be `Unit`.
                            // Type checker guarantees body is `Unit`.
                            result?;
                            Ok(self.value_from_meta(
                                Payload::Unit,
                                self.checked.types.meta_unit(),
                            ))
                        }
                    }
                } else {
                    match else_br {
                        Some(else_id) => self.eval(else_id).await,
                        None => {
                            // Single-arm `if` with bindings; body must be `Unit`.
                            // Type checker guarantees body is `Unit`.
                            Ok(self.value_from_meta(
                                Payload::Unit,
                                self.checked.types.meta_unit(),
                            ))
                        }
                    }
                }
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

        let mut it = arms.iter();
        let mut flow = ControlFlow::Continue(());
        while let Some(arm) = match &flow {
            ControlFlow::Continue(()) => it.next(),
            ControlFlow::Break(_) => None,
        } {
            flow = match self.try_match_pattern(
                scrutinee,
                arm.pattern,
                Some(val_id),
                &val,
                span,
            )? {
                None => ControlFlow::Continue(()),
                Some(bindings) => {
                    self.env.scopes.push();
                    self.apply_bindings(&bindings);

                    let guard = match arm.guard {
                        None => Ok(true),
                        Some(guard_expr) => self
                            .eval_payload(guard_expr)
                            .await
                            .map(|guard_val| match guard_val {
                                Payload::Bool(b) => b,
                                _ => typechecked!("match guard", "Bool"),
                            }),
                    };

                    let next = match guard {
                        Ok(true) => {
                            ControlFlow::Break(self.eval(arm.body).await)
                        }
                        Ok(false) => ControlFlow::Continue(()),
                        Err(e) => ControlFlow::Break(Err(e)),
                    };

                    self.env.scopes.pop();
                    next
                }
            };
        }

        match flow {
            ControlFlow::Break(result) => result,
            ControlFlow::Continue(()) => typechecked!("match", "exhaustive"),
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
