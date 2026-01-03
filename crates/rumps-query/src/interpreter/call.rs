//! Function and closure calling.

use async_recursion::async_recursion;
use smallvec::{smallvec, SmallVec};

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::env::{PrimCtx, PrimFn};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{CapturedEnv, TypeExprId, TypeId, Value, ValueId};
use crate::{Result, Span};

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
            // Type checker guarantees rhs is callable
            _ => typechecked!("|>", "Callable"),
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
        // Type checker guarantees arity matches
        if params.len() != args.len() {
            typechecked!("closure call", "correct arity")
        }

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
        result.and_then(|val| self.check_return_type(val, ret))
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
        // Type checker guarantees arity matches
        if params.len() != args.len() {
            typechecked!("function call", "correct arity")
        }

        // Push new scope for parameters
        self.env.scopes.push();
        self.bind_params(params, args, span)?;

        // Evaluate body
        let result = self.eval(body).await;

        // Pop parameter scope
        self.env.scopes.pop();

        // Validate return type if annotated
        result.and_then(|val| self.check_return_type(val, ret))
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
        let callee_expr = self
            .ast
            .get_expr(callee)
            .cloned()
            .unwrap_or_else(|| invariant!("ExprId in AST"));

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
            // Type checker / resolver guarantees function exists
            (None, None) => typechecked!("call_by_name", "defined function"),
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
            io: &mut self.io,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        // Look up and clone the result value
        Ok(self
            .arena
            .get(result_id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena")))
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
            ["Array", "foreach"] => self.array_foreach(args, span).await,
            ["Array", "sort-by"] => self.array_sort_by(args, span).await,
            ["Array", "zip-with"] => self.array_zip_with(args, span).await,
            ["Option", "map"] => self.option_map(args, span).await,
            ["Result", "map"] => self.result_map(args, span).await,
            ["Result", "map-err"] => self.result_map_err(args, span).await,
            _ => {
                // Regular module function; resolver guarantees it exists
                let prim =
                    self.env.get_module_fn(&path_refs).copied().unwrap_or_else(
                        || {
                            typechecked!(
                                "invoke_module_fn",
                                "known module function"
                            )
                        },
                    );

                self.invoke_primitive(prim, args, span).await
            }
        }
    }

    /// `Array.map(fn, arr) -> Array`
    ///
    /// Applies `fn` to each element of `arr` (or range), returning a new array.
    /// Validates that all results have the same type (homogeneous array).
    #[async_recursion]
    async fn array_map(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.map", "2 args"));
        let iterable_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.map", "2 args"));

        // Match on reference; only clone the `SmallVec`, not the whole `Value`
        match self.arena.get(iterable_id) {
            Some(Value::Array(_, elems)) => {
                let elems = elems.clone();
                self.array_map_rec(fn_id, &elems, SmallVec::new(), None, span)
                    .await
            }
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => self.range_map(fn_id, *start, *end, *inclusive, span).await,
            // Type checker guarantees iterable is Array or Range
            Some(_) => typechecked!("Array.map", "Iterable"),
            None => typechecked!("Array.map", "valid iterable"),
        }
    }

    /// Map over a range without allocating the entire range.
    #[async_recursion]
    async fn range_map(
        &mut self,
        fn_id: ValueId,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> Result<Value> {
        let actual_end = if inclusive { end + 1 } else { end };
        self.range_map_rec(
            fn_id,
            start,
            actual_end,
            SmallVec::new(),
            None,
            span,
        )
        .await
    }

    /// Type checker guarantees mapper function produces homogeneous results.
    #[async_recursion]
    async fn range_map_rec(
        &mut self,
        fn_id: ValueId,
        current: i64,
        end: i64,
        acc: SmallVec<[ValueId; 4]>,
        first_ty: Option<TypeId>,
        span: Span,
    ) -> Result<Value> {
        if current >= end {
            let elem_ty = first_ty
                .map(|ty| self.type_exprs.named(ty))
                .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
            Ok(Value::Array(elem_ty, acc))
        } else {
            let int_val = Value::Int(current);
            let int_id = self.arena.add(int_val, span);

            let result = self.invoke_callable(fn_id, &[int_id], span).await?;
            let result_ty = self
                .arena
                .base_type_of(result, &self.type_exprs)
                .unwrap_or(TypeId::UNKNOWN);

            let mut new_acc = acc;
            new_acc.push(result);
            self.range_map_rec(
                fn_id,
                current + 1,
                end,
                new_acc,
                first_ty.or(Some(result_ty)),
                span,
            )
            .await
        }
    }

    /// Recursive helper for `Array.map`.
    ///
    /// Type checker guarantees mapper function produces homogeneous results.
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
                    .unwrap_or(TypeId::UNKNOWN);

                let mut new_acc = acc;
                new_acc.push(result);
                self.array_map_rec(
                    fn_id,
                    tail,
                    new_acc,
                    first_ty.or(Some(result_ty)),
                    span,
                )
                .await
            }
        }
    }

    /// `Array.filter(predicate, arr) -> Array`
    ///
    /// Returns a new array containing only elements for which `predicate`
    /// returns `true`. Also works with Range values.
    #[async_recursion]
    async fn array_filter(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let pred_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.filter", "2 args"));
        let iterable_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.filter", "2 args"));

        // Match on reference; only clone the `SmallVec`, not the whole `Value`
        match self.arena.get(iterable_id) {
            Some(Value::Array(elem_ty, elems)) => {
                let (elem_ty, elems) = (*elem_ty, elems.clone());
                self.array_filter_rec(
                    pred_id,
                    elem_ty,
                    &elems,
                    SmallVec::new(),
                    span,
                )
                .await
            }
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => {
                self.range_filter(pred_id, *start, *end, *inclusive, span)
                    .await
            }
            // Type checker guarantees iterable is Array or Range
            Some(_) => typechecked!("Array.filter", "Iterable"),
            None => typechecked!("Array.filter", "valid iterable"),
        }
    }

    /// Filter a range without allocating the entire range.
    #[async_recursion]
    async fn range_filter(
        &mut self,
        pred_id: ValueId,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> Result<Value> {
        let actual_end = if inclusive { end + 1 } else { end };
        let int_ty = self.type_exprs.named(TypeId::INT);
        self.range_filter_rec(
            pred_id,
            start,
            actual_end,
            int_ty,
            SmallVec::new(),
            span,
        )
        .await
    }

    /// Type checker guarantees predicate returns `Bool`.
    #[async_recursion]
    async fn range_filter_rec(
        &mut self,
        pred_id: ValueId,
        current: i64,
        end: i64,
        elem_ty: TypeExprId,
        acc: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        if current >= end {
            Ok(Value::Array(elem_ty, acc))
        } else {
            let int_val = Value::Int(current);
            let int_id = self.arena.add(int_val, span);

            let result = self.invoke_callable(pred_id, &[int_id], span).await?;
            let result_val =
                self.arena.get(result).cloned().unwrap_or_else(|| {
                    typechecked!("Array.filter", "valid result")
                });

            let keep = match result_val {
                Value::Bool(b) => b,
                _ => typechecked!("Array.filter predicate", "Bool"),
            };
            let mut new_acc = acc;
            if keep {
                new_acc.push(int_id);
            }
            self.range_filter_rec(
                pred_id,
                current + 1,
                end,
                elem_ty,
                new_acc,
                span,
            )
            .await
        }
    }

    /// Recursive helper for `Array.filter`.
    ///
    /// Type checker guarantees predicate returns `Bool`.
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
                    self.arena.get(result).cloned().unwrap_or_else(|| {
                        typechecked!("Array.filter", "valid result")
                    });

                let keep = match result_val {
                    Value::Bool(b) => b,
                    _ => typechecked!("Array.filter predicate", "Bool"),
                };
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
    /// Also works with Range values.
    #[async_recursion]
    async fn array_reduce(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let reducer_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.reduce", "3 args"));
        let init_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.reduce", "3 args"));
        let iterable_id = *args
            .get(2)
            .unwrap_or_else(|| typechecked!("Array.reduce", "3 args"));

        // Match on reference; only clone the `SmallVec`, not the whole `Value`
        match self.arena.get(iterable_id) {
            Some(Value::Array(_, elems)) => {
                let elems = elems.clone();
                self.array_reduce_rec(reducer_id, init_id, &elems, span)
                    .await
            }
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => {
                self.range_reduce(
                    reducer_id, init_id, *start, *end, *inclusive, span,
                )
                .await
            }
            // Type checker guarantees iterable is Array or Range
            Some(_) => typechecked!("Array.reduce", "Iterable"),
            None => typechecked!("Array.reduce", "valid iterable"),
        }
    }

    /// Reduce over a range without allocating the entire range.
    #[async_recursion]
    async fn range_reduce(
        &mut self,
        reducer_id: ValueId,
        init_id: ValueId,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> Result<Value> {
        let actual_end = if inclusive { end + 1 } else { end };
        self.range_reduce_rec(reducer_id, init_id, start, actual_end, span)
            .await
    }

    #[async_recursion]
    async fn range_reduce_rec(
        &mut self,
        reducer_id: ValueId,
        acc_id: ValueId,
        current: i64,
        end: i64,
        span: Span,
    ) -> Result<Value> {
        if current >= end {
            Ok(self.arena.get(acc_id).cloned().unwrap_or_else(|| {
                typechecked!("Array.reduce", "valid accumulator")
            }))
        } else {
            let int_val = Value::Int(current);
            let int_id = self.arena.add(int_val, span);

            let new_acc = self
                .invoke_callable(reducer_id, &[acc_id, int_id], span)
                .await?;
            self.range_reduce_rec(reducer_id, new_acc, current + 1, end, span)
                .await
        }
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
            None => Ok(self.arena.get(acc_id).cloned().unwrap_or_else(|| {
                typechecked!("Array.reduce", "valid accumulator")
            })),
            Some((head, tail)) => {
                let new_acc = self
                    .invoke_callable(reducer_id, &[acc_id, *head], span)
                    .await?;
                self.array_reduce_rec(reducer_id, new_acc, tail, span).await
            }
        }
    }

    /// `Array.foreach(fn, arr) -> Unit`
    ///
    /// Invokes `fn` on each element of `arr` (or range) for side effects.
    /// The callback must return `Unit`. Returns `Unit`.
    #[async_recursion]
    async fn array_foreach(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Array.foreach", "2 args"));
        let iterable_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Array.foreach", "2 args"));

        match self.arena.get(iterable_id) {
            Some(Value::Array(_, elems)) => {
                let elems = elems.clone();
                self.array_foreach_rec(fn_id, &elems, span).await
            }
            Some(Value::Range {
                start,
                end,
                inclusive,
            }) => {
                self.range_foreach(fn_id, *start, *end, *inclusive, span)
                    .await
            }
            // Type checker guarantees iterable is Array or Range
            Some(_) => typechecked!("Array.foreach", "Iterable"),
            None => typechecked!("Array.foreach", "valid iterable"),
        }
    }

    /// Foreach over a range without allocating the entire range.
    #[async_recursion]
    async fn range_foreach(
        &mut self,
        fn_id: ValueId,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> Result<Value> {
        let actual_end = if inclusive { end + 1 } else { end };
        self.range_foreach_rec(fn_id, start, actual_end, span).await
    }

    #[async_recursion]
    async fn range_foreach_rec(
        &mut self,
        fn_id: ValueId,
        current: i64,
        end: i64,
        span: Span,
    ) -> Result<Value> {
        if current >= end {
            Ok(Value::Unit)
        } else {
            let int_val = Value::Int(current);
            let int_id = self.arena.add(int_val, span);
            self.invoke_callable(fn_id, &[int_id], span).await?;
            self.range_foreach_rec(fn_id, current + 1, end, span).await
        }
    }

    /// Recursive helper for `Array.foreach`.
    #[async_recursion]
    async fn array_foreach_rec(
        &mut self,
        fn_id: ValueId,
        elems: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        match elems.split_first() {
            None => Ok(Value::Unit),
            Some((head, tail)) => {
                self.invoke_callable(fn_id, &[*head], span).await?;
                self.array_foreach_rec(fn_id, tail, span).await
            }
        }
    }

    /// `Array.sort-by(cmp, arr) -> Array[T]`
    ///
    /// Sorts array using a comparator function that returns `Ordering`.
    #[async_recursion]
    async fn array_sort_by(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let cmp_fn = args[0];
        let arr_id = args[1];

        let (elem_ty, elems) = self
            .arena
            .get_array(arr_id)
            .unwrap_or_else(|| typechecked!("Array.sort-by", "Array"));

        let vals: Vec<ValueId> = elems.iter().copied().collect();

        // Recursive merge sort to allow async comparisons
        let result = self.array_sort_by_rec(cmp_fn, &vals, span).await?;

        Ok(Value::Array(elem_ty, result))
    }

    /// `Array.zip-with(f, a, b) -> Array[V]`
    ///
    /// Combines two arrays with a function.
    #[async_recursion]
    async fn array_zip_with(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let fn_id = args[0];
        let arr_a_id = args[1];
        let arr_b_id = args[2];

        let (_, elems_a) = self
            .arena
            .get_array(arr_a_id)
            .unwrap_or_else(|| typechecked!("Array.zip-with", "Array"));

        let (_, elems_b) = self
            .arena
            .get_array(arr_b_id)
            .unwrap_or_else(|| typechecked!("Array.zip-with", "Array"));

        // Zip and apply function
        let pairs: Vec<(ValueId, ValueId)> = elems_a
            .iter()
            .zip(elems_b.iter())
            .map(|(a, b)| (*a, *b))
            .collect();

        self.array_zip_with_rec(fn_id, &pairs, SmallVec::new(), None, span)
            .await
    }

    /// Recursive helper for `Array.zip-with`.
    #[async_recursion]
    async fn array_zip_with_rec(
        &mut self,
        fn_id: ValueId,
        pairs: &[(ValueId, ValueId)],
        acc: SmallVec<[ValueId; 4]>,
        first_ty: Option<TypeId>,
        span: Span,
    ) -> Result<Value> {
        match pairs.split_first() {
            None => {
                let elem_ty = first_ty
                    .map(|ty| self.type_exprs.named(ty))
                    .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
                Ok(Value::Array(elem_ty, acc))
            }
            Some(((a, b), tail)) => {
                let result =
                    self.invoke_callable(fn_id, &[*a, *b], span).await?;
                let result_ty = self
                    .arena
                    .base_type_of(result, &self.type_exprs)
                    .unwrap_or(TypeId::UNKNOWN);

                let mut new_acc = acc;
                new_acc.push(result);
                self.array_zip_with_rec(
                    fn_id,
                    tail,
                    new_acc,
                    first_ty.or(Some(result_ty)),
                    span,
                )
                .await
            }
        }
    }

    /// Recursive merge sort for `Array.sort-by`.
    #[async_recursion]
    async fn array_sort_by_rec(
        &mut self,
        cmp_fn: ValueId,
        vals: &[ValueId],
        span: Span,
    ) -> Result<SmallVec<[ValueId; 4]>> {
        if vals.len() <= 1 {
            Ok(vals.iter().copied().collect())
        } else {
            let mid = vals.len() / 2;
            let (left, right) = vals.split_at(mid);
            let sorted_left =
                self.array_sort_by_rec(cmp_fn, left, span).await?;
            let sorted_right =
                self.array_sort_by_rec(cmp_fn, right, span).await?;
            self.array_merge_sorted(cmp_fn, &sorted_left, &sorted_right, span)
                .await
        }
    }

    /// Merge two sorted slices using async comparator.
    #[async_recursion]
    async fn array_merge_sorted(
        &mut self,
        cmp_fn: ValueId,
        left: &[ValueId],
        right: &[ValueId],
        span: Span,
    ) -> Result<SmallVec<[ValueId; 4]>> {
        match (left.split_first(), right.split_first()) {
            (None, None) => Ok(SmallVec::new()),
            (Some((l, ls)), None) => {
                let mut result: SmallVec<[ValueId; 4]> =
                    ls.iter().copied().collect();
                result.insert(0, *l);
                Ok(result)
            }
            (None, Some((r, rs))) => {
                let mut result: SmallVec<[ValueId; 4]> =
                    rs.iter().copied().collect();
                result.insert(0, *r);
                Ok(result)
            }
            (Some((l, ls)), Some((r, rs))) => {
                let ord = self.invoke_callable(cmp_fn, &[*l, *r], span).await?;
                let ord_val = self
                    .arena
                    .get(ord)
                    .unwrap_or_else(|| invariant!("ValueId in arena"));

                // Check if it's Ordering.Gt (take right first if left > right)
                let is_gt = matches!(ord_val, Value::Tagged(ty, 2, _)
                    if self.type_exprs.base_type(*ty).is_some_and(|t| t == TypeId::ORDERING));

                if is_gt {
                    let mut rest =
                        self.array_merge_sorted(cmp_fn, left, rs, span).await?;
                    rest.insert(0, *r);
                    Ok(rest)
                } else {
                    let mut rest = self
                        .array_merge_sorted(cmp_fn, ls, right, span)
                        .await?;
                    rest.insert(0, *l);
                    Ok(rest)
                }
            }
        }
    }

    /// `Option.map(opt, fn) -> Option`
    ///
    /// If `opt` is `Some(v)`, applies `fn` to `v` and wraps result in `Some`.
    /// If `opt` is `None`, returns `None`.
    #[async_recursion]
    async fn option_map(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let opt_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Option.map", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Option.map", "2 args"));

        let opt = self
            .arena
            .get(opt_id)
            .unwrap_or_else(|| typechecked!("Option.map", "valid value"));

        let is_some = opt.is_some(&self.type_exprs);
        let is_none = opt.is_none(&self.type_exprs);

        match (is_some, is_none) {
            (true, false) => {
                // Option.Some(v) - apply fn and wrap in Some
                let inner = match opt {
                    Value::Tagged(_, _, payloads) => {
                        *payloads.first().unwrap_or_else(|| {
                            typechecked!("Option.Some", "payload")
                        })
                    }
                    _ => typechecked!("Option.Some", "Tagged"),
                };

                let result_id =
                    self.invoke_callable(fn_id, &[inner], span).await?;
                self.arena.get(result_id).unwrap_or_else(|| {
                    typechecked!("Option.map", "valid result")
                });

                // Wrap in Some with appropriate type
                let result_ty = self
                    .arena
                    .base_type_of(result_id, &self.type_exprs)
                    .unwrap_or(TypeId::UNKNOWN);
                let val_ty = self.type_exprs.named(result_ty);
                let opt_ty =
                    self.type_exprs.app(TypeId::OPTION, smallvec![val_ty]);
                Ok(Value::some(opt_ty, result_id))
            }
            (false, true) => {
                // Option.None - return None
                let unknown = self.type_exprs.named(TypeId::UNKNOWN);
                let opt_ty =
                    self.type_exprs.app(TypeId::OPTION, smallvec![unknown]);
                Ok(Value::none(opt_ty))
            }
            // Type checker guarantees arg is Option
            _ => typechecked!("Option.map", "Option"),
        }
    }

    /// `Result.map(res, fn) -> Result`
    ///
    /// If `res` is `Ok(v)`, applies `fn` to `v` and wraps result in `Ok`.
    /// If `res` is `Err(e)`, returns `Err(e)` unchanged.
    #[async_recursion]
    async fn result_map(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let res_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Result.map", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Result.map", "2 args"));

        let res = self
            .arena
            .get(res_id)
            .unwrap_or_else(|| typechecked!("Result.map", "valid value"));

        let is_ok = res.is_ok(&self.type_exprs);
        let is_err = res.is_err(&self.type_exprs);

        match (is_ok, is_err) {
            (true, false) => {
                // Result.Ok(v) - apply fn and wrap in Ok
                let inner = match res {
                    Value::Tagged(_, _, payloads) => {
                        *payloads.first().unwrap_or_else(|| {
                            typechecked!("Result.Ok", "payload")
                        })
                    }
                    _ => typechecked!("Result.Ok", "Tagged"),
                };

                let result_id =
                    self.invoke_callable(fn_id, &[inner], span).await?;
                self.arena.get(result_id).unwrap_or_else(|| {
                    typechecked!("Result.map", "valid result")
                });

                // Wrap in Ok with appropriate type
                let result_ty = self
                    .arena
                    .base_type_of(result_id, &self.type_exprs)
                    .unwrap_or(TypeId::UNKNOWN);
                let val_ty = self.type_exprs.named(result_ty);
                let unknown = self.type_exprs.named(TypeId::UNKNOWN);
                let res_ty = self
                    .type_exprs
                    .app(TypeId::RESULT, smallvec![val_ty, unknown]);
                Ok(Value::ok(res_ty, result_id))
            }
            (false, true) => {
                // Result.Err(e) - return unchanged (clone needed for passthrough)
                Ok(res.clone())
            }
            // Type checker guarantees arg is Result
            _ => typechecked!("Result.map", "Result"),
        }
    }

    /// `Result.map-err(res, fn) -> Result`
    ///
    /// If `res` is `Err(e)`, applies `fn` to `e` and wraps result in `Err`.
    /// If `res` is `Ok(v)`, returns `Ok(v)` unchanged.
    #[async_recursion]
    async fn result_map_err(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let res_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));

        let res = self
            .arena
            .get(res_id)
            .unwrap_or_else(|| typechecked!("Result.map-err", "valid value"));

        let is_ok = res.is_ok(&self.type_exprs);
        let is_err = res.is_err(&self.type_exprs);

        match (is_ok, is_err) {
            (true, false) => {
                // Result.Ok(v) - return unchanged (clone needed for passthrough)
                Ok(res.clone())
            }
            (false, true) => {
                // Result.Err(e) - apply fn and wrap in Err
                let inner = match res {
                    Value::Tagged(_, _, payloads) => {
                        *payloads.first().unwrap_or_else(|| {
                            typechecked!("Result.Err", "payload")
                        })
                    }
                    _ => typechecked!("Result.Err", "Tagged"),
                };

                let result_id =
                    self.invoke_callable(fn_id, &[inner], span).await?;
                self.arena.get(result_id).unwrap_or_else(|| {
                    typechecked!("Result.map-err", "valid result")
                });

                // Wrap in Err with appropriate type
                let result_ty = self
                    .arena
                    .base_type_of(result_id, &self.type_exprs)
                    .unwrap_or(TypeId::UNKNOWN);
                let err_ty = self.type_exprs.named(result_ty);
                let unknown = self.type_exprs.named(TypeId::UNKNOWN);
                let res_ty = self
                    .type_exprs
                    .app(TypeId::RESULT, smallvec![unknown, err_ty]);
                Ok(Value::err(res_ty, result_id))
            }
            // Type checker guarantees arg is Result
            _ => typechecked!("Result.map-err", "Result"),
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
            .unwrap_or_else(|| invariant!("ValueId in arena"));

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
            // Type checker guarantees callee is callable
            _ => typechecked!("invoke_callable", "Callable"),
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
            io: &mut self.io,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        Ok(self
            .arena
            .get(result_id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena")))
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
            // FOREVER continuation: calling it signals loop continuation
            Value::ForeverContinuation => {
                // Type checker guarantees exactly one argument
                let new_state_expr = args
                    .first()
                    .unwrap_or_else(|| typechecked!("continuation", "1 arg"));
                let new_state = self.eval(*new_state_expr).await?;
                let state_id = self.arena.add(new_state, span);
                Ok(Value::LoopContinue(state_id))
            }
            // Type checker guarantees callee is callable
            _ => typechecked!("call", "Callable"),
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
        // Type checker guarantees arity matches
        if params.len() != args.len() {
            typechecked!("call_function", "correct arity")
        }
        let vals = self.eval_args(args).await?;
        self.invoke_function(params, ret, body, &vals, span).await
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
        // Type checker guarantees arity matches
        if params.len() != args.len() {
            typechecked!("call_closure", "correct arity")
        }
        let vals = self.eval_args(args).await?;
        self.invoke_closure(params, ret, body, env, &vals, span)
            .await
    }

    /// Check that a return value matches the declared return type.
    fn check_return_type(
        &mut self,
        val: Value,
        ret: Option<TypeExprId>,
    ) -> Result<Value> {
        if let Some(expected_ty) = ret {
            if self.value_matches_type_expr(&val, expected_ty) {
                Ok(val)
            } else {
                typechecked!("return type", "matches declaration")
            }
        } else {
            Ok(val)
        }
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
    /// Provides detailed error messages, especially for object alias types.
    fn validate_param(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
        param_name: StringId,
        span: Span,
    ) -> Result<()> {
        let pname = self.arena.get_str(param_name).unwrap_or("?").to_owned();

        // Check if this is an object alias type and get resolved fields
        let resolved_fields = self.resolve_object_alias_fields(expected_ty);

        if let Some(fields) = resolved_fields {
            // Object alias type: validate with detailed errors
            match val {
                Value::Object(obj) => self.validate_object_fields(
                    obj,
                    &fields,
                    span,
                    Some(&pname),
                ),
                _ => typechecked!("parameter type", "Object"),
            }
        } else if self.value_matches_type_expr(val, expected_ty) {
            Ok(())
        } else {
            typechecked!("parameter type", "matches declaration")
        }
    }
}
