//! Function and closure calling.

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::{class, Interpreter};
use crate::ast::{Expr, ExprId};
use crate::builtins::{self, BuiltinCtx, OutputMeta};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::{
    ClassShape, ExprAux, RuntimeTyId, Ty, TyArena, TyId, TyVar, TypeClass,
};
use crate::value::{
    CapturedEnv, FunctionDef, Payload, TypeId, Value, ValueId, ValueMeta,
};
use crate::{ClassId, Result, Span};

struct FnCall<'a> {
    call_id: ExprId,
    name: StringId,
    params: &'a [(StringId, RuntimeTyId)],
    ret: RuntimeTyId,
    body: ExprId,
    args: &'a [ExprId],
    span: Span,
}

struct ClosureCall<'a> {
    call_id: ExprId,
    params: &'a [(StringId, RuntimeTyId)],
    ret: RuntimeTyId,
    body: ExprId,
    env: &'a CapturedEnv,
    args: &'a [ExprId],
    span: Span,
}

impl Interpreter<'_, '_> {
    /// Pipeline operator implementation.
    ///
    /// Applies the right operand (function/closure) to the left operand (value):
    /// `value |> func` becomes `func(value)`
    pub(super) async fn pipeline(
        &mut self,
        call_id: ExprId,
        left: Value,
        right: Payload,
        span: Span,
    ) -> Result<Value> {
        // Intern left value as argument
        let arg_id = self.add_value(left, span);

        // Handle PartialApp via resolve to avoid nesting
        if let Payload::PartialApp {
            callee,
            ref bound,
            expr_id,
        } = right
        {
            self.resolve_partial_app(
                callee,
                bound,
                expr_id,
                &[arg_id],
                Some(call_id),
                span,
            )
            .await
            .map(|value| {
                let meta = self.expr_meta(call_id);
                self.value_with_context_meta(value, meta)
            })
        // `right.clone()` is unavoidable here: `maybe_partial_app` takes
        // ownership, but the fallthrough `match right` below also consumes
        // `right`. In practice the clone is cheap since callable values
        // hold `SmallVec` params and (for closures) an `Arc<CapturedEnv>`.
        } else if let Some(partial) = self.maybe_partial_app(
            right.clone(),
            &[arg_id],
            Some(call_id),
            span,
        ) {
            Ok(self.value_for_expr(call_id, partial))
        } else {
            // Full application (arity == 1); dispatch as before
            match right {
                Payload::Closure {
                    params, body, env, ..
                } => {
                    self.invoke_closure(&params, body, &env, &[arg_id], span)
                        .await
                }
                Payload::Function { params, body, .. } => {
                    self.invoke_function(&params, body, &[arg_id], span).await
                }
                Payload::ModuleFn { path } => {
                    self.invoke_module_fn_for_expr(
                        call_id,
                        &path,
                        &[arg_id],
                        span,
                    )
                    .await
                }
                Payload::VariantCtor { ty, var } => {
                    let payload =
                        self.variant_ctor_payload(&ty, var, &[arg_id]);
                    Ok(self.value_for_expr(call_id, payload))
                }
                Payload::ClassMethodFn {
                    class,
                    method,
                    expr_id,
                } => {
                    self.call_method(
                        call_id,
                        class,
                        method,
                        expr_id,
                        SmallVec::from_slice(&[arg_id]),
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
    pub(super) async fn invoke_closure(
        &mut self,
        params: &[(StringId, RuntimeTyId)],
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
        self.bind_params(params, args, span);
        let ty_subst = self.runtime_ty_subst(params, args);
        self.runtime_ty_substs.push(ty_subst);

        // Evaluate body
        let result = self.eval(body).await;

        self.runtime_ty_substs.pop();

        // Restore original scope stack
        self.env.scopes.restore(saved);

        result
    }

    /// Invoke a named function with pre-evaluated arguments.
    pub(super) async fn invoke_function(
        &mut self,
        params: &[(StringId, RuntimeTyId)],
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
        self.bind_params(params, args, span);
        let ty_subst = self.runtime_ty_subst(params, args);
        self.runtime_ty_substs.push(ty_subst);

        // Evaluate body
        let result = self.eval(body).await;

        self.runtime_ty_substs.pop();

        // Pop parameter scope
        self.env.scopes.pop();

        result
    }

    /// Call a function with an expression-based callee.
    ///
    /// The callee can be:
    /// - A variable (`foo(x)`) resolved via name-based lookup
    /// - A field access (`obj.method(x)`) evaluated then called
    /// - Another call (`make_adder(5)(10)`) for chained calls
    /// - A closure literal (`(x => x * 2)(5)`) for IIFE
    pub(super) async fn call(
        &mut self,
        call_id: ExprId,
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
            Expr::Var(ref name) => {
                self.call_by_name(call_id, *name, args, span).await
            }
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
                    self.variant(call_id, &ty_qn, var_id, args, span)
                        .await
                        .map(|payload| self.value_for_expr(call_id, payload))
                } else {
                    // Evaluate callee expression and call the result
                    let callee_val = self.eval_payload(callee).await?;
                    self.call_value(call_id, callee_val, args, span).await
                }
            }
            _ => {
                // Evaluate callee expression and call the result
                let callee_val = self.eval_payload(callee).await?;
                self.call_value(call_id, callee_val, args, span).await
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
    async fn call_by_name(
        &mut self,
        call_id: ExprId,
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
                    .and_then(|val_id| self.arena.payload(val_id).cloned())
            },
            |_| None,
        );

        match (func_def, scope_val) {
            (Some(def), _) => {
                self.call_function(FnCall {
                    call_id,
                    name,
                    params: &def.params,
                    ret: def.ret,
                    body: def.body,
                    args,
                    span,
                })
                .await
            }
            (None, Some(callee)) => {
                self.call_value(call_id, callee, args, span).await
            }
            // Type checker / resolver guarantees function exists
            (None, None) => typechecked!("call_by_name", "defined function"),
        }
    }

    /// Invoke a module function with pre-evaluated arguments.
    ///
    /// Iterable functions (`Iter.map`, `Iter.filter`, `Iter.reduce`) are
    /// higher-order and need special handling since they invoke closures.
    pub(super) async fn invoke_module_fn(
        &mut self,
        path: &[StringId],
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        if let Some(fn_def) = self.env.get_user_module_fn(path).cloned() {
            // User-defined module function
            self.invoke_user_module_fn(path, &fn_def, args, span).await
        } else {
            // Builtin sync module function; resolver guarantees it exists
            let imp =
                self.env.get_module_fn(path).copied().unwrap_or_else(|| {
                    typechecked!("invoke_module_fn", "known module function")
                });

            let arg_ids: SmallVec<[ValueId; 4]> =
                args.iter().copied().collect();
            self.invoke_builtin(imp, arg_ids, span, OutputMeta::Payload, None)
                .await
        }
    }

    async fn invoke_module_fn_for_expr(
        &mut self,
        expr_id: ExprId,
        path: &[StringId],
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let is_user_fn = self.env.get_user_module_fn(path).is_some();
        let value = self.invoke_module_fn(path, args, span).await?;
        if is_user_fn {
            Ok(value)
        } else {
            let meta = self.expr_meta(expr_id);
            Ok(self.value_with_context_meta(value, meta))
        }
    }

    /// Invoke a user-defined module function.
    ///
    /// Binds all sibling functions and constants at call time, enabling
    /// mutual recursion between module functions. This is different from
    /// closures which capture their environment at creation time.
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

        // Bind all sibling functions as Payload::Function so they can be called
        module.functions.iter().for_each(|(&name, sibling)| {
            let val = Payload::Function {
                name: sibling.name,
                params: sibling.params.clone(),
                ret: sibling.ret,
                body: sibling.body,
            };
            let val_id = self.add_payload(val, span);
            self.env.scopes.bind(name, val_id);
        });

        // Bind all sibling constants
        module.constants.iter().for_each(|(&name, &const_id)| {
            self.env.scopes.bind(name, const_id);
        });

        // Bind parameters
        self.bind_params(&fn_def.params, args, span);

        // Evaluate body
        let result = self.eval(fn_def.body).await;

        // Pop the scope
        self.env.scopes.pop();

        result
    }

    /// Evaluate a class method expression.
    ///
    /// Parses the class name, evaluates arguments, and dispatches to the
    /// appropriate class method.
    pub(super) async fn class_method_expr(
        &mut self,
        expr_id: ExprId,
        class: StringId,
        method: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> Result<Value> {
        let arg_ids = self.eval_args(args).await?;

        let cmf = Payload::ClassMethodFn {
            class,
            method,
            expr_id: Some(expr_id),
        };
        if let Some(partial) =
            self.maybe_partial_app(cmf, &arg_ids, Some(expr_id), span)
        {
            Ok(self.value_for_expr(expr_id, partial))
        } else {
            self.dispatch_method(
                class,
                method,
                Some(expr_id),
                Some(expr_id),
                arg_ids.into(),
                span,
            )
            .await
        }
    }

    fn resolve_method(
        &self,
        cls: StringId,
        method: StringId,
        dispatch_expr_id: Option<ExprId>,
        output_expr_id: Option<ExprId>,
        args: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> class::Dispatch {
        let class = self
            .checked
            .class_registry
            .lookup_by_name(cls)
            .unwrap_or_else(|| typechecked!("method dispatch", "known class"));

        class::Dispatch {
            dispatch_expr_id,
            output_expr_id,
            output_ty: None,
            class,
            method,
            args,
            span,
        }
    }

    async fn dispatch_method(
        &mut self,
        cls: StringId,
        method: StringId,
        dispatch_expr_id: Option<ExprId>,
        output_expr_id: Option<ExprId>,
        args: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        let dispatch = self.resolve_method(
            cls,
            method,
            dispatch_expr_id,
            output_expr_id,
            args,
            span,
        );
        self.dispatch_class_method_value(dispatch).await
    }

    async fn call_method(
        &mut self,
        call_id: ExprId,
        cls: StringId,
        method: StringId,
        dispatch_expr_id: Option<ExprId>,
        args: SmallVec<[ValueId; 4]>,
        span: Span,
    ) -> Result<Value> {
        let dispatch = self.resolve_method(
            cls,
            method,
            dispatch_expr_id,
            Some(call_id),
            args,
            span,
        );
        let class = dispatch.class;
        let value = self.dispatch_class_method_value(dispatch).await?;
        let meta = self.class_method_call_meta(class, call_id);
        Ok(self.value_with_context_meta(value, meta))
    }

    /// Dispatch a class method call.
    ///
    /// Unified entry point for all class methods.
    ///
    /// The `expr_id` parameter is used by nullary methods like
    /// `Default:default` to look up the inferred type.
    pub(super) async fn dispatch_class_method(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Payload> {
        self.dispatch_class_method_value(class::Dispatch {
            dispatch_expr_id: expr_id,
            output_expr_id: expr_id,
            output_ty: None,
            class,
            method,
            args: SmallVec::from_slice(args),
            span,
        })
        .await
        .map(|value| value.payload)
    }

    pub(crate) async fn dispatch_class_method_value(
        &mut self,
        dispatch: class::Dispatch,
    ) -> Result<Value> {
        match self.select_class_call(&dispatch)? {
            builtins::Selected::User(name) => {
                self.invoke_user_instance_fn(name, dispatch).await
            }
            builtins::Selected::Builtin(call) => {
                self.invoke_builtin(
                    call.imp,
                    call.args,
                    call.span,
                    call.output,
                    call.meta,
                )
                .await
            }
        }
    }

    pub(super) fn select_class_call(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Result<builtins::Selected> {
        let inst = dispatch
            .output_expr_id
            .into_iter()
            .chain(dispatch.dispatch_expr_id)
            .find_map(|id| match &self.checked.expr(id).aux {
                ExprAux::InstanceCall { recv, fun, .. } => Some((*recv, *fun)),
                _ => None,
            });

        if let Some((recv, fun)) = inst {
            let fn_name = match recv {
                Some(recv) => fun.or_else(|| {
                    self.checked.types.to_type_id(recv).and_then(|tid| {
                        self.user_instances.lookup_method(
                            dispatch.class,
                            tid,
                            dispatch.method,
                        )
                    })
                }),
                None => fun,
            };
            match fn_name {
                Some(name) => Ok(builtins::Selected::User(name)),
                None => {
                    typechecked!(
                        "class instance dispatch",
                        "receiver or resolved method"
                    )
                }
            }
        } else if let Some(name) = self.hkt_method(dispatch) {
            Ok(builtins::Selected::User(name))
        } else if let Some(name) = self.output_ty_method(dispatch) {
            Ok(builtins::Selected::User(name))
        } else if let Some(tid) = dispatch
            .args
            .first()
            .and_then(|&id| self.arena.value(id))
            .and_then(|v| self.checked.types.to_type_id(v.ty))
        {
            if let Some(name) = self.user_instances.lookup_method(
                dispatch.class,
                tid,
                dispatch.method,
            ) {
                Ok(builtins::Selected::User(name))
            } else {
                self.select_repr_or_registered_class_call(dispatch)
            }
        } else {
            self.select_repr_or_registered_class_call(dispatch)
        }
    }

    fn output_ty_method(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Option<StringId> {
        self.dispatch_output_ty(dispatch)
            .and_then(|ty| self.checked.types.to_type_id(ty))
            .and_then(|tid| {
                self.user_instances.lookup_method(
                    dispatch.class,
                    tid,
                    dispatch.method,
                )
            })
    }

    fn dispatch_output_ty(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Option<RuntimeTyId> {
        dispatch
            .output_ty
            .or_else(|| {
                dispatch
                    .output_expr_id
                    .or(dispatch.dispatch_expr_id)
                    .map(|id| self.checked.expr(id).ty)
            })
            .map(|ty| self.runtime_ty(ty))
    }

    fn hkt_method(&self, dispatch: &class::Dispatch) -> Option<StringId> {
        if matches!(
            self.checked.class_registry.shape(dispatch.class),
            ClassShape::Hkt { .. }
        ) {
            self.hkt_recv_arg(dispatch.class, dispatch.method)
                .and_then(|idx| dispatch.args.get(idx).copied())
                .and_then(|id| self.arena.value(id))
                .and_then(|v| match &v.payload {
                    Payload::Tuple(elems) => self
                        .user_instances
                        .lookup_tuple_method(
                            dispatch.class,
                            elems.len(),
                            dispatch.method,
                        )
                        .or_else(|| {
                            self.user_instances.lookup_method(
                                dispatch.class,
                                TypeId::TUPLE,
                                dispatch.method,
                            )
                        }),
                    _ => self.checked.types.to_type_id(v.ty).and_then(|tid| {
                        self.user_instances.lookup_method(
                            dispatch.class,
                            tid,
                            dispatch.method,
                        )
                    }),
                })
        } else {
            None
        }
    }

    fn hkt_recv_arg(&self, class: ClassId, method: StringId) -> Option<usize> {
        let spec = self
            .checked
            .class_registry
            .get(class)
            .method(method, Span::default())
            .ok()?;
        let scheme = spec.scheme();
        let var =
            scheme.constraints.iter().find_map(|(var, cls)| match cls {
                TypeClass::Hkt { id, .. } if *id == class => Some(*var),
                _ => None,
            })?;
        self.checked.types.scheme_params(scheme).and_then(|params| {
            params.iter().position(|&ty| self.ty_has_hkt_var(ty, var))
        })
    }

    fn ty_has_hkt_var(&self, ty: TyId, var: TyVar) -> bool {
        match self.checked.types.raw(ty) {
            Ty::Var(v) => *v == var,
            Ty::Array(t) | Ty::Option(t) => self.ty_has_hkt_var(*t, var),
            Ty::Result(ok, err) | Ty::Map(ok, err) => {
                self.ty_has_hkt_var(*ok, var) || self.ty_has_hkt_var(*err, var)
            }
            Ty::Fn(params, ret) => {
                params.iter().any(|&t| self.ty_has_hkt_var(t, var))
                    || self.ty_has_hkt_var(*ret, var)
            }
            Ty::Tuple(ts) | Ty::Union(_, ts) | Ty::Named(_, ts) => {
                ts.iter().any(|&t| self.ty_has_hkt_var(t, var))
            }
            Ty::Object(fields) => fields
                .values()
                .any(|&field| self.ty_has_hkt_var(field, var)),
            Ty::Apply(v, ts) => {
                *v == var || ts.iter().any(|&t| self.ty_has_hkt_var(t, var))
            }
            Ty::AssocType(v, _, _) => *v == var,
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Time
            | Ty::Range
            | Ty::Json
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::Regex
            | Ty::RuntimeError
            | Ty::Local
            | Ty::Global
            | Ty::Unknown
            | Ty::Error => false,
        }
    }

    fn select_repr_or_registered_class_call(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Result<builtins::Selected> {
        let repr_ty =
            if matches!(dispatch.class, ClassId::INTO | ClassId::TRY_INTO) {
                None
            } else {
                dispatch
                    .args
                    .first()
                    .and_then(|&id| self.arena.value(id))
                    .and_then(|v| self.checked.types.to_type_id(v.repr))
            };

        if let Some(type_id) = repr_ty {
            if let Some(fn_name) = self.user_instances.lookup_method(
                dispatch.class,
                type_id,
                dispatch.method,
            ) {
                Ok(builtins::Selected::User(fn_name))
            } else {
                self.select_registered_class_call(dispatch)
            }
        } else {
            self.select_registered_class_call(dispatch)
        }
    }

    fn select_registered_class_call(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Result<builtins::Selected> {
        let def = *self
            .class_methods
            .lookup(dispatch.class, dispatch.method)
            .unwrap_or_else(|| typechecked!("class method", "registered"));
        let output = self.class_output_meta(
            dispatch.output_expr_id,
            dispatch.dispatch_expr_id,
            dispatch.output_ty,
            dispatch.class,
        );
        let meta = self.class_call_meta(dispatch, def.abi)?;
        match def.builtin {
            class::Builtin::Fixed(imp) => {
                Ok(builtins::Selected::Builtin(builtins::Call {
                    imp,
                    args: dispatch.args.clone(),
                    span: dispatch.span,
                    output,
                    meta,
                }))
            }
            class::Builtin::Selected(f) => {
                f(self, dispatch).map(builtins::Selected::Builtin)
            }
        }
    }

    fn class_call_meta(
        &mut self,
        dispatch: &class::Dispatch,
        abi: class::MethodAbi,
    ) -> Result<Option<builtins::CallMeta>> {
        match abi {
            class::MethodAbi::Nullary => {
                let ty = self.class_nullary_target(dispatch)?;
                Ok(Some(builtins::CallMeta::Nullary { ty }))
            }
            class::MethodAbi::Convert => {
                let target = self.class_convert_target(dispatch)?;
                let edge = self.class_approved_edge(dispatch);
                Ok(Some(builtins::CallMeta::Convert { target, edge }))
            }
            class::MethodAbi::Binary
            | class::MethodAbi::Unary
            | class::MethodAbi::Hkt => Ok(None),
        }
    }

    fn class_nullary_target(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Result<RuntimeTyId> {
        dispatch
            .output_ty
            .or_else(|| {
                dispatch
                    .output_expr_id
                    .or(dispatch.dispatch_expr_id)
                    .map(|id| self.checked.expr(id).ty)
            })
            .map(|ty| self.runtime_ty(ty))
            .ok_or_else(|| {
                typechecked!("nullary class method", "expression id")
            })
    }

    fn class_convert_target(
        &mut self,
        dispatch: &class::Dispatch,
    ) -> Result<RuntimeTyId> {
        self.class_approved_edge(dispatch)
            .map(|meta| meta.ty)
            .or_else(|| {
                dispatch.output_ty.or_else(|| {
                    dispatch
                        .output_expr_id
                        .or(dispatch.dispatch_expr_id)
                        .map(|id| self.checked.expr(id).ty)
                })
            })
            .map(|ty| self.runtime_ty(ty))
            .map(|ty| {
                if dispatch.class == ClassId::TRY_INTO {
                    match self.checked.types.get(ty) {
                        Ty::Result(ok, _) => RuntimeTyId::from(*ok),
                        _ => ty,
                    }
                } else {
                    ty
                }
            })
            .ok_or_else(|| {
                typechecked!("convert class method", "expression id")
            })
    }

    fn class_approved_edge(
        &self,
        dispatch: &class::Dispatch,
    ) -> Option<ValueMeta> {
        dispatch
            .output_expr_id
            .and_then(|id| self.approved_newtype_edge_meta(id))
            .or_else(|| {
                dispatch
                    .dispatch_expr_id
                    .and_then(|id| self.approved_newtype_edge_meta(id))
            })
    }

    async fn invoke_user_instance_fn(
        &mut self,
        name: StringId,
        dispatch: class::Dispatch,
    ) -> Result<Value> {
        let def = self.functions.get(&name).cloned();
        if let Some(def) = def {
            self.invoke_function(
                &def.params,
                def.body,
                &dispatch.args,
                dispatch.span,
            )
            .await
        } else {
            typechecked!("user instance method", "registered function")
        }
    }

    pub(super) fn refine_variant_output(
        &self,
        output: OutputMeta,
        payload: &Payload,
        args: &[ValueId],
    ) -> OutputMeta {
        match (output, payload) {
            (OutputMeta::Expr(id), Payload::Variant { .. })
                if self
                    .checked
                    .types
                    .to_type_id(self.expr_meta(id).ty)
                    .is_none() =>
            {
                let expr_ty = self.expr_meta(id).ty;
                args.iter()
                    .filter_map(|arg| self.arena.meta(*arg))
                    .find(|meta| {
                        self.checked.types.to_type_id(meta.repr).is_some()
                    })
                    .map_or(output, |meta| {
                        OutputMeta::Meta(
                            self.checked.types.union_meta(expr_ty, meta.repr),
                        )
                    })
            }
            _ => output,
        }
    }

    pub(in crate::interpreter) fn class_output_meta(
        &mut self,
        output_expr_id: Option<ExprId>,
        dispatch_expr_id: Option<ExprId>,
        output_ty: Option<RuntimeTyId>,
        class: ClassId,
    ) -> OutputMeta {
        match output_ty {
            Some(ty) => OutputMeta::Ty(ty),
            None => match output_expr_id {
                Some(id) => {
                    let info = self.checked.expr(id);
                    match &info.aux {
                        ExprAux::HofCall { out, .. } => OutputMeta::Ty(*out),
                        _ if class == ClassId::TRY_INTO => {
                            let target = self
                                .approved_newtype_edge_meta(id)
                                .map(|meta| meta.ty)
                                .or_else(|| {
                                    let ty = self.checked_expr_meta(id).ty;
                                    if self.checked.types.to_type_id(ty)
                                        == Some(TypeId::RESULT)
                                    {
                                        None
                                    } else {
                                        Some(ty)
                                    }
                                });
                            target.map_or(OutputMeta::Expr(id), |ty| {
                                OutputMeta::Ty(self.checked.types.result(
                                    ty,
                                    RuntimeTyId::from(TyArena::STRING),
                                ))
                            })
                        }
                        _ => OutputMeta::Expr(id),
                    }
                }
                None => dispatch_expr_id
                    .map(|id| {
                        let meta = self.checked_expr_meta(id);
                        self.callable_ret(meta.ty).unwrap_or_else(|| {
                            if class == ClassId::TRY_INTO {
                                self.checked.types.result(
                                    meta.ty,
                                    RuntimeTyId::from(TyArena::STRING),
                                )
                            } else {
                                meta.ty
                            }
                        })
                    })
                    .map_or(OutputMeta::Payload, OutputMeta::Ty),
            },
        }
    }

    fn class_method_call_meta(
        &mut self,
        class: ClassId,
        id: ExprId,
    ) -> ValueMeta {
        let meta = self.expr_meta(id);
        if class == ClassId::TRY_INTO
            && self.checked.types.to_type_id(meta.ty) != Some(TypeId::RESULT)
        {
            let ty = self
                .checked
                .types
                .result(meta.ty, RuntimeTyId::from(TyArena::STRING));
            self.checked.types.meta(ty)
        } else {
            meta
        }
    }

    pub(super) fn callable_ret(&self, ty: RuntimeTyId) -> Option<RuntimeTyId> {
        match self.checked.types.get(ty) {
            Ty::Fn(_, ret) => Some(RuntimeTyId::from(*ret)),
            _ => None,
        }
    }

    fn variant_ctor_arity(
        &self,
        ty: &QualifiedName,
        var: StringId,
    ) -> Option<usize> {
        self.registry
            .lookup(ty)
            .and_then(|id| self.registry.lookup_variant(id, var))
            .map(|v| v.arity as usize)
    }

    fn variant_ctor_payload(
        &self,
        ty: &QualifiedName,
        var: StringId,
        vals: &[ValueId],
    ) -> Payload {
        let var_def = self
            .registry
            .lookup(ty)
            .and_then(|id| self.registry.lookup_variant(id, var))
            .unwrap_or_else(|| typechecked!("variant constructor", "known"));
        if var_def.arity as usize == vals.len() {
            Payload::Variant {
                tag: var_def.idx,
                vals: SmallVec::from_slice(vals),
            }
        } else {
            typechecked!("variant constructor arity", "correct")
        }
    }

    pub(super) fn value_for_output(
        &mut self,
        output: OutputMeta,
        payload: Payload,
    ) -> Value {
        match output {
            OutputMeta::Expr(id) => self.value_for_expr(id, payload),
            OutputMeta::Ty(ty) => {
                let meta = self.checked.types.meta(ty);
                self.value_from_meta(payload, meta)
            }
            OutputMeta::Meta(meta) => self.value_from_meta(payload, meta),
            OutputMeta::Payload => self.value_from_payload(payload),
        }
    }

    pub(super) fn value_for_output_value(
        &mut self,
        output: OutputMeta,
        value: Value,
    ) -> Value {
        match output {
            OutputMeta::Expr(id) => {
                let meta = self.expr_meta(id);
                self.value_with_context_meta(value, meta)
            }
            OutputMeta::Ty(ty) => {
                let meta = match self.checked.types.get(ty) {
                    Ty::Union(_, _) => {
                        self.checked.types.union_meta(ty, value.repr)
                    }
                    _ => self.checked.types.meta(ty),
                };
                self.value_with_context_meta(value, meta)
            }
            OutputMeta::Meta(meta) => self.value_with_context_meta(value, meta),
            OutputMeta::Payload => value,
        }
    }

    fn value_for_optional_expr(
        &mut self,
        expr_id: Option<ExprId>,
        payload: Payload,
    ) -> Value {
        match expr_id {
            Some(id) => self.value_for_expr(id, payload),
            None => self.value_from_payload(payload),
        }
    }

    /// Invoke a callable value (closure/function) with arguments.
    ///
    /// Used by higher-order primitives to call user-provided functions.
    pub(crate) async fn invoke_callable(
        &mut self,
        callee_id: ValueId,
        args: &[ValueId],
        span: Span,
    ) -> Result<ValueId> {
        let callee = self
            .arena
            .value(callee_id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena"));
        let callee_ty = callee.ty;

        match callee.payload {
            Payload::Closure {
                params,
                ret,
                body,
                env,
            } => {
                let result = self
                    .invoke_closure(&params, body, &env, args, span)
                    .await?;
                let meta = self.checked.types.meta(ret);
                let result = self.value_with_context_meta(result, meta);
                Ok(self.add_value(result, span))
            }
            Payload::Function {
                params, ret, body, ..
            } => {
                let result =
                    self.invoke_function(&params, body, args, span).await?;
                let meta = self.checked.types.meta(ret);
                let result = self.value_with_context_meta(result, meta);
                Ok(self.add_value(result, span))
            }
            Payload::ModuleFn { path } => {
                let result = self.invoke_module_fn(&path, args, span).await?;
                let result = self.callable_ret(callee_ty).map_or(
                    result.clone(),
                    |ret| {
                        let meta = self.checked.types.meta(ret);
                        self.value_with_context_meta(result, meta)
                    },
                );
                Ok(self.add_value(result, span))
            }
            Payload::VariantCtor { ty, var } => {
                let payload = self.variant_ctor_payload(&ty, var, args);
                let value = match self.callable_ret(callee_ty) {
                    Some(ret) => {
                        let meta = self.checked.types.meta(ret);
                        self.value_from_meta(payload, meta)
                    }
                    None => self.value_from_payload(payload),
                };
                Ok(self.add_value(value, span))
            }
            Payload::ClassMethodFn {
                class,
                method,
                expr_id,
            } => {
                let result = self
                    .dispatch_method(
                        class,
                        method,
                        expr_id,
                        None,
                        SmallVec::from_slice(args),
                        span,
                    )
                    .await?;
                let result = self.callable_ret(callee_ty).map_or(
                    result.clone(),
                    |ret| {
                        let meta = self.checked.types.meta(ret);
                        self.value_with_context_meta(result, meta)
                    },
                );
                Ok(self.add_value(result, span))
            }
            Payload::PartialApp {
                callee,
                bound,
                expr_id,
            } => {
                let result = self
                    .resolve_partial_app(
                        callee, &bound, expr_id, args, None, span,
                    )
                    .await?;
                let result = self.callable_ret(callee_ty).map_or(
                    result.clone(),
                    |ret| {
                        let meta = self.checked.types.meta(ret);
                        self.value_with_context_meta(result, meta)
                    },
                );
                Ok(self.add_value(result, span))
            }
            // Type checker guarantees callee is callable
            _ => typechecked!("invoke_callable", "Callable"),
        }
    }

    /// Execute a selected builtin implementation.
    ///
    /// The caller supplies output metadata and call metadata. The sync branch
    /// calls the function pointer directly. The async branch is the only path
    /// that awaits a boxed future. Both branches finish through `BuiltinCtx`
    /// so output refinement stays centralized.
    pub(super) async fn invoke_builtin(
        &mut self,
        imp: builtins::Impl,
        args: SmallVec<[ValueId; 4]>,
        span: Span,
        output: OutputMeta,
        meta: Option<builtins::CallMeta>,
    ) -> Result<Value> {
        let mut ctx = BuiltinCtx::new(self, span, output, meta);

        let id = match imp {
            builtins::Impl::Sync(f) => f(&mut ctx, args)?,
            builtins::Impl::Async(f) => f(&mut ctx, args).await?,
        };

        ctx.finish(id)
    }

    /// Call a function or closure value.
    async fn call_value(
        &mut self,
        call_id: ExprId,
        callee: Payload,
        args: &[ExprId],
        span: Span,
    ) -> Result<Value> {
        match callee {
            Payload::Closure {
                params,
                ret,
                body,
                env,
            } => {
                self.call_closure(ClosureCall {
                    call_id,
                    params: &params,
                    ret,
                    body,
                    env: &env,
                    args,
                    span,
                })
                .await
            }
            Payload::Function {
                name,
                params,
                ret,
                body,
            } => {
                self.call_function(FnCall {
                    call_id,
                    name,
                    params: &params,
                    ret,
                    body,
                    args,
                    span,
                })
                .await
            }
            Payload::ModuleFn { path } => {
                let vals = self.eval_args(args).await?;
                if let Some(partial) = self.maybe_partial_app(
                    Payload::ModuleFn { path: path.clone() },
                    &vals,
                    Some(call_id),
                    span,
                ) {
                    Ok(self.value_for_expr(call_id, partial))
                } else {
                    self.invoke_module_fn_for_expr(call_id, &path, &vals, span)
                        .await
                }
            }
            Payload::VariantCtor { ty, var } => {
                let vals = self.eval_args(args).await?;
                if let Some(partial) = self.maybe_partial_app(
                    Payload::VariantCtor {
                        ty: ty.clone(),
                        var,
                    },
                    &vals,
                    Some(call_id),
                    span,
                ) {
                    Ok(self.value_for_expr(call_id, partial))
                } else {
                    let payload = self.variant_ctor_payload(&ty, var, &vals);
                    Ok(self.value_for_expr(call_id, payload))
                }
            }
            Payload::ClassMethodFn {
                class,
                method,
                expr_id,
            } => {
                let vals = self.eval_args(args).await?;
                if let Some(partial) = self.maybe_partial_app(
                    Payload::ClassMethodFn {
                        class,
                        method,
                        expr_id,
                    },
                    &vals,
                    Some(call_id),
                    span,
                ) {
                    Ok(self.value_for_expr(call_id, partial))
                } else {
                    self.call_method(
                        call_id,
                        class,
                        method,
                        expr_id,
                        SmallVec::from_slice(&vals),
                        span,
                    )
                    .await
                }
            }
            // `loop` continuation: calling it signals loop continuation
            Payload::LoopContinuation => {
                // Type checker guarantees exactly one argument
                let new_state_expr = args
                    .first()
                    .unwrap_or_else(|| typechecked!("continuation", "1 arg"));
                let new_state = self.eval(*new_state_expr).await?;
                let state_id = self.add_value(new_state, span);
                Ok(self
                    .value_for_expr(call_id, Payload::LoopContinue(state_id)))
            }
            Payload::PartialApp {
                callee,
                bound,
                expr_id,
            } => {
                let vals = self.eval_args(args).await?;
                self.resolve_partial_app(
                    callee,
                    &bound,
                    expr_id,
                    &vals,
                    Some(call_id),
                    span,
                )
                .await
                .map(|value| {
                    let meta = self.expr_meta(call_id);
                    self.value_with_context_meta(value, meta)
                })
            }
            // Type checker guarantees callee is callable
            _ => typechecked!("call", "Callable"),
        }
    }

    /// Call a named function with expression arguments.
    async fn call_function(&mut self, c: FnCall<'_>) -> Result<Value> {
        if c.args.len() > c.params.len() {
            typechecked!("call_function", "correct arity")
        }
        let vals = self.eval_args(c.args).await?;
        if vals.len() < c.params.len() {
            let f = Payload::Function {
                name: c.name,
                params: c.params.iter().copied().collect(),
                ret: c.ret,
                body: c.body,
            };
            let partial = self
                .maybe_partial_app(f, &vals, Some(c.call_id), c.span)
                .unwrap_or_else(|| {
                    invariant!("partial app when under-applied")
                });
            Ok(self.value_for_expr(c.call_id, partial))
        } else {
            let value = self
                .invoke_function(c.params, c.body, &vals, c.span)
                .await?;
            let meta = self.expr_meta(c.call_id);
            Ok(self.value_with_context_meta(value, meta))
        }
    }

    /// Call a closure with expression arguments.
    async fn call_closure(&mut self, c: ClosureCall<'_>) -> Result<Value> {
        if c.args.len() > c.params.len() {
            typechecked!("call_closure", "correct arity")
        }
        let vals = self.eval_args(c.args).await?;
        if vals.len() < c.params.len() {
            let closure = Payload::Closure {
                params: c.params.iter().copied().collect(),
                ret: c.ret,
                body: c.body,
                env: c.env.clone().into(),
            };
            let partial = self
                .maybe_partial_app(closure, &vals, Some(c.call_id), c.span)
                .unwrap_or_else(|| {
                    invariant!("partial app when under-applied")
                });
            Ok(self.value_for_expr(c.call_id, partial))
        } else {
            let value = self
                .invoke_closure(c.params, c.body, c.env, &vals, c.span)
                .await?;
            let meta = self.expr_meta(c.call_id);
            Ok(self.value_with_context_meta(value, meta))
        }
    }

    /// Determine the expected arity of any callable value.
    ///
    /// Returns `None` if the value is not callable.
    fn callable_arity(&self, v: &Payload) -> Option<usize> {
        match v {
            Payload::Closure { params, .. }
            | Payload::Function { params, .. } => Some(params.len()),
            Payload::ModuleFn { path } => self
                .env
                .get_user_module_fn(path)
                .map(|d| d.params.len())
                .or_else(|| self.checked.module_fn_arity(path)),
            Payload::VariantCtor { ty, var } => {
                self.variant_ctor_arity(ty, *var)
            }
            Payload::ClassMethodFn { class, method, .. } => {
                self.checked.class_registry.lookup_by_name(*class).and_then(
                    |kind| {
                        self.checked
                            .class_registry
                            .get(kind)
                            .method(*method, Span::default())
                            .ok()
                            .and_then(|spec| {
                                self.checked.types.scheme_arity(spec.scheme())
                            })
                    },
                )
            }
            Payload::PartialApp { callee, bound, .. } => self
                .arena
                .payload(*callee)
                .and_then(|c| self.callable_arity(c))
                .map(|n| n.saturating_sub(bound.len())),
            _ => None,
        }
    }

    /// Check if a call is a partial application.
    ///
    /// If `args` supplies fewer arguments than `callee` expects, returns a
    /// `Payload::PartialApp` capturing the callee and bound args. Otherwise
    /// returns `None`, meaning the caller should proceed with full invocation.
    fn maybe_partial_app(
        &mut self,
        callee: Payload,
        args: &[ValueId],
        expr_id: Option<ExprId>,
        span: Span,
    ) -> Option<Payload> {
        let arity = self.callable_arity(&callee)?;
        if args.len() < arity && !args.is_empty() {
            let callee_id = self.add_payload(callee, span);
            Some(Payload::PartialApp {
                callee: callee_id,
                bound: args.iter().copied().collect(),
                expr_id,
            })
        } else {
            None
        }
    }

    /// Resolve a partial application with additional arguments.
    ///
    /// Combines bound args with new args and either produces another
    /// `PartialApp` (still under-applied) or fully invokes the callee.
    async fn resolve_partial_app(
        &mut self,
        callee_id: ValueId,
        bound: &[ValueId],
        partial_expr_id: Option<ExprId>,
        new_args: &[ValueId],
        output_expr_id: Option<ExprId>,
        span: Span,
    ) -> Result<Value> {
        let all_args: SmallVec<[ValueId; 4]> =
            bound.iter().chain(new_args.iter()).copied().collect();

        let callee = self
            .arena
            .payload(callee_id)
            .cloned()
            .unwrap_or_else(|| invariant!("PartialApp callee in arena"));

        let arity = self
            .callable_arity(&callee)
            .unwrap_or_else(|| invariant!("PartialApp callee is callable"));

        if all_args.len() > arity {
            typechecked!("resolve_partial_app", "args <= arity")
        } else if all_args.len() < arity {
            let partial = Payload::PartialApp {
                callee: callee_id,
                bound: all_args,
                expr_id: output_expr_id.or(partial_expr_id),
            };
            Ok(self.value_from_payload(partial))
        } else {
            match callee {
                Payload::Closure {
                    params, body, env, ..
                } => {
                    self.invoke_closure(&params, body, &env, &all_args, span)
                        .await
                }
                Payload::Function { params, body, .. } => {
                    self.invoke_function(&params, body, &all_args, span).await
                }
                Payload::ModuleFn { path } => {
                    self.invoke_module_fn(&path, &all_args, span).await
                }
                Payload::VariantCtor { ty, var } => Ok(self
                    .value_from_payload(
                        self.variant_ctor_payload(&ty, var, &all_args),
                    )),
                Payload::ClassMethodFn {
                    class,
                    method,
                    expr_id,
                } => {
                    self.dispatch_method(
                        class,
                        method,
                        expr_id,
                        output_expr_id.or(partial_expr_id),
                        SmallVec::from_slice(&all_args),
                        span,
                    )
                    .await
                }
                _ => typechecked!("resolve_partial_app", "Callable callee"),
            }
        }
    }

    /// Evaluate a list of argument expressions.
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
                let val_id = self.add_value(val, span);
                acc.push(val_id);
                self.eval_args_rec(tail, acc).await
            }
        }
    }

    /// Bind parameters to argument values in the current scope.
    ///
    /// Type checking has already validated all argument types at call sites.
    pub(super) fn bind_params(
        &mut self,
        params: &[(StringId, RuntimeTyId)],
        args: &[ValueId],
        span: Span,
    ) {
        params
            .iter()
            .zip(args.iter())
            .for_each(|((name, ty), val_id)| {
                let id = match self.checked.types.get(*ty) {
                    Ty::Union(_, _) => {
                        let val =
                            self.arena.value(*val_id).cloned().unwrap_or_else(
                                || invariant!("parameter value in arena"),
                            );
                        let meta = self.checked.types.union_meta(*ty, val.repr);
                        let val = self.value_with_context_meta(val, meta);
                        self.add_value(val, span)
                    }
                    _ => *val_id,
                };
                self.env.scopes.bind(*name, id);
            });
    }
}
