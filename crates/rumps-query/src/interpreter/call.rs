//! Function and closure calling.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{Expr, ExprId};
use crate::env::{PrimCtx, PrimFn};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::value::{CapturedEnv, FunctionDef, TypeExprId, Value, ValueId};
use crate::{ClassId, Result, Span};

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

        // Handle PartialApp via resolve to avoid nesting
        if let Value::PartialApp { callee, ref bound } = right {
            self.resolve_partial_app(callee, bound, &[arg_id], span)
                .await
        // `right.clone()` is unavoidable here: `maybe_partial_app` takes
        // ownership, but the fallthrough `match right` below also consumes
        // `right`. In practice the clone is cheap since callable values
        // hold `SmallVec` params and (for closures) an `Arc<CapturedEnv>`.
        } else if let Some(partial) =
            self.maybe_partial_app(right.clone(), &[arg_id], span)
        {
            Ok(partial)
        } else {
            // Full application (arity == 1); dispatch as before
            match right {
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
                Value::Function {
                    params, ret, body, ..
                } => {
                    self.invoke_function(&params, ret, body, &[arg_id], span)
                        .await
                }
                Value::ModuleFn { path } => {
                    self.invoke_module_fn(&path, &[arg_id], span).await
                }
                Value::ClassMethodFn {
                    class,
                    method,
                    expr_id,
                } => {
                    self.invoke_class_method_fn(
                        class,
                        method,
                        expr_id,
                        &[arg_id],
                        span,
                    )
                    .await
                }
                // Type checker guarantees rhs is callable
                _ => typechecked!("|>", "Callable"),
            }
        }
    }

    /// Invoke a closure with pre-evaluated arguments.
    #[async_recursion]
    pub(super) async fn invoke_closure(
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
        result.and_then(|val| self.check_return_type(val, ret, span))
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
        result.and_then(|val| self.check_return_type(val, ret, span))
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
            Expr::Var(ref name) => self.call_by_name(*name, args, span).await,
            // Check if this is a variant constructor for a user-defined type
            Expr::Field(base_id, ref var_name) => {
                let maybe_variant =
                    self.ast.get_expr(base_id).and_then(|e| match e {
                        Expr::Var(ty_name) => {
                            let qn = QualifiedName::local(*ty_name);
                            self.registry.lookup(&qn).and_then(|type_id| {
                                self.registry
                                    .lookup_variant(type_id, *var_name)
                                    .map(|_| (qn, *var_name))
                            })
                        }
                        _ => None,
                    });

                if let Some((ty_qn, var_id)) = maybe_variant {
                    // Handle as variant constructor
                    self.variant(&ty_qn, var_id, args, span).await
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
    /// 1. Named functions (from fun definitions)
    /// 2. Lexical scope (may be a bound closure)
    ///
    /// Note: Built-in module functions (e.g., `Object.keys`) are resolved at
    /// parse time by `resolve.rs` and become `Expr::Path` nodes.
    #[async_recursion]
    async fn call_by_name(
        &mut self,
        name: StringId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        // Clone function def to avoid borrow issues with async
        let func_def = self.functions.get(&name).cloned();
        let scope_val = func_def.as_ref().map_or_else(
            || {
                self.env
                    .scopes
                    .lookup(name)
                    .and_then(|val_id| self.arena.get(val_id).cloned())
            },
            |_| None,
        );

        match (func_def, scope_val) {
            (Some(def), _) => {
                self.call_function(
                    name,
                    &def.params,
                    def.ret,
                    def.body,
                    args,
                    span,
                )
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
    /// Iterable functions (`Iter.map`, `Iter.filter`, `Iter.reduce`) are
    /// higher-order and need special handling since they invoke closures.
    #[async_recursion]
    pub(super) async fn invoke_module_fn(
        &mut self,
        path: &[StringId],
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        // Higher-order functions (those that invoke closures/functions passed as
        // arguments) are dispatched via `module_hofs` registry. The `PrimFn`
        // signature only receives values; it has no access to the interpreter's
        // closure invocation machinery (`invoke_callable`). See `primitives.rs`
        // module docs for details.
        if let Some((hof, result)) = self.module_hofs.lookup(path) {
            let v = self.run_hof_trampoline(hof, args, span).await?;
            // Some HoFs (e.g. `foreach`) delegate to another HoF but discard
            // the produced value, evaluating to `Unit` instead.
            Ok(match result {
                super::hof::HofResult::Keep => v,
                super::hof::HofResult::Discard => Value::Unit,
            })
        } else if let Some(fn_def) = self.env.get_user_module_fn(path).cloned()
        {
            // User-defined module function
            self.invoke_user_module_fn(path, &fn_def, args, span).await
        } else {
            // Builtin sync module function; resolver guarantees it exists
            let prim =
                self.env.get_module_fn(path).copied().unwrap_or_else(|| {
                    typechecked!("invoke_module_fn", "known module function")
                });

            self.invoke_primitive(prim, args, span).await
        }
    }

    /// Invoke a user-defined module function.
    ///
    /// Binds all sibling functions and constants at call time, enabling
    /// mutual recursion between module functions. This is different from
    /// closures which capture their environment at creation time.
    #[async_recursion]
    async fn invoke_user_module_fn(
        &mut self,
        path: &[StringId],
        fn_def: &FunctionDef,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        // Type checker guarantees arity matches
        if fn_def.params.len() != args.len() {
            typechecked!("user module call", "correct arity")
        }

        // Get the module path (all but last segment)
        let mod_path =
            path.split_last().map(|(_, rest)| rest).unwrap_or_else(|| {
                typechecked!("module path", "at least 2 segments")
            });

        // Look up the module to bind siblings
        let module = self
            .env
            .get_user_module(mod_path)
            .cloned()
            .unwrap_or_else(|| typechecked!("user module", "exists"));

        // Push a new scope for this function call
        self.env.scopes.push();

        // Bind all sibling functions as Value::Function so they can be called
        module.functions.iter().for_each(|(&name, sibling)| {
            let val = Value::Function {
                name: sibling.name,
                params: sibling.params.clone(),
                ret: sibling.ret,
                body: sibling.body,
            };
            let val_id = self.arena.add(val, span);
            self.env.scopes.bind(name, val_id);
        });

        // Bind all sibling constants
        module.constants.iter().for_each(|(&name, &const_id)| {
            self.env.scopes.bind(name, const_id);
        });

        // Bind parameters
        self.bind_params(&fn_def.params, args, span)?;

        // Evaluate body
        let result = self.eval(fn_def.body).await;

        // Pop the scope
        self.env.scopes.pop();

        // Validate return type if annotated
        result.and_then(|val| self.check_return_type(val, fn_def.ret, span))
    }

    /// Evaluate a class method expression.
    ///
    /// Parses the class name, evaluates arguments, and dispatches to the
    /// appropriate class method.
    #[async_recursion]
    pub(super) async fn class_method_expr(
        &mut self,
        expr_id: ExprId,
        class: StringId,
        method: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> Result<Value> {
        let arg_ids = self.eval_args(args).await?;

        let cmf = Value::ClassMethodFn {
            class,
            method,
            expr_id: Some(expr_id),
        };
        if let Some(partial) = self.maybe_partial_app(cmf, &arg_ids, span) {
            Ok(partial)
        } else {
            let kind =
                self.class_registry.lookup_by_name(class).unwrap_or_else(
                    || typechecked!("class method class", "known class"),
                );
            self.dispatch_class_method(
                Some(expr_id),
                kind,
                method,
                &arg_ids,
                span,
            )
            .await
        }
    }

    /// Invoke a class method from a `ClassMethodFn` value.
    ///
    /// Looks up class/method strings and dispatches to the class method.
    ///
    /// The `expr_id` is the expression ID of the `ClassMethodRef` that created
    /// this value; needed for convert methods to look up target types.
    #[async_recursion]
    async fn invoke_class_method_fn(
        &mut self,
        class: StringId,
        method: StringId,
        expr_id: Option<ExprId>,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let kind =
            self.class_registry
                .lookup_by_name(class)
                .unwrap_or_else(|| {
                    typechecked!("invoke_class_method_fn", "known class")
                });

        self.dispatch_class_method(expr_id, kind, method, args, span)
            .await
    }

    /// Dispatch a class method call.
    ///
    /// Unified entry point for all class methods. Checks for user-defined instances
    /// first; falls back to auto-derivation for Union/Newtype, then builtin dispatch.
    ///
    /// User instance lookup:
    /// 1. Check `instance_calls` map (for newtype/union where type isn't in value)
    /// 2. Check first arg if `Value::Tagged`, `Value::Union`, or `Value::Newtype`
    /// 3. Look up user instance by (class, type_id)
    /// 4. If found, dispatch to generated function; else auto-derive for Union/Newtype
    ///
    /// The `expr_id` parameter is used by nullary methods (like `Monoid:identity`)
    /// to look up the inferred type from `mempty_types`, and for user instance
    /// dispatch with newtype/union types.
    #[async_recursion]
    pub(super) async fn dispatch_class_method(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        // Check for user-defined instance dispatch.
        // Priority: `instance_calls` map > `Value::Tagged` (NOT `Union`/`Newtype`)
        //
        // IMPORTANT: Only `Value::Tagged` is used to infer user instances from
        // the value itself. `Union`/`Newtype` values should ONLY use user instances
        // when the typechecker explicitly marked the call site in `instance_calls`.
        // This prevents infinite recursion inside user class implementations.
        let user_type_id = expr_id
            .and_then(|id| self.instance_calls.get(&id).copied())
            .or_else(|| {
                args.first()
                    .and_then(|id| self.arena.get(*id))
                    .and_then(|v| match v {
                        Value::Tagged(ty_expr, _, _) => {
                            self.type_exprs.base_type(*ty_expr)
                        }
                        _ => None,
                    })
            });

        // Check for resolved parameterized instance function first
        let resolved_fn =
            expr_id.and_then(|id| self.resolved_instance_fns.get(&id).copied());

        // If we have a user type, check for user instance
        if let Some(type_id) = user_type_id {
            if let Some(fn_name) = resolved_fn.or_else(|| {
                self.user_instances.lookup_method(class, type_id, method)
            }) {
                // Dispatch to user-defined instance method
                let func_def = self.functions.get(&fn_name).cloned();
                if let Some(def) = func_def {
                    self.invoke_function(
                        &def.params,
                        def.ret,
                        def.body,
                        args,
                        span,
                    )
                    .await
                } else {
                    // User instance registered but function not found
                    typechecked!("user instance method", "registered function")
                }
            } else {
                // No user instance for this type; auto-derive for Union/Newtype
                self.dispatch_with_auto_derive(
                    expr_id, class, method, args, span,
                )
                .await
            }
        } else {
            // No user type from instance_calls or Tagged.
            // For Union/Newtype, auto-derive (unwrap and dispatch to builtin).
            let is_wrapped =
                args.first().and_then(|id| self.arena.get(*id)).is_some_and(
                    |v| matches!(v, Value::Union(..) | Value::Newtype(..)),
                );

            if is_wrapped {
                self.dispatch_with_auto_derive(
                    expr_id, class, method, args, span,
                )
                .await
            } else {
                self.dispatch_builtin_or_hof(expr_id, class, method, args, span)
                    .await
            }
        }
    }

    /// Dispatch with auto-derivation for Union/Newtype.
    ///
    /// Unwraps ALL Union/Newtype args (recursively) and dispatches to builtin methods.
    /// This allows Union/Newtype values to use the inner type's class implementations.
    #[async_recursion]
    async fn dispatch_with_auto_derive(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let unwrapped_args: SmallVec<[ValueId; 4]> =
            args.iter().map(|&id| self.unwrap_to_inner(id)).collect();

        self.dispatch_builtin_or_hof(
            expr_id,
            class,
            method,
            &unwrapped_args,
            span,
        )
        .await
    }

    /// Recursively unwrap Union/Newtype wrappers until we reach a non-wrapper value.
    ///
    /// This handles nested wrappers correctly, e.g. `Union(_, Newtype(_, inner))`.
    fn unwrap_to_inner(&self, val_id: ValueId) -> ValueId {
        self.arena.get(val_id).map_or(val_id, |v| match v {
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => {
                self.unwrap_to_inner(*inner_id)
            }
            _ => val_id,
        })
    }

    /// Dispatch to builtin class method or async HOF.
    ///
    /// Async wrapper that handles both sync builtin methods and async HOFs.
    #[async_recursion]
    async fn dispatch_builtin_or_hof(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        // Check for HOF first (requires async)
        if let Some(super::class::MethodFn::Hof(f)) =
            self.class_methods.lookup(class, method)
        {
            self.run_hof_trampoline(f, args, span).await
        } else {
            self.dispatch_builtin_class_method(
                expr_id, class, method, args, span,
            )
        }
    }

    /// Dispatch to builtin class method implementations.
    ///
    /// Handles all class methods defined in `class.rs`. Sync methods execute
    /// directly; async HOFs use the trampoline pattern.
    fn dispatch_builtin_class_method(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        use super::class::ClassCtx;

        let val = |i: usize| {
            self.arena
                .get(args[i])
                .cloned()
                .unwrap_or_else(|| invariant!("class method arg in arena"))
        };

        match self.class_methods.lookup(class, method) {
            Some(super::class::MethodFn::Binary(_)) => {
                let left = val(0);
                let right = val(1);
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    type_exprs: &mut self.type_exprs,
                    ty_arena: &self.ty_arena,
                    registry: &self.registry,
                    regex_cache: &self.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_binary(class, method, &mut ctx, &left, &right)
            }
            Some(super::class::MethodFn::Unary(_)) => {
                let v = val(0);
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    type_exprs: &mut self.type_exprs,
                    ty_arena: &self.ty_arena,
                    registry: &self.registry,
                    regex_cache: &self.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_unary(class, method, &mut ctx, &v)
            }
            Some(super::class::MethodFn::Nullary(_)) => {
                let id = expr_id.unwrap_or_else(|| {
                    typechecked!("nullary class method", "expression id")
                });
                let ty_id =
                    self.mempty_types.get(&id).copied().unwrap_or_else(|| {
                        typechecked!("nullary class method", "resolved type")
                    });
                let ty = self.ty_arena.get(ty_id).clone();
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    type_exprs: &mut self.type_exprs,
                    ty_arena: &self.ty_arena,
                    registry: &self.registry,
                    regex_cache: &self.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_nullary(class, method, &mut ctx, &ty)
            }
            Some(super::class::MethodFn::Convert(_)) => {
                let v = val(0);
                let id = expr_id.unwrap_or_else(|| {
                    typechecked!("convert class method", "expression id")
                });
                let ty_id =
                    self.convert_targets.get(&id).copied().unwrap_or_else(
                        || {
                            typechecked!(
                                "convert class method",
                                "resolved target type"
                            )
                        },
                    );
                let ty = self.ty_arena.get(ty_id).clone();
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    type_exprs: &mut self.type_exprs,
                    ty_arena: &self.ty_arena,
                    registry: &self.registry,
                    regex_cache: &self.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_convert(class, method, &mut ctx, &v, &ty)
            }
            Some(super::class::MethodFn::Hof(_)) => {
                // HOFs need async; caller should use dispatch_class_method
                typechecked!("builtin Hof", "async context")
            }
            None => typechecked!("class method", "registered"),
        }
    }

    /// Run a HoF method using a trampoline loop.
    ///
    /// The loop is necessary because Rust lacks tail-call optimization. Without
    /// it, processing a 10,000-element array would create 10,000 stack frames.
    /// The trampoline keeps stack depth O(1) regardless of input size.
    async fn run_hof_trampoline(
        &mut self,
        starter: super::hof::HofMethodFn,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        use super::class::ClassCtx;
        use super::hof::MethodResult;

        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            type_exprs: &mut self.type_exprs,
            ty_arena: &self.ty_arena,
            registry: &self.registry,
            regex_cache: &self.regex_cache,
            span,
        };
        let mut result = starter(&mut ctx, args)?;

        // Trampoline loop; see doc comment for why we use `loop` here.
        loop {
            match result {
                MethodResult::Done(v) => break Ok(v),
                MethodResult::Invoke(cont) => {
                    let call_result = self
                        .invoke_callable(cont.callee, &cont.args, span)
                        .await?;
                    let mut ctx = ClassCtx {
                        arena: &mut self.arena,
                        type_exprs: &mut self.type_exprs,
                        ty_arena: &self.ty_arena,
                        registry: &self.registry,
                        regex_cache: &self.regex_cache,
                        span,
                    };
                    result = super::hof::resume(&mut ctx, cont, call_result)?;
                }
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
            Value::ClassMethodFn {
                class,
                method,
                expr_id,
            } => {
                let result = self
                    .invoke_class_method_fn(class, method, expr_id, args, span)
                    .await?;
                Ok(self.arena.add(result, span))
            }
            Value::PartialApp { callee, bound } => {
                let result = self
                    .resolve_partial_app(callee, &bound, args, span)
                    .await?;
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
                name,
                params,
                ret,
                body,
            } => {
                self.call_function(name, &params, ret, body, args, span)
                    .await
            }
            Value::ModuleFn { path } => {
                let vals = self.eval_args(args).await?;
                if let Some(partial) = self.maybe_partial_app(
                    Value::ModuleFn { path: path.clone() },
                    &vals,
                    span,
                ) {
                    Ok(partial)
                } else {
                    self.invoke_module_fn(&path, &vals, span).await
                }
            }
            Value::ClassMethodFn {
                class,
                method,
                expr_id,
            } => {
                let vals = self.eval_args(args).await?;
                if let Some(partial) = self.maybe_partial_app(
                    Value::ClassMethodFn {
                        class,
                        method,
                        expr_id,
                    },
                    &vals,
                    span,
                ) {
                    Ok(partial)
                } else {
                    self.invoke_class_method_fn(
                        class, method, expr_id, &vals, span,
                    )
                    .await
                }
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
            Value::PartialApp { callee, bound } => {
                let vals = self.eval_args(args).await?;
                self.resolve_partial_app(callee, &bound, &vals, span).await
            }
            // Type checker guarantees callee is callable
            _ => typechecked!("call", "Callable"),
        }
    }

    /// Call a named function with expression arguments.
    #[async_recursion]
    pub(super) async fn call_function(
        &mut self,
        name: StringId,
        params: &[(StringId, Option<TypeExprId>)],
        ret: Option<TypeExprId>,
        body: ExprId,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        if args.len() > params.len() {
            typechecked!("call_function", "correct arity")
        }
        let vals = self.eval_args(args).await?;
        if vals.len() < params.len() {
            let f = Value::Function {
                name,
                params: params.iter().copied().collect(),
                ret,
                body,
            };
            Ok(self.maybe_partial_app(f, &vals, span).unwrap_or_else(|| {
                invariant!("partial app when under-applied")
            }))
        } else {
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
        if args.len() > params.len() {
            typechecked!("call_closure", "correct arity")
        }
        let vals = self.eval_args(args).await?;
        if vals.len() < params.len() {
            let c = Value::Closure {
                params: params.iter().copied().collect(),
                ret,
                body,
                env: env.clone().into(),
            };
            Ok(self.maybe_partial_app(c, &vals, span).unwrap_or_else(|| {
                invariant!("partial app when under-applied")
            }))
        } else {
            self.invoke_closure(params, ret, body, env, &vals, span)
                .await
        }
    }

    /// Determine the expected arity of any callable value.
    ///
    /// Returns `None` if the value is not callable.
    fn callable_arity(&self, v: &Value) -> Option<usize> {
        match v {
            Value::Closure { params, .. } | Value::Function { params, .. } => {
                Some(params.len())
            }
            Value::ModuleFn { path } => self
                .env
                .get_user_module_fn(path)
                .map(|d| d.params.len())
                .or_else(|| {
                    self.env
                        .get_module_fn_type(path)
                        .and_then(|s| s.arity(&self.ty_arena))
                }),
            Value::ClassMethodFn { class, method, .. } => {
                self.class_registry.lookup_by_name(*class).and_then(|kind| {
                    self.class_registry
                        .get(kind)
                        .method(*method, Span::default())
                        .ok()
                        .and_then(|spec| spec.scheme().arity(&self.ty_arena))
                })
            }
            Value::PartialApp { callee, bound } => self
                .arena
                .get(*callee)
                .and_then(|c| self.callable_arity(c))
                .map(|n| n.saturating_sub(bound.len())),
            _ => None,
        }
    }

    /// Check if a call is a partial application.
    ///
    /// If `args` supplies fewer arguments than `callee` expects, returns a
    /// `Value::PartialApp` capturing the callee and bound args. Otherwise
    /// returns `None`, meaning the caller should proceed with full invocation.
    fn maybe_partial_app(
        &mut self,
        callee: Value,
        args: &[ValueId],
        span: Span,
    ) -> Option<Value> {
        let arity = self.callable_arity(&callee)?;
        if args.len() < arity && !args.is_empty() {
            let callee_id = self.arena.add(callee, span);
            Some(Value::PartialApp {
                callee: callee_id,
                bound: args.iter().copied().collect(),
            })
        } else {
            None
        }
    }

    /// Resolve a partial application with additional arguments.
    ///
    /// Combines bound args with new args and either produces another
    /// `PartialApp` (still under-applied) or fully invokes the callee.
    #[async_recursion]
    async fn resolve_partial_app(
        &mut self,
        callee_id: ValueId,
        bound: &[ValueId],
        new_args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let all_args: SmallVec<[ValueId; 4]> =
            bound.iter().chain(new_args.iter()).copied().collect();

        let callee = self
            .arena
            .get(callee_id)
            .cloned()
            .unwrap_or_else(|| invariant!("PartialApp callee in arena"));

        let arity = self
            .callable_arity(&callee)
            .unwrap_or_else(|| invariant!("PartialApp callee is callable"));

        if all_args.len() > arity {
            typechecked!("resolve_partial_app", "args <= arity")
        } else if all_args.len() < arity {
            Ok(Value::PartialApp {
                callee: callee_id,
                bound: all_args,
            })
        } else {
            match callee {
                Value::Closure {
                    params,
                    ret,
                    body,
                    env,
                } => {
                    self.invoke_closure(
                        &params, ret, body, &env, &all_args, span,
                    )
                    .await
                }
                Value::Function {
                    params, ret, body, ..
                } => {
                    self.invoke_function(&params, ret, body, &all_args, span)
                        .await
                }
                Value::ModuleFn { path } => {
                    self.invoke_module_fn(&path, &all_args, span).await
                }
                Value::ClassMethodFn {
                    class,
                    method,
                    expr_id,
                } => {
                    self.invoke_class_method_fn(
                        class, method, expr_id, &all_args, span,
                    )
                    .await
                }
                _ => typechecked!("resolve_partial_app", "Callable callee"),
            }
        }
    }

    /// Check that a return value matches the declared return type.
    ///
    /// Also wraps the return value in `Value::Union` or `Value::Newtype` when
    /// the return type is a union or newtype.
    fn check_return_type(
        &mut self,
        val: Value,
        ret: Option<TypeExprId>,
        span: Span,
    ) -> Result<Value> {
        match ret {
            None => Ok(val),
            Some(expected_ty) => {
                if self.value_matches_type_expr(&val, expected_ty) {
                    Ok(self
                        .maybe_wrap_value(&val, expected_ty, span)
                        .unwrap_or(val))
                } else {
                    typechecked!("return type", "matches declaration")
                }
            }
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
    /// Validates each argument against its declared type (if any), and wraps
    /// values in `Value::Union` or `Value::Newtype` when the parameter type
    /// requires it.
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

                // Wrap if parameter type is union/newtype (named or inline)
                let bound_id = ty.map_or(*val_id, |expected_ty| {
                    self.maybe_wrap_value_id(*val_id, expected_ty, span)
                });

                self.env.scopes.bind(*name, bound_id);
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
            // Unwrap Union/Newtype to find the inner Object
            let unwrapped = self.unwrap_value_recursive(val);
            let v = unwrapped.as_ref().unwrap_or(val);
            // Object alias type: validate with detailed errors
            match v {
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
