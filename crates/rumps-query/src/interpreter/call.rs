//! Function and closure calling.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::env::{PrimCtx, PrimFn};
use crate::io::IoContext;
use crate::value::{CapturedEnv, StringId, TypeExprId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Pipeline operator implementation.
    ///
    /// Applies the right operand (function/closure) to the left operand (value):
    /// `value |> func` becomes `func(value)`
    #[async_recursion]
    pub(super) async fn pipeline(
        &mut self,
        left: Value,
        right: Value,
        span: Span,
    ) -> Result<Value> {
        // Intern left value as argument
        let arg_id = self.arena.add(left, span);

        match right {
            Value::Closure {
                params,
                ret,
                body,
                env,
            } => {
                self.call_closure_with_vals(
                    &params,
                    ret,
                    body,
                    &env,
                    &[arg_id],
                    span,
                )
                .await
            }
            Value::Function {
                params, ret, body, ..
            } => {
                self.call_function_with_vals(
                    &params,
                    ret,
                    body,
                    &[arg_id],
                    span,
                )
                .await
            }
            Value::ModuleFn { path } => {
                self.call_module_fn_with_vals(&path, &[arg_id], span).await
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "`|>` requires function on right side; got {}",
                    right.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Call a closure with pre-evaluated arguments.
    #[async_recursion]
    async fn call_closure_with_vals(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        env: &CapturedEnv,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Save current scope stack and replace with captured environment
            let saved = self.env.scopes.save();
            self.env.scopes.restore_from_captured(env);

            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, args, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Restore original scope stack
            self.env.scopes.restore(saved);

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Call a named function with pre-evaluated arguments.
    #[async_recursion]
    async fn call_function_with_vals(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, args, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Pop parameter scope
            self.env.scopes.pop();

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Call a function with an expression-based callee.
    ///
    /// The callee can be:
    /// - A variable (`foo(x)`) resolved via name-based lookup
    /// - A field access (`obj.method(x)`) evaluated then called
    /// - Another call (`make_adder(5)(10)`) for chained calls
    /// - A closure literal (`(x => x * 2)(5)`) for IIFE
    #[async_recursion]
    pub(super) async fn call(
        &mut self,
        callee: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let callee_expr =
            self.ast.get_expr(callee).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid callee expression")
            })?;

        // For variable callees, use name-based resolution (functions first)
        match callee_expr {
            Expr::Var(ref name) => self.call_by_name(name, args, span).await,
            // Check if this is a variant constructor for a user-defined type
            Expr::Field(base_id, ref var_name) => {
                let maybe_variant =
                    self.ast.get_expr(base_id).and_then(|e| match e {
                        Expr::Var(ty_name) => {
                            let ty_id = self.arena.intern(ty_name);
                            self.registry.lookup(ty_id).and_then(|type_id| {
                                let var_id = self.arena.intern(var_name);
                                self.registry
                                    .lookup_variant(type_id, var_id)
                                    .map(|_| {
                                        (ty_name.clone(), var_name.clone())
                                    })
                            })
                        }
                        _ => None,
                    });

                if let Some((ty_name, var_name)) = maybe_variant {
                    // Handle as variant constructor
                    self.variant(&ty_name, &var_name, args, span).await
                } else {
                    // Evaluate callee expression and call the result
                    let callee_val = self.eval(callee).await?;
                    self.call_value(callee_val, args, span).await
                }
            }
            _ => {
                // Evaluate callee expression and call the result
                let callee_val = self.eval(callee).await?;
                self.call_value(callee_val, args, span).await
            }
        }
    }

    /// Call a function by name (for `Var` callees).
    ///
    /// Resolution order:
    /// 1. Named functions (from FUN definitions)
    /// 2. Lexical scope (may be a bound closure)
    ///
    /// Note: Built-in module functions (e.g., `Object.keys`) are resolved at
    /// parse time by `resolve.rs` and become `Expr::Path` nodes.
    #[async_recursion]
    async fn call_by_name(
        &mut self,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        let name_id = self.arena.intern(name);

        // Clone function def to avoid borrow issues with async
        let func_def = self.functions.get(&name_id).cloned();
        let scope_val = func_def.as_ref().map_or_else(
            || {
                self.env
                    .scopes
                    .lookup(name_id)
                    .and_then(|val_id| self.arena.get(val_id).cloned())
            },
            |_| None,
        );

        match (func_def, scope_val) {
            (Some(def), _) => {
                self.call_function(&def.params, def.ret, def.body, args, span)
                    .await
            }
            (None, Some(callee)) => self.call_value(callee, args, span).await,
            (None, None) => Err(Error::runtime(
                span,
                format!("undefined function `{name}`"),
            )),
        }
    }

    /// Call a built-in primitive function.
    ///
    /// Evaluates arguments first, then invokes the primitive with a `PrimCtx`.
    #[async_recursion]
    async fn call_primitive(
        &mut self,
        prim: PrimFn,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Evaluate arguments
        let arg_ids: SmallVec<[ValueId; 4]> =
            self.eval_args(args).await?.into_iter().collect();

        // Create context and call primitive
        let mut ctx = PrimCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        // Look up and clone the result value
        self.arena.get(result_id).cloned().ok_or_else(|| {
            Error::runtime(span, "primitive returned invalid value")
        })
    }

    /// Call a module function with pre-evaluated arguments.
    ///
    /// Used by pipeline and other contexts where arguments are already values.
    #[async_recursion]
    pub(super) async fn call_module_fn_with_vals(
        &mut self,
        path: &[StringId],
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        // Convert StringIds to owned Strings first to avoid borrow issues
        let path_strs: SmallVec<[String; 4]> = path
            .iter()
            .filter_map(|id| self.arena.get_str(*id).map(String::from))
            .collect();

        // Format path for error messages
        let path_display = path_strs.join(".");

        // Convert to &str for lookup
        let path_refs: SmallVec<[&str; 4]> =
            path_strs.iter().map(String::as_str).collect();

        let prim =
            self.env.get_module_fn(&path_refs).copied().ok_or_else(|| {
                Error::runtime(
                    span,
                    format!("unknown function `{path_display}`"),
                )
            })?;

        self.call_primitive_with_vals(prim, args, span).await
    }

    /// Call a primitive with pre-evaluated arguments.
    #[async_recursion]
    async fn call_primitive_with_vals(
        &mut self,
        prim: PrimFn,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let arg_ids: SmallVec<[ValueId; 4]> = args.iter().copied().collect();

        let mut ctx = PrimCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        self.arena.get(result_id).cloned().ok_or_else(|| {
            Error::runtime(span, "primitive returned invalid value")
        })
    }

    /// Call a function or closure value.
    #[async_recursion]
    async fn call_value(
        &mut self,
        callee: Value,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        match callee {
            Value::Closure {
                params,
                ret,
                body,
                env,
            } => {
                self.call_closure(&params, ret, body, &env, args, span)
                    .await
            }
            Value::Function {
                params, ret, body, ..
            } => self.call_function(&params, ret, body, args, span).await,
            Value::ModuleFn { path } => {
                // Evaluate arguments first, then call
                let arg_vals = self.eval_args(args).await?;
                self.call_module_fn_with_vals(&path, &arg_vals, span).await
            }
            _ => Err(Error::runtime(
                span,
                format!(
                    "cannot call non-function value of type {}",
                    callee.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Call a named function (no captured environment).
    #[async_recursion]
    pub(super) async fn call_function(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Check arity
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Evaluate arguments
            let arg_vals = self.eval_args(args).await?;

            // Push new scope and bind parameters
            self.env.scopes.push();
            self.bind_params(params, &arg_vals, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Pop scope
            self.env.scopes.pop();

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Call a closure (with captured environment).
    #[async_recursion]
    pub(super) async fn call_closure(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        env: &CapturedEnv,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Check arity
        if params.len() != args.len() {
            Err(Error::runtime(
                span,
                format!(
                    "expected {} arguments, got {}",
                    params.len(),
                    args.len()
                ),
            ))
        } else {
            // Evaluate arguments in current environment
            let arg_vals = self.eval_args(args).await?;

            // Save current scope stack and replace with captured environment
            let saved_scopes = self.env.scopes.save();
            self.env.scopes.restore_from_captured(env);

            // Push new scope for parameters
            self.env.scopes.push();
            self.bind_params(params, &arg_vals, span)?;

            // Evaluate body
            let result = self.eval(body).await;

            // Restore original scope stack
            self.env.scopes.restore(saved_scopes);

            // Validate return type if annotated
            result.and_then(|val| self.check_return_type(val, ret, span))
        }
    }

    /// Check that a return value matches the declared return type.
    fn check_return_type(
        &self,
        val: Value,
        ret: Option<TypeExprId>,
        span: Span,
    ) -> Result<Value> {
        ret.map_or(Ok(val.clone()), |expected_ty| {
            if self.value_matches_type_expr(&val, expected_ty) {
                Ok(val)
            } else {
                let expected = self.format_type_expr(expected_ty);
                let actual = val.type_name(&self.registry, &self.type_exprs);
                Err(Error::type_err(
                    span,
                    format!(
                        "expected return type `{expected}`, got `{actual}`"
                    ),
                ))
            }
        })
    }

    /// Evaluate a list of argument expressions.
    #[async_recursion]
    pub(super) async fn eval_args(
        &mut self,
        args: &[ExprId],
    ) -> Result<Vec<ValueId>> {
        self.eval_args_rec(args, Vec::with_capacity(args.len()))
            .await
    }

    #[async_recursion]
    async fn eval_args_rec(
        &mut self,
        args: &[ExprId],
        mut acc: Vec<ValueId>,
    ) -> Result<Vec<ValueId>> {
        match args.split_first() {
            None => Ok(acc),
            Some((head, tail)) => {
                let span = self.ast.expr_span(*head).unwrap_or_default();
                let val = self.eval(*head).await?;
                let val_id = self.arena.add(val, span);
                acc.push(val_id);
                self.eval_args_rec(tail, acc).await
            }
        }
    }

    /// Bind parameters to argument values in the current scope.
    ///
    /// Validates each argument against its declared type (if any).
    pub(super) fn bind_params(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        args: &[ValueId],
        span: Span,
    ) -> Result<()> {
        params
            .iter()
            .zip(args.iter())
            .try_for_each(|((name, ty), val_id)| {
                // Validate type if annotated
                ty.map_or(Ok(()), |expected_ty| {
                    self.arena.get(*val_id).cloned().map_or(Ok(()), |val| {
                        self.validate_param(&val, expected_ty, *name, span)
                    })
                })?;
                self.env.scopes.bind(*name, *val_id);
                Ok(())
            })
    }

    /// Validate a function parameter against its expected type.
    ///
    /// Provides detailed error messages, especially for struct types.
    fn validate_param(
        &self,
        val: &Value,
        expected_ty: TypeExprId,
        param_name: StringId,
        span: Span,
    ) -> Result<()> {
        let pname = self.arena.get_str(param_name).unwrap_or("?");

        self.get_struct_fields(expected_ty).map_or_else(
            || {
                // Non-struct type: use standard matching
                if self.value_matches_type_expr(val, expected_ty) {
                    Ok(())
                } else {
                    let expected = self.format_type_expr(expected_ty);
                    let actual =
                        val.type_name(&self.registry, &self.type_exprs);
                    Err(Error::type_err(
                        span,
                        format!(
                            "parameter `{pname}`: expected `{expected}`, \
                             got `{actual}`"
                        ),
                    ))
                }
            },
            |expected_fields| {
                // Struct type: validate with detailed errors
                match val {
                    Value::Object(obj) => self.validate_object_fields(
                        obj,
                        expected_fields,
                        span,
                        Some(pname),
                    ),
                    _ => {
                        let expected = self.format_type_expr(expected_ty);
                        let actual =
                            val.type_name(&self.registry, &self.type_exprs);
                        Err(Error::type_err(
                            span,
                            format!(
                                "parameter `{pname}`: expected `{expected}`, \
                                 got `{actual}`"
                            ),
                        ))
                    }
                }
            },
        )
    }
}
