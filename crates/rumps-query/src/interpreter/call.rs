//! Function and closure calling.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::env::{PrimCtx, PrimFn};
use crate::io::IoContext;
use crate::value::{CapturedEnv, StringId, TypeExprId, TypeId, Value, ValueId};
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
                self.invoke_closure(&params, ret, body, &env, &[arg_id], span)
                    .await
            }
            Value::Function {
                params, ret, body, ..
            } => {
                self.invoke_function(&params, ret, body, &[arg_id], span)
                    .await
            }
            Value::ModuleFn { path } => {
                self.invoke_module_fn(&path, &[arg_id], span).await
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

    /// Invoke a closure with pre-evaluated arguments.
    #[async_recursion]
    async fn invoke_closure(
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

    /// Invoke a named function with pre-evaluated arguments.
    #[async_recursion]
    async fn invoke_function(
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

    /// Invoke a module function with pre-evaluated arguments.
    ///
    /// Array functions (`Array.map`, `Array.filter`, `Array.reduce`) are
    /// higher-order and need special handling since they invoke closures.
    #[async_recursion]
    pub(super) async fn invoke_module_fn(
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

        // Higher-order functions (those that invoke closures/functions passed as
        // arguments) MUST be handled here, not in `primitives.rs`. The `PrimFn`
        // signature only receives values; it has no access to the interpreter's
        // closure invocation machinery (`invoke_callable`). See `primitives.rs`
        // module docs for details.
        match path_refs.as_slice() {
            ["Array", "map"] => self.array_map(args, span).await,
            ["Array", "filter"] => self.array_filter(args, span).await,
            ["Array", "reduce"] => self.array_reduce(args, span).await,
            _ => {
                // Regular module function
                let prim =
                    self.env.get_module_fn(&path_refs).copied().ok_or_else(
                        || {
                            Error::runtime(
                                span,
                                format!("unknown function `{path_display}`"),
                            )
                        },
                    )?;

                self.invoke_primitive(prim, args, span).await
            }
        }
    }

    /// `Array.map(fn, arr) -> Array`
    ///
    /// Applies `fn` to each element of `arr`, returning a new array.
    /// Validates that all results have the same type (homogeneous array).
    #[async_recursion]
    async fn array_map(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        (args.len() == 2).then_some(()).ok_or_else(|| {
            Error::runtime(
                span,
                format!("Array.map expects 2 arguments, got {}", args.len()),
            )
        })?;

        let fn_id = *args.first().ok_or_else(|| {
            Error::runtime(span, "Array.map: missing function")
        })?;
        let arr_id = *args
            .get(1)
            .ok_or_else(|| Error::runtime(span, "Array.map: missing array"))?;

        let (_, elems) = self
            .arena
            .get_array(arr_id)
            .ok_or_else(|| Error::type_err(span, "Array.map expects Array"))?;

        self.array_map_rec(fn_id, &elems, SmallVec::new(), None, span)
            .await
    }

    /// Recursive helper for `Array.map`.
    #[async_recursion]
    async fn array_map_rec(
        &mut self,
        fn_id: ValueId,
        elems: &[ValueId],
        acc: SmallVec<[ValueId; 4]>,
        first_ty: Option<TypeId>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => {
                let elem_ty = first_ty
                    .map(|ty| self.type_exprs.named(ty))
                    .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
                Ok(Value::Array(elem_ty, acc))
            }
            Some((head, tail)) => {
                let result =
                    self.invoke_callable(fn_id, &[*head], span).await?;
                let result_ty = self
                    .arena
                    .base_type_of(result, &self.type_exprs)
                    .ok_or_else(|| {
                        Error::runtime(span, "Array.map: invalid result")
                    })?;

                // Check homogeneity
                let checked_ty = first_ty.map_or_else(
                    || Ok(result_ty),
                    |fty| {
                        if fty == result_ty {
                            Ok(fty)
                        } else {
                            Err(Error::runtime(
                                span,
                                "Array.map: function produces heterogeneous \
                                 results; all elements must have the same type",
                            ))
                        }
                    },
                )?;

                let mut new_acc = acc;
                new_acc.push(result);
                self.array_map_rec(fn_id, tail, new_acc, Some(checked_ty), span)
                    .await
            }
        }
    }

    /// `Array.filter(predicate, arr) -> Array`
    ///
    /// Returns a new array containing only elements for which `predicate`
    /// returns a truthy value.
    #[async_recursion]
    async fn array_filter(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        (args.len() == 2).then_some(()).ok_or_else(|| {
            Error::runtime(
                span,
                format!("Array.filter expects 2 arguments, got {}", args.len()),
            )
        })?;

        let pred_id = *args.first().ok_or_else(|| {
            Error::runtime(span, "Array.filter: missing predicate")
        })?;
        let arr_id = *args.get(1).ok_or_else(|| {
            Error::runtime(span, "Array.filter: missing array")
        })?;

        let (elem_ty, elems) =
            self.arena.get_array(arr_id).ok_or_else(|| {
                Error::type_err(span, "Array.filter expects Array")
            })?;

        self.array_filter_rec(pred_id, elem_ty, &elems, SmallVec::new(), span)
            .await
    }

    /// Recursive helper for `Array.filter`.
    #[async_recursion]
    async fn array_filter_rec(
        &mut self,
        pred_id: ValueId,
        elem_ty: TypeExprId,
        elems: &[ValueId],
        acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Array(elem_ty, acc)),
            Some((head, tail)) => {
                let result =
                    self.invoke_callable(pred_id, &[*head], span).await?;
                let result_val =
                    self.arena.get(result).cloned().ok_or_else(|| {
                        Error::runtime(span, "Array.filter: invalid result")
                    })?;

                let keep = result_val.is_truthy(&self.arena, &self.type_exprs);
                let mut new_acc = acc;
                if keep {
                    new_acc.push(*head);
                }
                self.array_filter_rec(pred_id, elem_ty, tail, new_acc, span)
                    .await
            }
        }
    }

    /// `Array.reduce(reducer, init, arr) -> T`
    ///
    /// Folds left: `reducer(reducer(init, arr[0]), arr[1])...`
    #[async_recursion]
    async fn array_reduce(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        (args.len() == 3).then_some(()).ok_or_else(|| {
            Error::runtime(
                span,
                format!("Array.reduce expects 3 arguments, got {}", args.len()),
            )
        })?;

        let reducer_id = *args.first().ok_or_else(|| {
            Error::runtime(span, "Array.reduce: missing reducer")
        })?;
        let init_id = *args.get(1).ok_or_else(|| {
            Error::runtime(span, "Array.reduce: missing initial value")
        })?;
        let arr_id = *args.get(2).ok_or_else(|| {
            Error::runtime(span, "Array.reduce: missing array")
        })?;

        let (_, elems) = self.arena.get_array(arr_id).ok_or_else(|| {
            Error::type_err(span, "Array.reduce expects Array")
        })?;

        self.array_reduce_rec(reducer_id, init_id, &elems, span)
            .await
    }

    /// Recursive helper for `Array.reduce`.
    #[async_recursion]
    async fn array_reduce_rec(
        &mut self,
        reducer_id: ValueId,
        acc_id: ValueId,
        elems: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => self.arena.get(acc_id).cloned().ok_or_else(|| {
                Error::runtime(span, "Array.reduce: invalid accumulator")
            }),
            Some((head, tail)) => {
                let new_acc = self
                    .invoke_callable(reducer_id, &[acc_id, *head], span)
                    .await?;
                self.array_reduce_rec(reducer_id, new_acc, tail, span).await
            }
        }
    }

    /// Invoke a callable value (closure/function) with arguments.
    ///
    /// Used by higher-order primitives to call user-provided functions.
    #[async_recursion]
    async fn invoke_callable(
        &mut self,
        callee_id: ValueId,
        args: &[ValueId],
        span: Span,
    ) -> Result<ValueId> {
        let callee = self
            .arena
            .get(callee_id)
            .cloned()
            .ok_or_else(|| Error::runtime(span, "invalid callable"))?;

        match callee {
            Value::Closure {
                params,
                ret,
                body,
                env,
            } => {
                let result = self
                    .invoke_closure(&params, ret, body, &env, args, span)
                    .await?;
                Ok(self.arena.add(result, span))
            }
            Value::Function {
                params, ret, body, ..
            } => {
                let result = self
                    .invoke_function(&params, ret, body, args, span)
                    .await?;
                Ok(self.arena.add(result, span))
            }
            Value::ModuleFn { path } => {
                let result = self.invoke_module_fn(&path, args, span).await?;
                Ok(self.arena.add(result, span))
            }
            _ => Err(Error::type_err(
                span,
                format!(
                    "expected function, got {}",
                    callee.type_name(&self.registry, &self.type_exprs)
                ),
            )),
        }
    }

    /// Invoke a primitive with pre-evaluated arguments.
    #[async_recursion]
    async fn invoke_primitive(
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
                let vals = self.eval_args(args).await?;
                self.invoke_module_fn(&path, &vals, span).await
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

    /// Call a named function with expression arguments.
    #[async_recursion]
    pub(super) async fn call_function(
        &mut self,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ExprId],
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
            let vals = self.eval_args(args).await?;
            self.invoke_function(params, ret, body, &vals, span).await
        }
    }

    /// Call a closure with expression arguments.
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
            let vals = self.eval_args(args).await?;
            self.invoke_closure(params, ret, body, env, &vals, span)
                .await
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
