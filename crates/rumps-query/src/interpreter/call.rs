//! Function and closure calling.

use std::cmp::Ordering;
use std::ops::ControlFlow;
use std::sync::Arc;

use async_recursion::async_recursion;
use smallvec::SmallVec;

use super::class::{self, ClassCtx, MethodFn};
use super::{hof, Interpreter};
use crate::ast::{Expr, ExprId};
use crate::env::{PrimCtx, PrimFn};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::typecheck::{
    ClassShape, ExprAux, RuntimeTyId, Ty, TyArena, TyId, TyVar, TypeClass,
};
use crate::value::{
    CapturedEnv, FunctionDef, Payload, TypeId, Value, ValueId, ValueMeta,
};
use crate::{ClassId, Error, Result, Span};

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

#[derive(Clone)]
pub(super) struct ClassDispatch {
    pub(super) dispatch_expr_id: Option<ExprId>,
    pub(super) output_expr_id: Option<ExprId>,
    pub(super) output_ty: Option<RuntimeTyId>,
    pub(super) class: ClassId,
    pub(super) method: StringId,
    pub(super) args: SmallVec<[ValueId; 4]>,
    pub(super) span: Span,
}

#[derive(Clone)]
struct ClassMethodInvoke {
    class: StringId,
    method: StringId,
    dispatch_expr_id: Option<ExprId>,
    output_expr_id: Option<ExprId>,
    args: SmallVec<[ValueId; 4]>,
    span: Span,
}

#[derive(Clone, Copy)]
enum OutputMeta {
    Expr(ExprId),
    Ty(RuntimeTyId),
    Meta(ValueMeta),
    Payload,
}

impl<I: IoContext> Interpreter<'_, I> {
    /// Pipeline operator implementation.
    ///
    /// Applies the right operand (function/closure) to the left operand (value):
    /// `value |> func` becomes `func(value)`
    #[async_recursion]
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
                } => self
                    .invoke_class_method_fn_value(ClassMethodInvoke {
                        class,
                        method,
                        dispatch_expr_id: expr_id,
                        output_expr_id: Some(call_id),
                        args: SmallVec::from_slice(&[arg_id]),
                        span,
                    })
                    .await
                    .map(|value| {
                        let kind = self
                            .checked
                            .class_registry
                            .lookup_by_name(class)
                            .unwrap_or_else(|| {
                                typechecked!("class method class", "known")
                            });
                        let meta = self.class_method_call_meta(kind, call_id);
                        self.value_with_context_meta(value, meta)
                    }),
                // Type checker guarantees rhs is callable
                _ => typechecked!("|>", "Callable"),
            }
        }
    }

    /// Invoke a closure with pre-evaluated arguments.
    #[async_recursion]
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
    #[async_recursion]
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
    #[async_recursion]
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
    #[async_recursion]
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

    /// Call a built-in primitive function.
    ///
    /// Evaluates arguments first, then invokes the primitive with a `PrimCtx`.
    #[async_recursion]
    async fn call_primitive(
        &mut self,
        prim: PrimFn,
        args: &[ExprId],
        span: Span,
    ) -> Result<Payload> {
        // Evaluate arguments
        let arg_ids: SmallVec<[ValueId; 4]> =
            self.eval_args(args).await?.into_iter().collect();

        // Create context and call primitive
        let mut ctx = PrimCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            io: &mut self.io,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        // Look up and clone the result value
        Ok(self
            .arena
            .payload(result_id)
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
        // Builtins that need interpreter services are intercepted before sync
        // primitive dispatch. This includes HoFs that invoke closures and async
        // builtins that need class dispatch or other interpreter-owned context.
        if let Some(value) =
            self.invoke_async_map_module_fn(path, args, span).await?
        {
            Ok(value)
        } else if let Some((hof, result)) = self.module_hofs.lookup(path) {
            let value = self
                .run_hof_trampoline(OutputMeta::Payload, hof, args, span)
                .await?;
            // Some HoFs (e.g. `foreach`) delegate to another HoF but discard
            // the produced value, evaluating to `Unit` instead.
            match result {
                hof::ResultMode::Keep => Ok(value),
                hof::ResultMode::Discard => {
                    Ok(self.value_from_payload(Payload::Unit))
                }
            }
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

    #[async_recursion]
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
            let kind = self
                .checked
                .class_registry
                .lookup_by_name(class)
                .unwrap_or_else(|| {
                    typechecked!("class method class", "known class")
                });
            self.dispatch_class_method_value(ClassDispatch {
                dispatch_expr_id: Some(expr_id),
                output_expr_id: Some(expr_id),
                output_ty: None,
                class: kind,
                method,
                args: arg_ids.into(),
                span,
            })
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
    ) -> Result<Payload> {
        self.invoke_class_method_fn_value(ClassMethodInvoke {
            class,
            method,
            dispatch_expr_id: expr_id,
            output_expr_id: None,
            args: SmallVec::from_slice(args),
            span,
        })
        .await
        .map(|value| value.payload)
    }

    #[async_recursion]
    async fn invoke_class_method_fn_value(
        &mut self,
        invoke: ClassMethodInvoke,
    ) -> Result<Value> {
        let kind = self
            .checked
            .class_registry
            .lookup_by_name(invoke.class)
            .unwrap_or_else(|| {
                typechecked!("invoke_class_method_fn", "known class")
            });

        self.dispatch_class_method_value(ClassDispatch {
            dispatch_expr_id: invoke.dispatch_expr_id,
            output_expr_id: invoke.output_expr_id,
            output_ty: None,
            class: kind,
            method: invoke.method,
            args: invoke.args,
            span: invoke.span,
        })
        .await
    }

    /// Dispatch a class method call.
    ///
    /// Unified entry point for all class methods.
    ///
    /// The `expr_id` parameter is used by nullary methods like
    /// `Default:default` to look up the inferred type.
    #[async_recursion]
    pub(super) async fn dispatch_class_method(
        &mut self,
        expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Payload> {
        self.dispatch_class_method_value(ClassDispatch {
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

    #[async_recursion]
    pub(super) async fn dispatch_class_method_value(
        &mut self,
        dispatch: ClassDispatch,
    ) -> Result<Value> {
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
                Some(name) => {
                    self.invoke_user_instance_fn(name, dispatch).await
                }
                None => {
                    typechecked!(
                        "class instance dispatch",
                        "receiver or resolved method"
                    )
                }
            }
        } else if let Some(name) = self.hkt_method(&dispatch) {
            self.invoke_user_instance_fn(name, dispatch).await
        } else if let Some(name) = self.output_ty_method(&dispatch) {
            self.invoke_user_instance_fn(name, dispatch).await
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
                self.invoke_user_instance_fn(name, dispatch).await
            } else {
                self.dispatch_via_repr_or_builtin(dispatch).await
            }
        } else {
            self.dispatch_via_repr_or_builtin(dispatch).await
        }
    }

    fn output_ty_method(
        &mut self,
        dispatch: &ClassDispatch,
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
        dispatch: &ClassDispatch,
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

    fn hkt_method(&self, dispatch: &ClassDispatch) -> Option<StringId> {
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

    #[async_recursion]
    async fn dispatch_via_repr_or_builtin(
        &mut self,
        dispatch: ClassDispatch,
    ) -> Result<Value> {
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
                self.invoke_user_instance_fn(fn_name, dispatch).await
            } else {
                self.dispatch_builtin_or_hof(dispatch).await
            }
        } else {
            self.dispatch_builtin_or_hof(dispatch).await
        }
    }

    #[async_recursion]
    async fn invoke_user_instance_fn(
        &mut self,
        name: StringId,
        dispatch: ClassDispatch,
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

    /// Dispatch to builtin class method or async HOF.
    ///
    /// Async wrapper that handles both sync builtin methods and async HOFs.
    #[async_recursion]
    async fn dispatch_builtin_or_hof(
        &mut self,
        dispatch: ClassDispatch,
    ) -> Result<Value> {
        // Check for HOF first (requires async)
        let output = self.class_output_meta(
            dispatch.output_expr_id,
            dispatch.dispatch_expr_id,
            dispatch.output_ty,
            dispatch.class,
        );
        if let Some(value) = self
            .dispatch_async_map_class_method(
                output,
                dispatch.class,
                dispatch.method,
                &dispatch.args,
                dispatch.span,
            )
            .await?
        {
            Ok(value)
        } else if let Some(result) = self
            .dispatch_forwarding_builtin_class_method(
                dispatch.output_expr_id,
                dispatch.class,
                dispatch.method,
                &dispatch.args,
                dispatch.span,
            )
        {
            result
        } else if let Some(MethodFn::Hof(f)) =
            self.class_methods.lookup(dispatch.class, dispatch.method)
        {
            self.run_hof_trampoline(output, f, &dispatch.args, dispatch.span)
                .await
        } else {
            self.dispatch_builtin_class_method(&dispatch)
                .map(|payload| {
                    let output = self.refine_variant_output(
                        output,
                        &payload,
                        &dispatch.args,
                    );
                    self.value_for_output(output, payload)
                })
        }
    }

    fn refine_variant_output(
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

    fn class_output_meta(
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

    fn callable_ret(&self, ty: RuntimeTyId) -> Option<RuntimeTyId> {
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

    fn value_for_output(
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

    fn value_for_output_value(
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

    fn dispatch_forwarding_builtin_class_method(
        &mut self,
        output_expr_id: Option<ExprId>,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Option<Result<Value>> {
        let unwrap = self.arena.intern("unwrap");
        let index = self.arena.intern("index");
        if class == ClassId::FALLIBLE && method == unwrap {
            Some(self.fallible_unwrap_value(args, span))
        } else if class == ClassId::INDEXABLE && method == index {
            Some(self.indexable_index_value(output_expr_id, args, span))
        } else {
            None
        }
    }

    fn fallible_unwrap_value(
        &mut self,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let recv = *args
            .first()
            .unwrap_or_else(|| typechecked!("unwrap", "1 arg"));
        let recv_ty = self.arena.meta(recv).and_then(|m| {
            self.checked
                .types
                .to_type_id(m.repr)
                .or_else(|| self.checked.types.to_type_id(m.ty))
        });
        match self.arena.payload(recv).cloned() {
            Some(Payload::Variant { tag: 1, vals })
                if recv_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                vals.first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .ok_or_else(|| {
                        typechecked!("unwrap", "Option.Some payload")
                    })
            }
            Some(Payload::Variant { tag: 0, .. })
                if recv_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            Some(Payload::Variant { tag: 0, vals })
                if recv_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                vals.first()
                    .and_then(|id| self.arena.value(*id).cloned())
                    .ok_or_else(|| typechecked!("unwrap", "Result.Ok payload"))
            }
            Some(Payload::Variant { tag: 1, .. })
                if recv_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Err(Error::runtime(span, "cannot unwrap Result.Err"))
            }
            _ => typechecked!("unwrap", "Fallible"),
        }
    }

    async fn dispatch_async_map_class_method(
        &mut self,
        output: OutputMeta,
        class: ClassId,
        method: StringId,
        args: &[ValueId],
        span: Span,
    ) -> Result<Option<Value>> {
        let index = self.arena.intern("index");
        let get = self.arena.intern("get");
        let eq = self.arena.intern("eq");
        let compare = self.arena.intern("compare");
        let concat = self.arena.intern("concat");
        let base_id = args.first().copied();
        let idx_id = args.get(1).copied();
        let map = base_id.and_then(|id| self.arena.get_map(id).cloned());

        if class == ClassId::EQ && method == eq {
            let other =
                args.get(1).and_then(|id| self.arena.get_map(*id)).cloned();
            match (map, other) {
                (Some(map), Some(other)) => {
                    let b = self.map_eq_maps(&map, &other, span).await?;
                    Ok(Some(self.value_for_output(output, Payload::Bool(b))))
                }
                (Some(_), None) => typechecked!("Eq:eq", "Map"),
                _ => Ok(None),
            }
        } else if class == ClassId::ORD && method == compare {
            let other =
                args.get(1).and_then(|id| self.arena.get_map(*id)).cloned();
            match (map, other) {
                (Some(map), Some(other)) => {
                    let payload =
                        match self.map_cmp_maps(&map, &other, span).await? {
                            Ordering::Less => Payload::lt(),
                            Ordering::Equal => Payload::eq_ord(),
                            Ordering::Greater => Payload::gt(),
                        };
                    let value = match output {
                        OutputMeta::Payload => self.value_from_meta(
                            payload,
                            self.checked.types.meta_ordering(),
                        ),
                        _ => self.value_for_output(output, payload),
                    };
                    Ok(Some(value))
                }
                (Some(_), None) => typechecked!("Ord:compare", "Map"),
                _ => Ok(None),
            }
        } else if class == ClassId::CONCATABLE && method == concat {
            let other =
                args.get(1).and_then(|id| self.arena.get_map(*id)).cloned();
            match (map, other) {
                (Some(map), Some(other)) => {
                    let map = self.map_merge_maps(&map, &other, span).await?;
                    Ok(Some(
                        self.value_for_output(
                            output,
                            Payload::Map(Arc::new(map)),
                        ),
                    ))
                }
                (Some(_), None) => typechecked!("Concatable:concat", "Map"),
                _ => Ok(None),
            }
        } else if class == ClassId::INDEXABLE && method == index {
            match (map, idx_id) {
                (Some(map), Some(idx_id)) => {
                    let value = self
                        .map_lookup_id(&map, idx_id, span)
                        .await?
                        .and_then(|id| self.arena.value(id).cloned())
                        .ok_or_else(|| {
                            Error::runtime(span, "map key not found")
                        })?;
                    Ok(Some(self.value_for_output_value(output, value)))
                }
                _ => Ok(None),
            }
        } else if class == ClassId::INDEXABLE && method == get {
            match (map, idx_id) {
                (Some(map), Some(idx_id)) => {
                    let payload = self
                        .map_lookup_id(&map, idx_id, span)
                        .await?
                        .map(Payload::some)
                        .unwrap_or_else(Payload::none);
                    Ok(Some(self.value_for_output(output, payload)))
                }
                _ => Ok(None),
            }
        } else {
            Ok(None)
        }
    }

    fn indexable_index_value(
        &mut self,
        output_expr_id: Option<ExprId>,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let base_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Indexable:index", "2 args"));
        let idx_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Indexable:index", "2 args"));
        let base = self
            .arena
            .payload(base_id)
            .cloned()
            .unwrap_or_else(|| invariant!("index base in arena"));
        let idx = self
            .arena
            .payload(idx_id)
            .cloned()
            .unwrap_or_else(|| invariant!("index arg in arena"));

        match (&base, &idx) {
            (Payload::Array(elems), Payload::Int(i)) => {
                let index = if *i < 0 {
                    elems.len().checked_sub((-*i) as usize)
                } else {
                    Some(*i as usize)
                };
                index
                    .and_then(|idx| elems.get(idx))
                    .and_then(|id| self.arena.value(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("array index {i} out of bounds"),
                        )
                    })
            }
            (Payload::Map(_), _) => {
                typechecked!("Indexable:index", "async Map index")
            }
            (Payload::String(sid), Payload::Int(i)) => {
                let s = self.arena.get_str(*sid).unwrap_or("");
                let len = s.chars().count() as i64;
                let index = if *i < 0 { len + *i } else { *i };
                s.chars()
                    .nth(index as usize)
                    .map(|c| {
                        self.value_for_optional_expr(
                            output_expr_id,
                            Payload::Char(c),
                        )
                    })
                    .ok_or_else(|| {
                        Error::runtime(
                            span,
                            format!("string index {i} out of bounds"),
                        )
                    })
            }
            _ => typechecked!("Indexable:index", "Array, Map, or String"),
        }
    }

    /// Dispatch to builtin class method implementations.
    ///
    /// Handles all class methods defined in `class.rs`. Sync methods execute
    /// directly; async HOFs use the trampoline pattern.
    fn dispatch_builtin_class_method(
        &mut self,
        dispatch: &ClassDispatch,
    ) -> Result<Payload> {
        let class = dispatch.class;
        let method = dispatch.method;
        let args = dispatch.args.as_slice();
        let span = dispatch.span;
        let val = |i: usize| {
            self.arena
                .payload(args[i])
                .cloned()
                .unwrap_or_else(|| invariant!("class method arg in arena"))
        };

        match self.class_methods.lookup(class, method) {
            Some(MethodFn::Binary(_)) if class == ClassId::ORD => {
                let left =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let right =
                    self.arena.value(args[1]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                Ok(class::Ord::compare_values(&mut ctx, &left, &right))
            }
            Some(MethodFn::Binary(_)) if class == ClassId::EQ => {
                let left =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let right =
                    self.arena.value(args[1]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                Ok(class::Eq::eq_values(&mut ctx, &left, &right))
            }
            Some(MethodFn::Binary(_)) if class == ClassId::CONCATABLE => {
                let left =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let right =
                    self.arena.value(args[1]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                class::Concatable::concat_values(&mut ctx, &left, &right)
            }
            Some(MethodFn::Binary(_)) => {
                let left = val(0);
                let right = val(1);
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_binary(class, method, &mut ctx, &left, &right)
            }
            Some(MethodFn::Unary(_)) if class == ClassId::DISPLAY => {
                let v =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                Ok(class::Display::display_value(&mut ctx, &v))
            }
            Some(MethodFn::Unary(_)) if class == ClassId::FALLIBLE => {
                let v =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                class::Fallible::unwrap_value(&mut ctx, &v)
            }
            Some(MethodFn::Unary(_)) => {
                let v = val(0);
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_unary(class, method, &mut ctx, &v)
            }
            Some(MethodFn::Nullary(_)) => {
                let ty_id = dispatch.output_ty.unwrap_or_else(|| {
                    let id = dispatch
                        .output_expr_id
                        .or(dispatch.dispatch_expr_id)
                        .unwrap_or_else(|| {
                            typechecked!(
                                "nullary class method",
                                "expression id"
                            )
                        });
                    self.checked.expr(id).ty
                });
                let ty_id = self.runtime_ty(ty_id);
                let ty = self.checked.types.get(ty_id).clone();
                let mut ctx = ClassCtx {
                    arena: &mut self.arena,
                    runtime_types: &mut self.checked.types,
                    registry: &self.registry,
                    regex_cache: &self.checked.regex_cache,
                    span,
                };
                self.class_methods
                    .dispatch_nullary(class, method, &mut ctx, &ty)
            }
            Some(MethodFn::Convert(_)) => {
                let v =
                    self.arena.value(args[0]).cloned().unwrap_or_else(|| {
                        invariant!("class method arg in arena")
                    });
                let id = dispatch
                    .output_expr_id
                    .or(dispatch.dispatch_expr_id)
                    .unwrap_or_else(|| {
                        typechecked!("convert class method", "expression id")
                    });
                let edge = dispatch
                    .output_expr_id
                    .and_then(|edge_id| {
                        self.approved_newtype_edge_meta(edge_id)
                    })
                    .or_else(|| self.approved_newtype_edge_meta(id));
                match edge {
                    Some(meta) if class == ClassId::INTO => {
                        Ok(self.value_with_context_meta(v, meta).payload)
                    }
                    Some(meta) if class == ClassId::TRY_INTO => {
                        let val = self.value_with_context_meta(v, meta);
                        Ok(self.make_result_ok_value(val, span))
                    }
                    _ => {
                        let ty_id = self.checked.expr(id).ty;
                        let ty = self.checked.types.get(ty_id).clone();
                        let mut ctx = ClassCtx {
                            arena: &mut self.arena,
                            runtime_types: &mut self.checked.types,
                            registry: &self.registry,
                            regex_cache: &self.checked.regex_cache,
                            span,
                        };
                        match class {
                            ClassId::INTO => {
                                class::Into::into_value(&mut ctx, &v, &ty)
                            }
                            ClassId::TRY_INTO => {
                                class::TryInto::try_into_value(
                                    &mut ctx, &v, &ty,
                                )
                            }
                            _ => self.class_methods.dispatch_convert(
                                class, method, &mut ctx, &v.payload, &ty,
                            ),
                        }
                    }
                }
            }
            Some(MethodFn::Hof(_)) => {
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
        output: OutputMeta,
        starter: hof::MethodFn,
        args: &[ValueId],
        span: Span,
    ) -> Result<Value> {
        let mut ctx = ClassCtx {
            arena: &mut self.arena,
            runtime_types: &mut self.checked.types,
            registry: &self.registry,
            regex_cache: &self.checked.regex_cache,
            span,
        };
        let result = starter(&mut ctx, args)?;

        // Trampoline loop; see doc comment for why we avoid recursion here.
        let mut flow = ControlFlow::Continue(result);
        while let ControlFlow::Continue(result) = flow {
            match result {
                hof::Step::Done(v) => {
                    let output = self.refine_variant_output(output, &v, args);
                    flow = ControlFlow::Break(self.value_for_output(output, v));
                }
                hof::Step::DoneValue(id) => {
                    let value =
                        self.arena.value(id).cloned().unwrap_or_else(|| {
                            invariant!("HoF result in arena")
                        });
                    let output = self.refine_variant_output(
                        output,
                        &value.payload,
                        args,
                    );
                    flow = ControlFlow::Break(
                        self.value_for_output_value(output, value),
                    );
                }
                hof::Step::Compare(cmp) => {
                    let method = self.arena.intern("compare");
                    let value = self
                        .dispatch_class_method_value(ClassDispatch {
                            dispatch_expr_id: None,
                            output_expr_id: None,
                            output_ty: Some(RuntimeTyId::from(
                                TyArena::ORDERING,
                            )),
                            class: ClassId::ORD,
                            method,
                            args: cmp.args.iter().copied().collect(),
                            span,
                        })
                        .await?;
                    let id = self.add_value(value, span);
                    let mut ctx = ClassCtx {
                        arena: &mut self.arena,
                        runtime_types: &mut self.checked.types,
                        registry: &self.registry,
                        regex_cache: &self.checked.regex_cache,
                        span,
                    };
                    flow = ControlFlow::Continue(
                        ctx.resume_compare(cmp.state, id)?,
                    );
                }
                hof::Step::Eq(eq) => {
                    let method = self.arena.intern("eq");
                    let value = self
                        .dispatch_class_method_value(ClassDispatch {
                            dispatch_expr_id: None,
                            output_expr_id: None,
                            output_ty: Some(RuntimeTyId::from(TyArena::BOOL)),
                            class: ClassId::EQ,
                            method,
                            args: eq.args.iter().copied().collect(),
                            span,
                        })
                        .await?;
                    let id = self.add_value(value, span);
                    let mut ctx = ClassCtx {
                        arena: &mut self.arena,
                        runtime_types: &mut self.checked.types,
                        registry: &self.registry,
                        regex_cache: &self.checked.regex_cache,
                        span,
                    };
                    flow = ControlFlow::Continue(ctx.resume_eq(eq.state, id)?);
                }
                hof::Step::ClassCall(call) => {
                    let value = self
                        .dispatch_class_method_value(ClassDispatch {
                            dispatch_expr_id: None,
                            output_expr_id: None,
                            output_ty: call.output_ty,
                            class: call.class,
                            method: call.method,
                            args: call.args.iter().copied().collect(),
                            span,
                        })
                        .await?;
                    let id = self.add_value(value, span);
                    let mut ctx = ClassCtx {
                        arena: &mut self.arena,
                        runtime_types: &mut self.checked.types,
                        registry: &self.registry,
                        regex_cache: &self.checked.regex_cache,
                        span,
                    };
                    flow = ControlFlow::Continue(
                        ctx.resume_class_call(call.state, id)?,
                    );
                }
                hof::Step::Invoke(cont) => {
                    let call_result = self
                        .invoke_callable(cont.callee, &cont.args, span)
                        .await?;
                    let mut ctx = ClassCtx {
                        arena: &mut self.arena,
                        runtime_types: &mut self.checked.types,
                        registry: &self.registry,
                        regex_cache: &self.checked.regex_cache,
                        span,
                    };
                    flow =
                        ControlFlow::Continue(ctx.resume(cont, call_result)?);
                }
            }
        }
        match flow {
            ControlFlow::Break(value) => Ok(value),
            ControlFlow::Continue(_) => invariant!("HoF trampoline completed"),
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
                    .invoke_class_method_fn_value(ClassMethodInvoke {
                        class,
                        method,
                        dispatch_expr_id: expr_id,
                        output_expr_id: None,
                        args: SmallVec::from_slice(args),
                        span,
                    })
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
            runtime_types: &mut self.checked.types,
            io: &mut self.io,
            span,
        };
        let result_id = prim(&mut ctx, arg_ids).await?;

        Ok(self
            .arena
            .value(result_id)
            .cloned()
            .unwrap_or_else(|| invariant!("ValueId in arena")))
    }

    /// Call a function or closure value.
    #[async_recursion]
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
                    self.invoke_class_method_fn_value(ClassMethodInvoke {
                        class,
                        method,
                        dispatch_expr_id: expr_id,
                        output_expr_id: Some(call_id),
                        args: SmallVec::from_slice(&vals),
                        span,
                    })
                    .await
                    .map(|value| {
                        let kind = self
                            .checked
                            .class_registry
                            .lookup_by_name(class)
                            .unwrap_or_else(|| {
                                typechecked!("class method class", "known")
                            });
                        let meta = self.class_method_call_meta(kind, call_id);
                        self.value_with_context_meta(value, meta)
                    })
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
    #[async_recursion]
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
                    self.invoke_class_method_fn_value(ClassMethodInvoke {
                        class,
                        method,
                        dispatch_expr_id: expr_id,
                        output_expr_id: output_expr_id.or(partial_expr_id),
                        args: SmallVec::from_slice(&all_args),
                        span,
                    })
                    .await
                }
                _ => typechecked!("resolve_partial_app", "Callable callee"),
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
