//! Expression type inference.
//!
//! Contains methods for inferring types of expressions: literals, variables,
//! operators, collections, function calls, control flow, etc.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::iter;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::{Constraint, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExpr, AstTypeExprId, BinOp, DbRef, Expr, ExprId,
    Intrinsic, JsonAccessKey, JsonAccessKind, Literal, MatchArm, NumericLit,
    ObjectEntry, PostfixOp, RefTarget, StmtId, SubscriptElem, TransactionExpr,
    TxnId, TypeParam, TypePattern, UnOp, Visibility,
};
use crate::env::TxnReq;
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::env::MethodRefOrigin;
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::Instance;
use crate::typecheck::ty::{
    ClassShape, MethodSpec, Scheme, TrackKind, Ty, TyArena, TyId, TyVar,
    TypeClass,
};
use crate::typecheck::CheckedTypePatternInfo;
use crate::value::{TypeDef, TypeId};
use crate::{ClassId, Span};

impl InferCtx<'_> {
    /// Infer the type of an expression.
    ///
    /// Records the inferred type in `expr_types` and returns it. For undefined
    /// variables, records an error and returns `Ty::Error` for recovery.
    ///
    /// Currently handles Phase 4.3 expressions (literals and variables).
    /// Other expression types will be added in subsequent phases.
    pub(crate) fn expr(&mut self, id: ExprId) -> TyId {
        let span = self.ast.expr_span(id).unwrap_or_default();
        // Clone the expression to avoid borrow issues with mutable ast reference
        let ty = match self.ast.get_expr(id).cloned() {
            None => TyArena::ERROR,
            Some(expr) => self.expr_inner(id, &expr, span),
        };
        self.record_type(id, ty);
        ty
    }

    fn track_concrete_return(&mut self, id: ExprId, ret: TyId) {
        self.interp.set_concrete_expr_ty(id, ret);
    }

    /// Inner expression inference; dispatches on expression variant.
    fn expr_inner(&mut self, id: ExprId, expr: &Expr, span: Span) -> TyId {
        match expr {
            // Literals
            Expr::Literal(lit) => self.literal(id, lit, span),

            // String interpolation: all parts must be Into[String]
            Expr::Interpolation(parts) => self.interpolation(parts, span),

            // Unit: empty tuple
            Expr::Tuple(elems) if elems.is_empty() => TyArena::UNIT,

            // Variable reference
            Expr::Var(name) => self.var(*name, span),

            // Binary operations
            Expr::Binary(lhs, op, rhs) => {
                self.binary(id, *lhs, *op, *rhs, span)
            }

            // Unary operations
            Expr::Unary(op, operand) => self.unary(id, *op, *operand, span),

            // Range expressions
            Expr::Range(start, end, _inclusive) => {
                let start_ty = self.expr(*start);
                let end_ty = self.expr(*end);
                self.unify(start_ty, TyArena::INT, span);
                self.unify(end_ty, TyArena::INT, span);
                TyArena::RANGE
            }

            // Arrays
            Expr::Array(elems) => self.array(elems, span),

            // Non-empty tuples (empty handled above as Unit)
            Expr::Tuple(elems) => self.tuple(elems),

            // Structural objects with potential spreads
            Expr::Object(entries) => self.object(entries, span),

            // Map literals
            Expr::MapLit(entries) => self.map_lit(entries, span),

            // Field access: obj.field
            Expr::Field(base, field) => {
                let f = self.env.get_str(*field).unwrap_or_default().to_owned();
                self.field(id, *base, &f, span)
            }

            // Optional field access: obj?.field
            Expr::OptionalField(base, field) => {
                let f = self.env.get_str(*field).unwrap_or_default().to_owned();
                self.optional_field(*base, &f, span)
            }

            // Tuple index: tuple.0, tuple.1, etc.
            Expr::TupleIndex(base, idx) => self.tuple_index(*base, *idx, span),

            // Index access: arr[i] or map[k]
            Expr::Index(base, idx) => self.index(*base, *idx, span),

            // Optional index access: arr?[i] or str?[i] (safe, returns Option)
            Expr::OptionalIndex(base, idx) => {
                self.optional_index(*base, *idx, span)
            }

            // JSON access: data.field, data..field, data->"key", data->>"key"
            Expr::JsonAccess(base, kind, key) => {
                self.json_access(*base, kind, key, span)
            }

            // JSON literals
            Expr::Json(fields) => {
                fields.iter().for_each(|(_, expr)| {
                    self.expr(*expr);
                });
                TyArena::JSON
            }

            // Closures: (x, y) => body or [T](x: T) -> T => body
            Expr::Closure {
                type_params,
                params,
                ret,
                body,
            } => {
                self.closure(id, type_params, params, ret.as_ref(), *body, span)
            }

            // Function calls: f(args...)
            Expr::Call(callee, args) => {
                self.call_or_variant(id, *callee, args, span)
            }

            // Control flow: IF
            Expr::If(cond, then_br, else_br) => {
                self.r#if(*cond, *then_br, else_br.as_ref().copied(), span)
            }

            // Control flow: blocks
            Expr::Block(stmts, tail) => {
                self.block(stmts, tail.as_ref().copied())
            }

            // Control flow: match
            Expr::Match(scrutinee, arms) => {
                self.r#match(*scrutinee, arms, span)
            }

            // Variant constructors
            Expr::Variant(ty_name, var_name, args) => {
                self.variant(id, ty_name.clone(), *var_name, args, span)
            }

            // Naked variant constructors
            Expr::NakedVariant(var_name, args) => {
                self.naked_variant(id, *var_name, args, span)
            }

            // Postfix operators: `!`
            Expr::Postfix(op, inner) => self.postfix(id, *op, *inner, span),

            // Type check: `expr is Pattern`
            Expr::Is(scrutinee, pattern) => {
                self.is_check(id, *scrutinee, pattern, span)
            }

            // Type cast: `expr as Type`
            Expr::As(inner, ty_id) => self.as_cast(id, *inner, *ty_id, span),

            // Fallible conversion: `expr read Type`
            Expr::Read(inner, ty_id) => {
                self.read_conv(id, *inner, *ty_id, span)
            }

            // Database intrinsics: `@get`, `@set`, `@kill`, `@data`, `@order`, `@query`
            Expr::Intrinsic(op, ref rt, val, _) => {
                self.intrinsic(id, *op, rt, val.as_ref().copied(), span)
            }

            // Type annotation: `(expr) : Type`
            Expr::Annotate(inner, ty_id) => {
                self.annotate(id, *inner, *ty_id, span)
            }

            // Module path: `Module.function` or `Module.constant`
            Expr::Path(segments) => {
                // Look up the type from the runtime environment;
                // first check constants, then functions
                // Check builtin module constants first (no constraints)
                if let Some(ty) =
                    self.runtime_env.get_module_const_type(segments)
                {
                    ty
                } else if let Some(scheme) =
                    self.runtime_env.get_module_fn_type(segments)
                {
                    // Builtin module functions (no user constraints)
                    let (ty, constraints) =
                        scheme.instantiate(&mut self.uf, &mut self.ty_arena);
                    self.emit_class_constraints(constraints, span);
                    ty
                } else {
                    // Split segments into module path (all but last) and member (last)
                    let mod_qn = QualifiedName::new(
                        segments[..segments.len().saturating_sub(1)].to_vec(),
                    );
                    let member_id = segments
                        .last()
                        .copied()
                        .unwrap_or_else(|| invariant!("path has segments"));
                    match self.env.lookup_user_module_member(&mod_qn, member_id)
                    {
                        Some(member) => {
                            // Check visibility; private members cannot be accessed
                            // from outside the module
                            if member.vis == Visibility::Private {
                                let module = mod_qn.display(&self.env.strings);
                                let name = self.env.resolve_string(member_id);
                                self.error(TypeError::PrivateAccess {
                                    module,
                                    name,
                                    span,
                                });
                                TyArena::ERROR
                            } else {
                                // Public member; instantiate and use
                                let scheme = member.scheme.clone();
                                let (ty, constraints) = scheme.instantiate(
                                    &mut self.uf,
                                    &mut self.ty_arena,
                                );
                                self.emit_class_constraints(constraints, span);
                                self.hoist.record_forward_ref(
                                    &scheme,
                                    ty,
                                    span,
                                    self.current_module.clone(),
                                );
                                ty
                            }
                        }
                        None => {
                            // Path resolved as module but member not found
                            let module = mod_qn.display(&self.env.strings);
                            let name = self.env.resolve_string(member_id);
                            self.error(TypeError::NotFoundInModule {
                                module,
                                name,
                                span,
                            });
                            TyArena::ERROR
                        }
                    }
                }
            }

            // Regex literal: `/pattern/`
            Expr::Regex(pattern, _) => {
                // Compile and cache the regex pattern; invalid patterns
                // produce a type error during compile_regex
                self.compile_regex(pattern, span)
                    .map(|idx| self.interp.set_regex_index(id, idx));
                TyArena::REGEX
            }

            // Regex match: `expr MATCHES regex`
            Expr::Matches(lhs, rhs) => {
                let lhs_ty = self.expr(*lhs);
                let rhs_ty = self.expr(*rhs);

                // LHS must be convertible to String
                self.constrain(Constraint::Class {
                    ty: lhs_ty,
                    class: TypeClass::param(ClassId::INTO, TyArena::STRING),
                    span,
                });

                // RHS must be Regex
                self.unify(rhs_ty, TyArena::REGEX, span);

                TyArena::BOOL
            }

            // Catch expression: `expr CATCH handler`
            Expr::Catch(expr_id, handler_id) => {
                let expr_ty = self.expr(*expr_id);
                let handler_ty = self.expr(*handler_id);

                // Handler must be `(Error) -> T` where `T` matches expr type
                let expected = self
                    .ty_arena
                    .func(smallvec![TyArena::RUNTIME_ERROR], expr_ty);
                self.unify(handler_ty, expected, span);

                expr_ty
            }

            // Write expression: `write expr [JSON] [TO target]`
            // Same typing as statement version, but returns `Unit`
            Expr::Write(output) => {
                self.write(output, span);
                TyArena::UNIT
            }

            // Raise expression: `raise expr`
            // Never returns; can unify with any expected type.
            Expr::Raise(inner) => {
                let ty = self.expr(*inner);
                // Error message must be convertible to String
                self.constrain(Constraint::Class {
                    ty,
                    class: TypeClass::param(ClassId::INTO, TyArena::STRING),
                    span,
                });
                self.fresh()
            }

            // Loop expression: `loop seed (state, cont) => body`
            Expr::Loop {
                seed,
                state_param,
                cont_param,
                body,
            } => self.loop_expr(*seed, state_param, cont_param, *body, span),

            // Transaction block: `transaction { ... }`
            Expr::Transaction(ref txn) => self.transaction(id, txn, span),

            // Default value: `_`.
            //
            // Creates a fresh type variable with the current default value constraint.
            // The concrete type is inferred from context (e.g., `_ ++ [1]` infers `Array[Int]`).
            // Store the type variable for later resolution.
            Expr::DefaultValue => {
                let tv = self.fresh();
                self.constrain(Constraint::Class {
                    ty: tv,
                    class: TypeClass::simple(ClassId::DEFAULT),
                    span,
                });
                self.interp.set_concrete_expr_ty(id, tv);
                tv
            }

            // Ref literal: `data{1, 2}` or `^global{key}`
            // Creates a first-class `Local` or `Global` type.
            Expr::Ref(ref dbref) => match dbref {
                DbRef::Local(_, subs) => {
                    self.check_subscript_elems(subs, span);
                    TyArena::LOCAL
                }
                DbRef::Global(_, subs) => {
                    self.check_subscript_elems(subs, span);
                    TyArena::GLOBAL
                }
            },

            // Class method call: `Class:method(args)`
            Expr::ClassMethod(class, method, args) => {
                self.class_method(id, *class, *method, args, span)
            }

            // Class method reference: `Class:method` or `Class[T]:method`
            Expr::ClassMethodRef(class, type_args, method) => {
                self.class_method_ref(id, *class, type_args, *method, span)
            }

            // Naked class method call: `:method(args)`
            Expr::NakedClassMethod(method, args) => {
                self.naked_class_method(id, *method, args, span)
            }

            // Naked class method reference: `:method`
            Expr::NakedClassMethodRef(method) => {
                self.naked_class_method_ref(id, *method, span)
            }
        }
    }

    /// Type check a class method call.
    ///
    /// Validates the class and method names, checks argument types, and returns
    /// the result type.
    fn class_method(
        &mut self,
        id: ExprId,
        class: StringId,
        method: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        match self.env.class_registry().lookup_by_name(class) {
            Some(k) => {
                self.call_class_method_generic(id, k, method, args, span)
            }
            None => {
                let cn = self.env.resolve_str(class).to_owned();
                self.error(TypeError::UnknownClass(cn, span));
                TyArena::ERROR
            }
        }
    }

    /// Type check a class method reference (used as a first-class value).
    ///
    /// Returns the polymorphic function type of the method. Convert methods
    /// (`Wrappable:wrap`, `Into:into`, `TryInto:try-into`) require type arguments.
    fn class_method_ref(
        &mut self,
        id: ExprId,
        class: StringId,
        type_args: &SmallVec<[AstTypeExprId; 2]>,
        method: StringId,
        span: Span,
    ) -> TyId {
        let empty_subst = IndexMap::new();

        match self.env.class_registry().lookup_by_name(class) {
            Some(k) => {
                match self.env.class_def(k).method(method, span).cloned() {
                    Err(e) => {
                        self.error(e);
                        TyArena::ERROR
                    }
                    Ok(spec) => {
                        // Check if convert method requires type args
                        let needs_type_arg = matches!(
                            spec,
                            MethodSpec::Tracked {
                                track: TrackKind::Convert
                                    | TrackKind::ConvertResultInner,
                                ..
                            }
                        );
                        if needs_type_arg && type_args.is_empty() {
                            let cn = self.env.resolve_str(class).to_owned();
                            let mn = self.env.resolve_str(method).to_owned();
                            self.error(TypeError::ConvertMethodNeedsType {
                                class: cn,
                                method: mn,
                                span,
                            });
                            TyArena::ERROR
                        } else {
                            match spec {
                                MethodSpec::Standard(scheme) => {
                                    let (ty, constraints) = scheme.instantiate(
                                        &mut self.uf,
                                        &mut self.ty_arena,
                                    );
                                    if let (
                                        Some(&arg_id),
                                        Some(&(recv, ref class)),
                                    ) =
                                        (type_args.first(), constraints.first())
                                    {
                                        let target = match class {
                                            TypeClass::Concrete {
                                                ref params,
                                                ..
                                            } => params
                                                .first()
                                                .copied()
                                                .unwrap_or(recv),
                                            _ => recv,
                                        };
                                        let arg_ty =
                                            self.convert().ast_type_to_ty(
                                                arg_id,
                                                &empty_subst,
                                            );
                                        self.unify(target, arg_ty, span);
                                        if matches!(
                                            class,
                                            TypeClass::Concrete { ref params, .. } if !params.is_empty()
                                        ) && k.idx()
                                            >= ClassId::BUILTIN_COUNT
                                        {
                                            self.deferred_param_calls.push((
                                                id, k, method, recv, arg_ty,
                                            ));
                                        }
                                    }
                                    self.emit_class_constraints(
                                        constraints,
                                        span,
                                    );
                                    ty
                                }

                                MethodSpec::Tracked {
                                    track:
                                        track @ (TrackKind::Default
                                        | TrackKind::Zero
                                        | TrackKind::One),
                                    ..
                                } => {
                                    let class = match track {
                                        TrackKind::Default => ClassId::DEFAULT,
                                        TrackKind::Zero => ClassId::ADDITIVE,
                                        TrackKind::One => {
                                            ClassId::MULTIPLICATIVE
                                        }
                                        TrackKind::Convert
                                        | TrackKind::ConvertResultInner => {
                                            typechecked!(
                                                "nullary tracked method",
                                                "non-convert track"
                                            )
                                        }
                                    };
                                    let tv = if let Some(&ty_id) =
                                        type_args.first()
                                    {
                                        self.convert()
                                            .ast_type_to_ty(ty_id, &empty_subst)
                                    } else {
                                        self.fresh()
                                    };
                                    self.constrain(Constraint::Class {
                                        ty: tv,
                                        class: TypeClass::simple(class),
                                        span,
                                    });
                                    if !type_args.is_empty() {
                                        self.track_concrete_return(id, tv);
                                    }
                                    self.ty_arena.func(smallvec![], tv)
                                }

                                MethodSpec::Tracked {
                                    track: TrackKind::Convert,
                                    ..
                                } => {
                                    // Type arg is required (checked above).
                                    let target_ty_id = *type_args
                                        .first()
                                        .unwrap_or_else(|| {
                                            typechecked!(
                                                "convert method",
                                                "type argument"
                                            )
                                        });
                                    let target_ty =
                                        self.convert().ast_type_to_ty(
                                            target_ty_id,
                                            &empty_subst,
                                        );
                                    self.interp.set_expr_ty(id, target_ty);

                                    let input_var = self.fresh_var();
                                    let input_ty =
                                        self.ty_arena.alloc(Ty::Var(input_var));

                                    let ms = self.env.resolve_str(method);
                                    match (k, ms) {
                                        (ClassId::WRAPPABLE, "wrap") => {
                                            self.constrain(Constraint::Class {
                                                ty: target_ty,
                                                class: TypeClass::hkt_elem(
                                                    ClassId::WRAPPABLE,
                                                    input_ty,
                                                ),
                                                span,
                                            });
                                            self.ty_arena.func(
                                                smallvec![input_ty],
                                                target_ty,
                                            )
                                        }
                                        (ClassId::INTO, "into") => {
                                            let class = TypeClass::param(
                                                ClassId::INTO,
                                                target_ty,
                                            );
                                            self.constrain(Constraint::Class {
                                                ty: input_ty,
                                                class: class.clone(),
                                                span,
                                            });
                                            let fn_ty = self.ty_arena.func(
                                                smallvec![input_ty],
                                                target_ty,
                                            );
                                            self.newtype_edge_checks.push((
                                                id,
                                                input_ty,
                                                target_ty,
                                                span,
                                                self.current_module.clone(),
                                            ));
                                            fn_ty
                                        }
                                        _ => {
                                            self.error(TypeError::Custom {
                                            msg: "Convert track for unknown method"
                                                .into(),
                                            span,
                                        });
                                            TyArena::ERROR
                                        }
                                    }
                                }

                                MethodSpec::Tracked {
                                    track: TrackKind::ConvertResultInner,
                                    ..
                                } => {
                                    let target_ty_id = *type_args
                                        .first()
                                        .unwrap_or_else(|| {
                                            typechecked!(
                                                "try-into method",
                                                "type argument"
                                            )
                                        });
                                    let target_ty =
                                        self.convert().ast_type_to_ty(
                                            target_ty_id,
                                            &empty_subst,
                                        );
                                    self.interp.set_expr_ty(id, target_ty);

                                    let input_var = self.fresh_var();
                                    let input_ty =
                                        self.ty_arena.alloc(Ty::Var(input_var));
                                    let class = TypeClass::param(
                                        ClassId::TRY_INTO,
                                        target_ty,
                                    );
                                    self.constrain(Constraint::Class {
                                        ty: input_ty,
                                        class: class.clone(),
                                        span,
                                    });
                                    let ret_ty = self
                                        .ty_arena
                                        .result(target_ty, TyArena::STRING);
                                    self.ty_arena
                                        .func(smallvec![input_ty], ret_ty)
                                }
                            }
                        }
                    }
                }
            }
            None => {
                let cn = self.env.resolve_str(class).to_owned();
                self.error(TypeError::UnknownClass(cn, span));
                TyArena::ERROR
            }
        }
    }

    /// Resolve a naked class method call (`:method(args)`).
    ///
    /// Searches all classes for a unique method match. If exactly one class
    /// defines the method, delegates to `class_method`. Otherwise, emits an
    /// ambiguity or unknown-method error.
    fn naked_class_method(
        &mut self,
        id: ExprId,
        method: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        match self.resolve_naked_method(method, span) {
            Some((k, class_name)) => {
                self.interp.set_naked_method(id, class_name);
                self.call_class_method_generic(id, k, method, args, span)
            }
            None => TyArena::ERROR,
        }
    }

    /// Resolve a naked class method reference (`:method`).
    fn naked_class_method_ref(
        &mut self,
        id: ExprId,
        method: StringId,
        span: Span,
    ) -> TyId {
        match self.resolve_naked_method(method, span) {
            Some((_k, class_name)) => {
                self.interp.set_naked_method(id, class_name);
                let empty = SmallVec::new();
                self.class_method_ref(id, class_name, &empty, method, span)
            }
            None => TyArena::ERROR,
        }
    }

    /// Search all classes for a method name; returns `Some((ClassId, class StringId))`
    /// if exactly one class defines it, or `None` after emitting an error.
    fn resolve_naked_method(
        &mut self,
        method: StringId,
        span: Span,
    ) -> Option<(ClassId, StringId)> {
        let matches = self.env.class_registry().lookup_by_method(method);
        match matches.as_slice() {
            [] => {
                let mn = self.env.resolve_str(method).to_owned();
                self.error(TypeError::UnknownNakedMethod(mn, span));
                None
            }
            &[k] => {
                let name = self.env.class_registry().name(k);
                Some((k, name))
            }
            _ => {
                let mn = self.env.resolve_str(method).to_owned();
                let classes = matches
                    .iter()
                    .map(|&k| {
                        self.env
                            .resolve_str(self.env.class_registry().name(k))
                            .to_owned()
                    })
                    .collect();
                self.error(TypeError::AmbiguousNakedMethod {
                    method: mn,
                    classes,
                    span,
                });
                None
            }
        }
    }

    /// Emit a class constraint for a type.
    fn emit_class_constraint(
        &mut self,
        ty: TyId,
        class: TypeClass<TyId>,
        span: Span,
    ) {
        self.constrain(Constraint::Class { ty, class, span });
    }

    /// Check if a class instance is available in the current scope.
    ///
    /// An instance is available if:
    /// - It is top-level (no module), or
    /// - Its owning module has been imported
    ///
    /// Returns the cloned instance if available, `None` otherwise. If the
    /// instance exists but its module is not imported, emits an error.
    fn check_instance_available(
        &mut self,
        class: ClassId,
        type_id: TypeId,
        span: Span,
    ) -> Option<Instance> {
        let inst = self.instance_registry.lookup(class, type_id)?.clone();
        match inst.module {
            None => Some(inst),
            Some(ref mod_qn) => {
                // Check if the module's root segment has been imported
                let imported = self.env.is_module_imported(
                    *mod_qn
                        .segments()
                        .first()
                        .unwrap_or_else(|| invariant!("module has segments")),
                );
                if imported {
                    Some(inst)
                } else {
                    self.error(TypeError::InstanceNotImported {
                        class,
                        type_id,
                        module: mod_qn.display(&self.env.strings),
                        span,
                    });
                    None
                }
            }
        }
    }

    /// Generic class method call type checking.
    ///
    /// Uses the centralized spec from `ClassId::method` to:
    /// 1. Check arity
    /// 2. Instantiate the scheme with fresh type variables
    /// 3. Unify argument types with parameter types
    /// 4. Emit class constraints
    /// 5. Handle type tracking for runtime dispatch
    /// 6. Return the result type
    fn call_class_method_generic(
        &mut self,
        id: ExprId,
        kind: ClassId,
        method: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        match self.env.class_def(kind).method(method, span).cloned() {
            Err(e) => {
                self.error(e);
                TyArena::ERROR
            }
            Ok(spec) => {
                let scheme = spec.scheme();
                let expected_arity = scheme.arity(&self.ty_arena).unwrap_or(0);

                if args.len() > expected_arity {
                    self.error(TypeError::TooManyArguments {
                        expected: expected_arity,
                        got: args.len(),
                        span,
                    });
                    TyArena::ERROR
                } else if args.is_empty() && expected_arity > 0 {
                    self.error(TypeError::ZeroArguments {
                        expected: expected_arity,
                        span,
                    });
                    TyArena::ERROR
                } else {
                    let is_partial = args.len() < expected_arity;

                    // Instantiate scheme with fresh type variables
                    let (fn_ty, constraints) =
                        scheme.instantiate(&mut self.uf, &mut self.ty_arena);

                    // Extract params and return type (copy out before further arena use)
                    let (params, ret) = match self.ty_arena.get(fn_ty) {
                        Ty::Fn(p, r) => (p.clone(), *r),
                        _ => {
                            self.error(TypeError::Custom {
                                msg:
                                    "class method scheme is not a function type"
                                        .into(),
                                span,
                            });
                            (smallvec![], TyArena::ERROR)
                        }
                    };

                    // Infer argument types and unify with prefix of params
                    let arg_tys: SmallVec<[TyId; 4]> = args
                        .iter()
                        .zip(params.iter())
                        .map(|(arg, &param)| {
                            let arg_ty = self.expr(*arg);
                            self.unify(arg_ty, param, span);
                            arg_ty
                        })
                        .collect();

                    // For HKT classes, extract the container type
                    // variable from the constraint BEFORE consuming them.
                    // The constraint `(ty, Hkt { id, .. })` where `id`
                    // matches the current class has `ty` pointing to the
                    // HKT constructor variable, which resolves to the
                    // container type after constraint solving.
                    let hkt_container_ty = if matches!(
                        self.env.class_registry().shape(kind),
                        ClassShape::Hkt { .. }
                    ) {
                        constraints.iter().find_map(|(ty, c)| match c {
                            TypeClass::Hkt { id, .. } if *id == kind => {
                                Some(*ty)
                            }
                            _ => None,
                        })
                    } else {
                        None
                    };

                    // Emit class constraints from scheme
                    constraints.into_iter().for_each(|(ty, class)| {
                        self.emit_class_constraint(ty, class, span);
                    });

                    // Track user instance calls for dispatch.
                    //
                    // For HKT classes, the instance is keyed by the
                    // container constructor variable from the class
                    // constraint, not the return type. This is correct
                    // for ALL HKT methods regardless of whether the
                    // return type contains the container (`Mappable:map`
                    // returns `F[B]`) or not (`Filterable:filter` returns
                    // `Array[T]`, `Iterable:length` returns `Int`).
                    {
                        let lookup_ty = hkt_container_ty
                            .or_else(|| arg_tys.first().copied());
                        if let Some(ty) = lookup_ty {
                            let type_id = self.nominal_type_id(ty);
                            match type_id {
                                Some(tid)
                                    if self
                                        .check_instance_available(
                                            kind, tid, span,
                                        )
                                        .is_some() =>
                                {
                                    self.set_instance_call(id, tid);
                                }
                                _ => {
                                    self.deferred_inst_calls
                                        .push((id, ty, kind));
                                }
                            }
                        }

                        // For parameterized user classes, record info for
                        // deferred resolution of the specific instance fn.
                        let is_param_user = matches!(
                            self.env.class_registry().shape(kind),
                            ClassShape::Concrete { params } if params > 0
                        ) && kind.idx()
                            >= ClassId::BUILTIN_COUNT;
                        if is_param_user {
                            if let Some(ty) = arg_tys.first().copied() {
                                self.deferred_param_calls
                                    .push((id, kind, method, ty, ret));
                            }
                        }

                        // For user HKT classes, record for tuple arity
                        // disambiguation after constraint solving.
                        let is_hkt_user = matches!(
                            self.env.class_registry().shape(kind),
                            ClassShape::Hkt { .. }
                        ) && kind.idx()
                            >= ClassId::BUILTIN_COUNT;
                        if is_hkt_user {
                            if let Some(ty) = lookup_ty {
                                self.deferred_hkt_user_calls
                                    .push((id, kind, method, ty));
                            }
                        }
                    }

                    // Track output type for `bimap` calls
                    if kind == ClassId::BIMAPPABLE {
                        self.interp.set_hof_call(id, ret);
                    }

                    // Handle tracking for runtime dispatch
                    match spec {
                        MethodSpec::Standard(_) => {}
                        MethodSpec::Tracked { track, .. } => match track {
                            TrackKind::Default
                            | TrackKind::Zero
                            | TrackKind::One => {
                                self.track_concrete_return(id, ret);
                            }
                            TrackKind::Convert => {
                                self.interp.set_expr_ty(id, ret);
                                if !is_partial {
                                    if let Some(from) = arg_tys.first().copied()
                                    {
                                        self.newtype_edge_checks.push((
                                            id,
                                            from,
                                            ret,
                                            span,
                                            self.current_module.clone(),
                                        ));
                                    }
                                }
                            }
                            TrackKind::ConvertResultInner => {
                                // Return type is `Result[T, E]`; track inner `T`
                                let inner = match self.ty_arena.get(ret) {
                                    Ty::Result(ok, _) => *ok,
                                    _ => {
                                        self.error(TypeError::Custom {
                                            msg:
                                            "ConvertResultInner expects Result type"
                                                .into(),
                                            span,
                                        });
                                        TyArena::ERROR
                                    }
                                };
                                self.interp.set_expr_ty(id, inner);
                            }
                        },
                    }

                    if is_partial {
                        // Residual function type from remaining params
                        let remaining: SmallVec<[TyId; 4]> =
                            params[args.len()..].iter().copied().collect();
                        self.ty_arena.func(remaining, ret)
                    } else {
                        ret
                    }
                }
            }
        }
    }

    /// Infer type of a literal expression.
    ///
    /// Integer literals are polymorphic: they get a fresh type variable
    /// (WITHOUT a constraint) that can unify with any type. This allows:
    /// - `d(10)` where `d: Word -> Word` (10 unifies with Word)
    /// - `[1, "a", true]` (1 unifies with Json via heterogeneous array)
    ///
    /// Unresolved integer type variables default to `Int` during constraint
    /// solving. Arithmetic operators emit their matching capability constraints,
    /// such as `Additive` for `+`.
    ///
    /// Float literals are NOT polymorphic; they are always `Float`.
    ///
    /// Records the expression ID for integer literals so the interpreter
    /// can convert them to the correct runtime type.
    pub(super) fn literal(
        &mut self,
        id: ExprId,
        lit: &Literal,
        span: Span,
    ) -> TyId {
        match lit {
            Literal::Bool(_) => TyArena::BOOL,
            Literal::Numeric(NumericLit::Int(_)) => {
                // Polymorphic integer literal: fresh var with Numeric constraint.
                // The Numeric constraint prevents unification with incompatible
                // types (e.g., Tuple in `let (a, b) = 42`).
                //
                // The constraint is checked after solving, allowing the type var
                // to unify with unions containing numeric members. If it ends up
                // bound to a non-numeric type, check_numeric will error.
                //
                // Note: join_types handles IF/match branches specially, resolving
                // numeric vars to Int for union creation (avoiding the var being
                // bound to a sibling branch's type like String).
                let ty = self.fresh_numeric();
                self.constrain(Constraint::Class {
                    ty,
                    class: TypeClass::simple(ClassId::NUMERIC),
                    span,
                });
                self.interp.set_expr_ty(id, ty);
                ty
            }
            // Float literals are NOT polymorphic; always Float
            Literal::Numeric(NumericLit::Float(_)) => TyArena::FLOAT,
            Literal::Char(_) => TyArena::CHAR,
            Literal::String(_) => TyArena::STRING,
            Literal::Null => TyArena::JSON,
            Literal::Unit => TyArena::UNIT,
        }
    }

    /// Infer type of a literal in a pattern context.
    ///
    /// Similar to `literal`, but for patterns where there is no `ExprId`.
    /// Integer literals are polymorphic; float literals are `Float`.
    /// The interpreter compares pattern literals against the scrutinee
    /// directly, so no runtime type conversion is needed.
    pub(super) fn pattern_literal(
        &mut self,
        lit: &Literal,
        span: Span,
    ) -> TyId {
        match lit {
            Literal::Bool(_) => TyArena::BOOL,
            Literal::Numeric(NumericLit::Int(_)) => {
                // Polymorphic integer literal in pattern context with Numeric
                // constraint. Will unify with scrutinee type; constraint ensures
                // the scrutinee is numeric-compatible.
                let ty = self.fresh_numeric();
                self.constrain(Constraint::Class {
                    ty,
                    class: TypeClass::simple(ClassId::NUMERIC),
                    span,
                });
                ty
            }
            Literal::Numeric(NumericLit::Float(_)) => TyArena::FLOAT,
            Literal::Char(_) => TyArena::CHAR,
            Literal::String(_) => TyArena::STRING,
            Literal::Null => TyArena::JSON,
            Literal::Unit => TyArena::UNIT,
        }
    }

    /// Infer type of string interpolation.
    ///
    /// All expression parts (odd indices) must be convertible to `String`.
    /// Literal parts (even indices) are already strings. Returns `String`.
    fn interpolation(&mut self, parts: &[ExprId], span: Span) -> TyId {
        parts.iter().enumerate().for_each(|(i, &part_id)| {
            let part_ty = self.expr(part_id);
            // Odd indices are expressions; they must be convertible to String
            // Even indices are string literals; no constraint needed
            if i % 2 == 1 {
                self.constrain(Constraint::Class {
                    ty: part_ty,
                    class: TypeClass::param(ClassId::INTO, TyArena::STRING),
                    span,
                });
            }
        });
        TyArena::STRING
    }

    /// Infer type of a variable reference.
    ///
    /// Looks up the variable in the type environment and instantiates its
    /// scheme with fresh type variables. If undefined, records an error
    /// and returns `Ty::Error`.
    fn var(&mut self, name: StringId, span: Span) -> TyId {
        match self.env.lookup(name) {
            Some(scheme) => {
                let scheme = scheme.clone();
                let (ty, constraints) =
                    scheme.instantiate(&mut self.uf, &mut self.ty_arena);
                self.emit_class_constraints(constraints, span);
                // Phase 4: record forward-ref instantiation if this
                // lookup hit a not-yet-finalized hoisted scheme.
                self.hoist.record_forward_ref(
                    &scheme,
                    ty,
                    span,
                    self.current_module.clone(),
                );
                ty
            }
            None => {
                self.error(TypeError::UndefinedVar(
                    self.env.resolve_string(name),
                    span,
                ));
                TyArena::ERROR
            }
        }
    }

    /// Apply an operator's type scheme to operands.
    ///
    /// Instantiates the scheme, unifies operands with parameter types,
    /// emits constraints from the scheme, and returns the result type.
    fn apply_op_scheme(
        &mut self,
        scheme: &Scheme,
        args: &[TyId],
        span: Span,
    ) -> TyId {
        let (fn_ty, constraints) =
            scheme.instantiate(&mut self.uf, &mut self.ty_arena);
        self.emit_class_constraints(constraints, span);

        // Copy out params/ret before further arena mutation
        match self.ty_arena.get(fn_ty) {
            Ty::Fn(params, ret) => {
                let params = params.clone();
                let ret = *ret;
                params.iter().zip(args.iter()).for_each(|(&param, &arg)| {
                    self.unify(arg, param, span);
                });
                ret
            }
            _ => {
                self.error(TypeError::Custom {
                    msg: "operator scheme must be function type".into(),
                    span,
                });
                TyArena::ERROR
            }
        }
    }

    /// Infer type of a binary operation.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type. Operands must satisfy the scheme's constraints.
    fn binary(
        &mut self,
        id: ExprId,
        lhs_id: ExprId,
        op: BinOp,
        rhs_id: ExprId,
        span: Span,
    ) -> TyId {
        let lhs_ty = self.expr(lhs_id);
        let rhs_ty = self.expr(rhs_id);

        let lhs_is_ref = self.ty_arena.get(lhs_ty).is_ref();
        let rhs_is_ref = self.ty_arena.get(rhs_ty).is_ref();

        let ret = match op {
            // FIXME: Special case for Ref comparison. This allows comparing
            // `Local` and `Global` refs (e.g., `data{1} == ^info{"key"}`).
            // Once union types are properly represented in the interpreter
            // (as `Value::Union` wrapping the underlying value), this special
            // case can be removed. The type checker will naturally allow
            // `Ref = Local | Global` comparisons via the union support in
            // `satisfies_class`, and the interpreter will unwrap the union
            // wrappers to compare the underlying ref values.
            BinOp::Eq | BinOp::Ne => {
                if lhs_is_ref && rhs_is_ref {
                    // Both are refs (Local/Global), but don't unify them.
                    // Just require both satisfy Eq and return Bool.
                    self.constrain(Constraint::Class {
                        ty: lhs_ty,
                        class: TypeClass::simple(ClassId::EQ),
                        span,
                    });
                    self.constrain(Constraint::Class {
                        ty: rhs_ty,
                        class: TypeClass::simple(ClassId::EQ),
                        span,
                    });
                    TyArena::BOOL
                } else {
                    // Not refs, use normal type scheme (which unifies types)
                    let scheme = op.def(&mut self.ty_arena).ty;
                    self.apply_op_scheme(&scheme, &[lhs_ty, rhs_ty], span)
                }
            }

            // Pipe needs Callable constraint for polymorphic callables
            BinOp::Pipe => {
                let result = self.fresh();
                let args = smallvec::smallvec![lhs_ty];
                self.constrain(Constraint::Callable {
                    callee: rhs_ty,
                    args: args.clone(),
                    ret: result,
                    span,
                });
                self.record_convert_ref_newtype_edge(id, rhs_id, &args, span);
                self.record_method_ref_call(
                    id, rhs_id, rhs_ty, &args, result, span,
                );
                result
            }

            // All other operators use their type schemes
            _ => {
                let scheme = op.def(&mut self.ty_arena).ty;
                self.apply_op_scheme(&scheme, &[lhs_ty, rhs_ty], span)
            }
        };

        // Track user instance calls for binary operators that dispatch to
        // class methods. This mirrors what `class_method()` does for explicit
        // `Class:method(...)` calls.
        //
        // Skip when inside a class instance body for the same (class, type);
        // otherwise operators like `a + b` inside `class Additive FOR MyInt`
        // would recurse infinitely instead of auto-deriving from the inner type.
        let class_tag = op.class_dispatch().map(|(tag, _)| tag);

        if let Some(kind) = class_tag {
            let type_id = self.nominal_type_id(lhs_ty);

            // Suppress if we're inside the class instance for this exact
            // (class, type) combination to prevent infinite recursion.
            let inside_same = self
                .class_context
                .as_ref()
                .is_some_and(|ctx| ctx.class == kind && ctx.type_id == type_id);

            if !inside_same {
                match type_id {
                    Some(tid)
                        if self
                            .check_instance_available(kind, tid, span)
                            .is_some() =>
                    {
                        self.set_instance_call(id, tid);
                    }
                    _ => {
                        self.deferred_inst_calls.push((id, lhs_ty, kind));
                    }
                }
            }
        }

        // Track Fallible instance for `??` (coalesce) dispatch on user types.
        // Coalesce is not in `class_dispatch()` so needs separate handling.
        if matches!(op, BinOp::Coalesce) {
            let kind = ClassId::FALLIBLE;
            let type_id = self.nominal_type_id(lhs_ty);
            let inside_same = self
                .class_context
                .as_ref()
                .is_some_and(|ctx| ctx.class == kind && ctx.type_id == type_id);
            if !inside_same {
                match type_id {
                    Some(tid)
                        if self
                            .check_instance_available(kind, tid, span)
                            .is_some() =>
                    {
                        self.set_instance_call(id, tid);
                    }
                    _ => {
                        self.deferred_inst_calls.push((id, lhs_ty, kind));
                    }
                }
            }
        }

        ret
    }

    /// Infer type of a unary operation.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type. For `?` (wrap), tracks the result type for interpreter
    /// dispatch to either `Option.Some` or `Result.Ok`.
    fn unary(
        &mut self,
        id: ExprId,
        op: UnOp,
        operand_id: ExprId,
        span: Span,
    ) -> TyId {
        let operand_ty = self.expr(operand_id);
        let scheme = op.def(&mut self.ty_arena).ty;
        let result = self.apply_op_scheme(&scheme, &[operand_ty], span);

        if matches!(op, UnOp::Wrap) {
            self.interp.set_expr_ty(id, result);

            // Track Wrappable instance for `?` dispatch on user types.
            // Check the result type (e.g. `Box[T]`) for a Wrappable instance.
            let kind = ClassId::WRAPPABLE;
            let type_id = self.nominal_type_id(result);
            let inside_same = self
                .class_context
                .as_ref()
                .is_some_and(|ctx| ctx.class == kind && ctx.type_id == type_id);
            if !inside_same {
                match type_id {
                    Some(tid)
                        if self
                            .check_instance_available(kind, tid, span)
                            .is_some() =>
                    {
                        self.set_instance_call(id, tid);
                    }
                    _ => {
                        self.deferred_inst_calls.push((id, result, kind));
                    }
                }
            }
        }

        result
    }

    /// Infer type of an array literal with potential spread elements.
    ///
    /// Empty arrays get a fresh element type. Homogeneous arrays get
    /// `Array[T]`. Heterogeneous arrays (mixed types) become `Json`.
    /// Spreads contribute their element type to the overall array type.
    fn array(&mut self, elems: &[ArrayElem], span: Span) -> TyId {
        // Collect element types (for regular elements) and array element types (for spreads)
        let elem_tys: SmallVec<[TyId; 8]> = elems
            .iter()
            .map(|elem| match elem {
                ArrayElem::Elem(id) => self.expr(*id),
                ArrayElem::Spread(id) => {
                    let spread_ty = self.expr(*id);
                    // Check if spread is a numeric literal var; these cannot
                    // be arrays, so emit NotAnArray directly with Int (the
                    // default) to avoid confusing "Numeric constraint" errors.
                    let is_numeric_var = matches!(
                        self.ty_arena.get(spread_ty),
                        Ty::Var(v) if self.numeric_vars.contains(v)
                    );
                    if is_numeric_var {
                        self.error(TypeError::NotAnArray(TyArena::INT, span));
                        TyArena::ERROR
                    } else {
                        match self.ty_arena.get(spread_ty) {
                            Ty::Array(inner) => *inner,
                            Ty::Var(_) => {
                                // Create constraint: spread must be an array
                                let elem_ty = self.fresh();
                                let arr_ty = self.ty_arena.array(elem_ty);
                                self.unify(spread_ty, arr_ty, span);
                                elem_ty
                            }
                            Ty::Error => TyArena::ERROR,
                            _ => {
                                self.error(TypeError::NotAnArray(
                                    spread_ty, span,
                                ));
                                TyArena::ERROR
                            }
                        }
                    }
                }
            })
            .collect();

        if let Some((&first_ty, rest_tys)) = elem_tys.split_first() {
            // Check for errors
            if first_ty == TyArena::ERROR {
                TyArena::ERROR
            } else {
                // Check if array is heterogeneous.
                //
                // Type variables (from polymorphic numeric literals) have a
                // Numeric constraint, so they can only unify with Int/Word/Float.
                // We detect heterogeneity in two cases:
                // 1. Two concrete non-compatible types (e.g., String vs Bool)
                // 2. Type variables mixed with non-numeric concrete types
                //
                // In either case, the array becomes Json.
                let has_type_var = elem_tys
                    .iter()
                    .any(|&ty| matches!(self.ty_arena.get(ty), Ty::Var(_)));
                let concrete_tys: SmallVec<[TyId; 8]> = elem_tys
                    .iter()
                    .copied()
                    .filter(|&ty| {
                        !matches!(self.ty_arena.get(ty), Ty::Var(_) | Ty::Error)
                    })
                    .collect();

                // Case 1: incompatible concrete types
                let concrete_incompatible = concrete_tys
                    .windows(2)
                    .any(|pair| !self.types_compatible(pair[0], pair[1]));

                // Case 2: type var + non-numeric concrete type
                // (numeric literals can't unify with String, Bool, etc.)
                let var_with_non_numeric = has_type_var
                    && concrete_tys.iter().any(|&ty| {
                        !matches!(
                            self.ty_arena.get(ty),
                            Ty::Int | Ty::Word | Ty::Float
                        )
                    });

                let heterogeneous =
                    concrete_incompatible || var_with_non_numeric;

                if heterogeneous {
                    TyArena::JSON
                } else {
                    // Homogeneous: unify all elements
                    rest_tys.iter().for_each(|&ty| {
                        if ty != TyArena::ERROR {
                            self.unify(first_ty, ty, span);
                        }
                    });
                    self.ty_arena.array(first_ty)
                }
            }
        } else {
            let fresh = self.fresh();
            self.ty_arena.array(fresh)
        }
    }

    /// Infer type of a tuple literal.
    ///
    /// Infers each element independently; the tuple type contains all element
    /// types in order. Empty tuples are handled as `Unit` in `expr_inner`.
    fn tuple(&mut self, elems: &SmallVec<[ExprId; 4]>) -> TyId {
        let tys: SmallVec<[TyId; 4]> =
            elems.iter().map(|id| self.expr(*id)).collect();
        self.ty_arena.alloc(Ty::Tuple(tys))
    }

    /// Infer type of an object literal with potential spread entries.
    ///
    /// Spreads merge fields from the spread object; later fields override earlier.
    /// For struct preservation (Option 2): if we spread a struct and the result
    /// still has all required fields, we preserve the struct type.
    fn object(&mut self, entries: &[ObjectEntry], span: Span) -> TyId {
        // Track accumulated fields; later entries override earlier
        let mut acc: IndexMap<StringId, TyId> = IndexMap::new();
        // Track if we're spreading exactly one struct (for potential preservation)
        let mut spread_struct: Option<TypeId> = None;
        let mut has_error = false;

        entries.iter().for_each(|entry| match entry {
            ObjectEntry::Field(name, expr_id) => {
                let field_ty = self.expr(*expr_id);
                // Override or add field
                acc.insert(*name, field_ty);
            }
            ObjectEntry::Spread(expr_id) => {
                let spread_ty = self.expr(*expr_id);
                match self.ty_arena.get(spread_ty) {
                    Ty::Object(fields) => {
                        // Merge fields from spread object
                        let fields = fields.clone();
                        fields.iter().for_each(|(k, t)| {
                            acc.insert(*k, *t);
                        });
                    }
                    Ty::Named(ty_id, args) => {
                        let ty_id = *ty_id;
                        let args: SmallVec<[TyId; 4]> = args.clone();
                        if let Some((ps, fields)) = self.object_alias(ty_id) {
                            // Remember we spread this (for potential preservation)
                            if spread_struct.is_none() && acc.is_empty() {
                                spread_struct = Some(ty_id);
                            } else {
                                // Multiple spreads or fields before; no preservation
                                spread_struct = None;
                            }
                            // Merge fields (convert AstTypeExprId -> TyId)
                            let subst: IndexMap<_, _> = ps
                                .iter()
                                .zip(args.iter())
                                .map(|(p, &a)| (*p, a))
                                .collect();
                            fields.iter().for_each(|(name, ast_ty_id)| {
                                let field_ty = self
                                    .convert()
                                    .ast_type_to_ty(*ast_ty_id, &subst);
                                acc.insert(*name, field_ty);
                            });
                        } else {
                            self.error(TypeError::NotAnObjectSpread(
                                spread_ty, span,
                            ));
                            has_error = true;
                        }
                    }
                    Ty::Var(_) => {
                        // Create constraint: spread must be an object
                        let fresh_obj =
                            self.ty_arena.alloc(Ty::Object(IndexMap::new()));
                        self.unify(spread_ty, fresh_obj, span);
                        // Can't know fields statically; no struct preservation
                        spread_struct = None;
                    }
                    Ty::Error => has_error = true,
                    _ => {
                        self.error(TypeError::NotAnObjectSpread(
                            spread_ty, span,
                        ));
                        has_error = true;
                    }
                }
            }
        });

        if has_error {
            TyArena::ERROR
        } else if let Some(struct_id) = spread_struct {
            // Check if we can preserve the struct type (all required fields present)
            // Since spreading can only add fields, never remove them, the struct is valid
            // Extensible record semantics: struct + extra fields is still that struct
            self.ty_arena.named(struct_id, smallvec![])
        } else {
            self.ty_arena.alloc(Ty::Object(acc))
        }
    }

    /// Infer type of a map literal.
    ///
    /// Keys are unified to a common type; values are unified to a common type.
    /// Empty maps get fresh type variables for both.
    fn map_lit(
        &mut self,
        entries: &SmallVec<[(ExprId, ExprId); 8]>,
        span: Span,
    ) -> TyId {
        if let Some(((first_k, first_v), rest)) = entries.split_first() {
            let k_ty = self.expr(*first_k);
            let v_ty = self.expr(*first_v);
            rest.iter().for_each(|(k, v)| {
                let k = self.expr(*k);
                let v = self.expr(*v);
                self.unify(k_ty, k, span);
                self.unify(v_ty, v, span);
            });
            self.ty_arena.map_ty(k_ty, v_ty)
        } else {
            let k = self.fresh();
            let v = self.fresh();
            self.ty_arena.map_ty(k, v)
        }
    }

    /// Infer type of field access: `base.field`.
    ///
    /// Works for structural objects (`Ty::Object`) and named struct types
    /// (`Ty::Named` with `TypeDef::Struct`). For type variables, we cannot
    /// yet infer the field type without row polymorphism, so we create a
    /// structural object constraint.
    ///
    /// # AST Rewrite for Variant Access
    ///
    /// The parser produces `Field(Var("Type"), "Variant")` for `Type.Variant`
    /// since it cannot distinguish field access from variant access without
    /// type information. This method detects when `Type` resolves to a sum
    /// type with variant `Variant` and rewrites the AST accordingly:
    ///
    /// - **Zero-arity variants** (e.g., `Option.None`): Rewritten immediately
    ///   to `Variant("Option", "None", [])` since no call is needed.
    ///
    /// - **Non-zero-arity variants** (e.g., `Option.Some`): Returns a function
    ///   type `(T) -> Option[T]`. The actual rewrite to `Variant` happens in
    ///   `call_or_variant` when the constructor is invoked.
    ///
    /// This split handling is necessary because `Type.Variant` can appear in
    /// two contexts: as a value (`Option.None`) or as a function to be called
    /// (`Option.Some(x)`).
    fn field(
        &mut self,
        expr_id: ExprId,
        base_id: ExprId,
        field: &str,
        span: Span,
    ) -> TyId {
        // Check for variant access: `Type.Variant` where Type is in scope
        // via module-aware resolution.
        //
        // Extract type name first to avoid borrow issues.
        let ty_name_opt = self.ast.get_expr(base_id).and_then(|e| match e {
            Expr::Var(name) => Some(*name),
            _ => None,
        });

        // Try to resolve as a type with a variant (any arity)
        let field_id = self.env.intern(field);
        let variant_lookup = ty_name_opt.and_then(|ty_name_id| {
            self.convert()
                .resolve_type_name(&QualifiedName::local(ty_name_id))
                .and_then(|(type_id, qid)| {
                    self.registry
                        .lookup_variant(type_id, field_id)
                        .map(|v| (type_id, qid, v.arity))
                })
        });

        match variant_lookup {
            Some((type_id, resolved_id, 0)) => {
                // Zero-arity variant: rewrite AST to Variant expression
                self.ast.set_expr(
                    expr_id,
                    Expr::Variant(resolved_id, field_id, smallvec![]),
                );
                // Return the variant type
                self.variant_type_for_nullary(type_id)
            }
            Some((type_id, resolved_id, _arity)) => {
                // Non-zero-arity variant: return a function type for the
                // constructor. The AST will be rewritten to `Variant` by
                // `call` when this is invoked.
                self.variant_ctor_fn_type(type_id, resolved_id, field_id, span)
            }
            None => {
                // Regular field access
                let base_ty = self.expr(base_id);
                self.field_type(base_ty, field, span)
            }
        }
    }

    /// Infer type of optional field access: `base?.field`.
    ///
    /// Works on any type that has the field; always returns `Option[FieldType]`.
    /// - If base is `Option[T]`, unwraps and accesses field on `T`
    /// - If base is an object/struct with the field, accesses it directly
    /// - Either way, result is wrapped in `Option`
    fn optional_field(
        &mut self,
        base_id: ExprId,
        field: &str,
        span: Span,
    ) -> TyId {
        let base_ty = self.expr(base_id);

        match self.ty_arena.get(base_ty) {
            // `Option[T]`: unwrap, access field on `T`, rewrap
            Ty::Option(inner) => {
                let inner = *inner;
                let field_ty = self.optional_field_type(inner, field, span);
                self.ty_arena.option(field_ty)
            }

            // Object: field may or may not exist; missing -> `Unknown` (no error)
            Ty::Object(fields) => {
                let fields = fields.clone();
                let field_id = self.env.intern(field);
                let field_ty =
                    fields.get(&field_id).copied().unwrap_or(TyArena::UNKNOWN);
                self.ty_arena.option(field_ty)
            }

            // Named struct: use strict field lookup (structs have defined schema)
            Ty::Named(_, _) => {
                let field_ty = self.field_type(base_ty, field, span);
                self.ty_arena.option(field_ty)
            }

            // Type variable: create object constraint, wrap result in `Option`
            Ty::Var(_) => {
                let field_ty = self.field_type(base_ty, field, span);
                self.ty_arena.option(field_ty)
            }

            Ty::Error => TyArena::ERROR,

            _ => {
                self.error(TypeError::NotAnObject(base_ty, span));
                TyArena::ERROR
            }
        }
    }

    /// Infer type of tuple index: `tuple.0`, `tuple.1`, etc.
    fn tuple_index(&mut self, base_id: ExprId, idx: u32, span: Span) -> TyId {
        let base_ty = self.expr(base_id);

        match self.ty_arena.get(base_ty) {
            Ty::Tuple(elems) => {
                let elems = elems.clone();
                elems.get(idx as usize).copied().unwrap_or_else(|| {
                    self.error(TypeError::TupleIndexOutOfBounds {
                        idx,
                        len: elems.len(),
                        span,
                    });
                    TyArena::ERROR
                })
            }

            Ty::Var(_) => {
                // Cannot infer tuple structure from index access alone;
                // the constraint solver would need tuple row polymorphism.
                // For now, return fresh var and hope it unifies later.
                self.fresh()
            }

            Ty::Error => TyArena::ERROR,

            _ => {
                self.error(TypeError::NotATuple(base_ty, span));
                TyArena::ERROR
            }
        }
    }

    /// Infer type of index access: `base[idx]`.
    ///
    /// Works for `Array[T]` (index must be `Int`, returns `T`),
    /// `Map[K, V]` (index unifies with `K`, returns `Option[V]`),
    /// and `String` (index must be `Int`, returns `Char`).
    fn index(&mut self, base_id: ExprId, idx_id: ExprId, span: Span) -> TyId {
        let base_ty = self.expr(base_id);
        let idx_ty = self.expr(idx_id);

        match self.ty_arena.get(base_ty) {
            Ty::Array(elem) => {
                let elem = *elem;
                self.unify(idx_ty, TyArena::INT, span);
                elem
            }

            Ty::Map(key, val) => {
                let (key, val) = (*key, *val);
                self.unify(idx_ty, key, span);
                val
            }

            Ty::Var(v) => {
                let v = *v;
                // Base is type variable; generate Indexable constraint.
                // The index type is resolved via the associated type `Base.Index`.
                let elem = self.fresh();
                let expected_idx = self.ty_arena.alloc(Ty::AssocType(
                    v,
                    ClassId::INDEXABLE,
                    self.env.intern("Index"),
                ));
                self.unify(idx_ty, expected_idx, span);
                self.constrain(Constraint::Class {
                    ty: base_ty,
                    class: TypeClass::param(ClassId::INDEXABLE, elem),
                    span,
                });
                elem
            }

            Ty::Error => TyArena::ERROR,

            Ty::String => {
                // String indexing returns Char
                self.unify(idx_ty, TyArena::INT, span);
                TyArena::CHAR
            }

            Ty::Named(id, type_args) => {
                let (id, type_args) = (*id, type_args.clone());
                // Check for user-defined Indexable instance
                match self.check_instance_available(
                    ClassId::INDEXABLE,
                    id,
                    span,
                ) {
                    Some(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);

                        // Resolve index type from associated type
                        let inst_idx_ty = inst
                            .get_assoc_type(self.env.intern("Index"))
                            .map(|a| self.ty_arena.apply(a.ty, &param_subst))
                            .unwrap_or(TyArena::UNKNOWN);
                        self.unify(idx_ty, inst_idx_ty, span);

                        // Resolve element type from class args
                        inst.class_args
                            .first()
                            .map(|&t| self.ty_arena.apply(t, &param_subst))
                            .unwrap_or(TyArena::UNKNOWN)
                    }
                    None => {
                        // Only emit UnsatisfiedClass if instance truly doesn't exist
                        // (if it exists but isn't imported, error was already emitted)
                        if self
                            .instance_registry
                            .lookup(ClassId::INDEXABLE, id)
                            .is_none()
                        {
                            self.error(TypeError::UnsatisfiedClass(
                                TypeClass::param(
                                    ClassId::INDEXABLE,
                                    TyArena::ERROR,
                                ),
                                base_ty,
                                span,
                            ));
                        }
                        TyArena::ERROR
                    }
                }
            }

            _ => {
                self.error(TypeError::UnsatisfiedClass(
                    TypeClass::param(ClassId::INDEXABLE, TyArena::ERROR),
                    base_ty,
                    span,
                ));
                TyArena::ERROR
            }
        }
    }

    /// Infer type of optional index access: `base?[idx]`.
    ///
    /// Safe indexing that returns `Option[T]` instead of panicking:
    /// - `Array[T]?[Int]` returns `Option[T]`
    /// - `Map[K, V]?[K]` returns `Option[V]` (map lookup already returns Option)
    /// - `String?[Int]` returns `Option[Char]`
    fn optional_index(
        &mut self,
        base_id: ExprId,
        idx_id: ExprId,
        span: Span,
    ) -> TyId {
        let base_ty = self.expr(base_id);
        let idx_ty = self.expr(idx_id);

        match self.ty_arena.get(base_ty) {
            Ty::Array(elem) => {
                let elem = *elem;
                self.unify(idx_ty, TyArena::INT, span);
                self.ty_arena.option(elem)
            }

            Ty::Map(key, val) => {
                let (key, val) = (*key, *val);
                self.unify(idx_ty, key, span);
                // Map?[k] is the same as Map[k] since both return Option[V]
                self.ty_arena.option(val)
            }

            Ty::Var(v) => {
                let v = *v;
                // Generate Indexable constraint with elem wrapped in Option.
                // The index type is resolved via the associated type `Base.Index`.
                let inner = self.fresh();
                let expected_idx = self.ty_arena.alloc(Ty::AssocType(
                    v,
                    ClassId::INDEXABLE,
                    self.env.intern("Index"),
                ));
                self.unify(idx_ty, expected_idx, span);
                self.constrain(Constraint::Class {
                    ty: base_ty,
                    class: TypeClass::param(ClassId::INDEXABLE, inner),
                    span,
                });
                self.ty_arena.option(inner)
            }

            Ty::Error => TyArena::ERROR,

            Ty::String => {
                self.unify(idx_ty, TyArena::INT, span);
                self.ty_arena.option(TyArena::CHAR)
            }

            Ty::Named(id, type_args) => {
                let (id, type_args) = (*id, type_args.clone());
                // Check for user-defined Indexable instance
                match self.check_instance_available(
                    ClassId::INDEXABLE,
                    id,
                    span,
                ) {
                    Some(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);

                        // Resolve index type from associated type
                        let inst_idx_ty = inst
                            .get_assoc_type(self.env.intern("Index"))
                            .map(|a| self.ty_arena.apply(a.ty, &param_subst))
                            .unwrap_or(TyArena::UNKNOWN);
                        self.unify(idx_ty, inst_idx_ty, span);

                        // Resolve element type from class args, wrapped in Option
                        let elem = inst
                            .class_args
                            .first()
                            .map(|&t| self.ty_arena.apply(t, &param_subst))
                            .unwrap_or(TyArena::UNKNOWN);
                        self.ty_arena.option(elem)
                    }
                    None => {
                        // Only emit UnsatisfiedClass if instance truly doesn't exist
                        // (if it exists but isn't imported, error was already emitted)
                        if self
                            .instance_registry
                            .lookup(ClassId::INDEXABLE, id)
                            .is_none()
                        {
                            self.error(TypeError::UnsatisfiedClass(
                                TypeClass::param(
                                    ClassId::INDEXABLE,
                                    TyArena::ERROR,
                                ),
                                base_ty,
                                span,
                            ));
                        }
                        TyArena::ERROR
                    }
                }
            }

            _ => {
                self.error(TypeError::UnsatisfiedClass(
                    TypeClass::param(ClassId::INDEXABLE, TyArena::ERROR),
                    base_ty,
                    span,
                ));
                TyArena::ERROR
            }
        }
    }

    /// Infer type of JSON access operators.
    ///
    /// | Operator | Returns                           |
    /// |----------|-----------------------------------|
    /// | `.`      | `Json`                            |
    /// | `..`     | `Option[Scalar]` (union)          |
    /// | `->`     | `Json`                            |
    /// | `->>`    | `Option[Scalar]` (union)          |
    fn json_access(
        &mut self,
        base_id: ExprId,
        kind: &JsonAccessKind,
        key: &JsonAccessKey,
        span: Span,
    ) -> TyId {
        let base_ty = self.expr(base_id);

        // Infer the key expression type if dynamic
        if let JsonAccessKey::Expr(key_id) = key {
            let key_ty = self.expr(*key_id);
            // Dynamic key must be String
            self.unify(key_ty, TyArena::STRING, span);
        }

        // Base must be Json
        match self.ty_arena.get(base_ty) {
            Ty::Json | Ty::Var(_) => {
                if matches!(self.ty_arena.get(base_ty), Ty::Var(_)) {
                    self.unify(base_ty, TyArena::JSON, span);
                }
                match kind {
                    JsonAccessKind::Json => TyArena::JSON,
                    JsonAccessKind::Scalar => {
                        let scalar = self.ty_arena.scalar();
                        self.ty_arena.option(scalar)
                    }
                }
            }
            Ty::Error => TyArena::ERROR,
            _ => {
                self.error(TypeError::NotJson(base_ty, span));
                TyArena::ERROR
            }
        }
    }

    /// Infer types for function/closure parameters.
    ///
    /// For each parameter: uses annotation if present, otherwise fresh type var.
    pub(super) fn param_tys(
        &mut self,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
    ) -> Vec<TyId> {
        let subst = self.cur_subst();
        self.param_tys_with_subst(params, &subst)
    }

    /// Infer types for function/closure parameters with type param substitution.
    ///
    /// For generic functions, the `subst` map provides fresh type variables for
    /// explicit type parameters (e.g., `T` in `fn foo[T](x: T)`).
    pub(super) fn param_tys_with_subst(
        &mut self,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        subst: &IndexMap<StringId, TyId>,
    ) -> Vec<TyId> {
        params
            .iter()
            .map(|(_, ann)| match ann {
                Some(id) => self.convert().ast_type_to_ty(*id, subst),
                None => self.fresh(),
            })
            .collect()
    }

    /// Bind parameters in the current scope with their inferred types.
    pub(super) fn bind_params(
        &mut self,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        tys: &[TyId],
    ) {
        params.iter().zip(tys.iter()).for_each(|((name, _), &ty)| {
            self.env.bind(*name, Scheme::mono(ty));
        });
    }

    /// Infer type of a closure expression.
    ///
    /// For each parameter: uses annotation if present, otherwise fresh type var.
    /// Binds parameters in a new scope, infers body, then pops scope.
    /// If return annotation present, unifies body type with it.
    ///
    /// For generic closures (`[T](x: T) -> T => x`), type parameters are bound
    /// as fresh type variables before inferring parameter/return types.
    /// Default qualified `let` generalization captures the resulting function
    /// type and reachable constraints when the closure is bound.
    fn closure(
        &mut self,
        _expr_id: ExprId,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) -> TyId {
        // Two-pass approach: first create all type variables, then emit
        // constraints (needed for Iterable[T] where T references another param)
        // Keep track of name -> TyVar for scheme building
        let name_to_tv: HashMap<StringId, TyVar> = type_params
            .iter()
            .map(|tp| (tp.name, self.fresh_var()))
            .collect();

        let type_param_subst: IndexMap<_, _> = type_params
            .iter()
            .map(|tp| {
                let tv = name_to_tv[&tp.name];
                (tp.name, self.ty_arena.alloc(Ty::Var(tv)))
            })
            .collect();

        // Emit constraints for each user-specified bound
        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[&tp.name];
            let ty = self.ty_arena.alloc(Ty::Var(tv));
            self.record_let_tv_name(tv, tp.name);

            tp.constraints.iter().for_each(|c| {
                let class =
                    self.convert().ast_class_to_ty_class(c, &type_param_subst);
                self.record_let_tv_constraint(tv, class.clone());

                // Emit constraint for body inference
                self.constrain(Constraint::Class {
                    ty,
                    class: class.clone(),
                    span,
                });

                // Emit transitive superclass constraints (skip if the
                // class is not HKT; only HKT classes have superclasses)
                self.env
                    .class_registry()
                    .transitive_supers(class.tag())
                    .into_iter()
                    .for_each(|sup| {
                        if let Some(sc) = class.with_tag(sup) {
                            self.record_let_tv_constraint(tv, sc.clone());
                            self.constrain(Constraint::Class {
                                ty,
                                class: sc,
                                span,
                            });
                        }
                    });
            });
        });

        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        self.env.push_scope();
        self.bind_params(params, &param_tys);
        self.ty_substs.push(type_param_subst.clone());

        // Register type param vars as polymorphic parameters (cannot be refined)
        name_to_tv.values().for_each(|&tv| {
            self.poly_param_vars.insert(tv);
        });

        let body_ty = self.expr(body);
        self.ty_substs.pop();
        self.env.pop_scope();

        // If return annotation present, unify body with it
        let ret_ty = match ret {
            Some(ret_id) => {
                let expected =
                    self.convert().ast_type_to_ty(*ret_id, &type_param_subst);
                let ret_span = self.ast.type_expr_span(*ret_id).unwrap_or(span);
                self.unify(body_ty, expected, ret_span);
                expected
            }
            None => body_ty,
        };

        let param_sv: SmallVec<[TyId; 4]> = param_tys.into_iter().collect();
        let fn_ty = self.ty_arena.func(param_sv, ret_ty);
        self.interp.function_types.insert(body, fn_ty);

        fn_ty
    }

    /// Infer type of a function call, detecting variant constructor calls.
    ///
    /// # AST Rewrite for Variant Constructors
    ///
    /// The parser produces `Call(Field(Var("Type"), "Variant"), args)` for
    /// `Type.Variant(args)` since it cannot distinguish a method/function
    /// call from a variant constructor without type information.
    ///
    /// This method detects when the callee is `Field(Var(type_name), variant)`
    /// and `type_name` resolves to a sum type with that variant. When found,
    /// it rewrites the entire `Call` expression to `Variant(qualified_type,
    /// variant, args)` and delegates to the `variant` method.
    ///
    /// For example, `Circle.Circle(3.14)` is parsed as:
    /// ```text
    /// Call(Field(Var("Circle"), "Circle"), [3.14])
    /// ```
    /// and rewritten to:
    /// ```text
    /// Variant("ModulePath.Circle", "Circle", [3.14])
    /// ```
    ///
    /// Note: Zero-arity variants are handled by `field` directly since they
    /// do not appear in call position.
    fn call_or_variant(
        &mut self,
        expr_id: ExprId,
        callee_id: ExprId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        // Check if callee is `Field(Var(ty_name), var_name)`
        let callee_expr = self.ast.get_expr(callee_id).cloned();
        let variant_info = callee_expr.and_then(|e| match e {
            Expr::Field(base_id, field) => {
                self.ast.get_expr(base_id).and_then(|base| match base {
                    Expr::Var(ty_name) => Some((*ty_name, field)),
                    _ => None,
                })
            }
            _ => None,
        });

        // Try to resolve as variant constructor
        let resolved = variant_info.and_then(|(ty_name_id, var_name_id)| {
            self.convert()
                .resolve_type_name(&QualifiedName::local(ty_name_id))
                .and_then(|(type_id, qid)| {
                    self.registry
                        .lookup_variant(type_id, var_name_id)
                        .filter(|v| v.arity > 0)
                        .map(|_| (qid, var_name_id))
                })
        });

        match resolved {
            Some((qid, var_name_id)) => {
                // Rewrite AST to Variant expression
                self.ast.set_expr(
                    expr_id,
                    Expr::Variant(qid.clone(), var_name_id, args.clone()),
                );
                // Delegate to variant method
                self.variant(expr_id, qid, var_name_id, args, span)
            }
            None => self.call(expr_id, callee_id, args, span),
        }
    }

    /// Infer type of a function call expression.
    ///
    /// Infers callee and argument types, then adds a `Callable` constraint.
    /// Returns a fresh type variable that will be unified with the return type.
    fn call(
        &mut self,
        call_id: ExprId,
        callee_id: ExprId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        let callee_ty = self.expr(callee_id);
        let arg_tys: SmallVec<[TyId; 4]> =
            args.iter().map(|id| self.expr(*id)).collect();
        let call_arg_tys = arg_tys.clone();

        let ret = self.fresh();
        self.constrain(Constraint::Callable {
            callee: callee_ty,
            args: arg_tys,
            ret,
            span,
        });
        self.record_convert_ref_newtype_edge(
            call_id,
            callee_id,
            &call_arg_tys,
            span,
        );
        self.record_method_ref_call(
            call_id,
            callee_id,
            callee_ty,
            &call_arg_tys,
            ret,
            span,
        );
        ret
    }

    fn record_method_ref_call(
        &mut self,
        call_id: ExprId,
        callee_id: ExprId,
        callee_ty: TyId,
        args: &SmallVec<[TyId; 4]>,
        ret: TyId,
        span: Span,
    ) {
        let origin = self.method_ref_origin_for_callee(callee_id);
        if let Some(origin) = origin {
            self.record_method_ref_origin_call(
                call_id, origin, callee_ty, args, ret, span,
            );
        }
    }

    fn method_ref_origin_for_callee(
        &self,
        callee_id: ExprId,
    ) -> Option<MethodRefOrigin> {
        self.ast.get_expr(callee_id).and_then(|expr| match expr {
            Expr::Var(name) => self.env.lookup_method_ref_origin(*name),
            Expr::Path(segments) => {
                segments.split_last().and_then(|(&member, mod_segments)| {
                    let mod_qn = QualifiedName::new(mod_segments.to_vec());
                    self.env
                        .lookup_user_module_member(&mod_qn, member)
                        .and_then(|member| member.method_origin)
                })
            }
            Expr::Field(base, member) => {
                self.ast.get_expr(*base).and_then(|e| match e {
                    Expr::Var(module) => {
                        let mod_qn = QualifiedName::local(*module);
                        self.env
                            .lookup_user_module_member(&mod_qn, *member)
                            .and_then(|member| member.method_origin)
                    }
                    _ => None,
                })
            }
            Expr::Call(callee, args) => self
                .method_ref_origin_for_callee(*callee)
                .and_then(|origin| {
                    let applied = origin.applied + args.len();
                    self.expr_method_ref_arity(origin)
                        .filter(|&arity| applied < arity)
                        .map(|_| MethodRefOrigin { applied, ..origin })
                }),
            _ => None,
        })
    }

    fn expr_method_ref_arity(&self, origin: MethodRefOrigin) -> Option<usize> {
        self.env
            .class_def(origin.class)
            .method(origin.method, Span::default())
            .ok()
            .and_then(|spec| spec.scheme().arity(&self.ty_arena))
    }

    fn record_method_ref_origin_call(
        &mut self,
        call_id: ExprId,
        origin: MethodRefOrigin,
        callee_ty: TyId,
        args: &SmallVec<[TyId; 4]>,
        ret: TyId,
        span: Span,
    ) {
        match self
            .env
            .class_def(origin.class)
            .method(origin.method, span)
            .cloned()
        {
            Ok(spec) => {
                let (fn_ty, constraints) =
                    spec.scheme().instantiate(&mut self.uf, &mut self.ty_arena);
                let (params, sig_ret) = match self.ty_arena.get(fn_ty) {
                    Ty::Fn(params, ret) => (params.clone(), *ret),
                    _ => {
                        self.error(TypeError::Custom {
                            msg: "class method scheme is not a function type"
                                .into(),
                            span,
                        });
                        (smallvec![], TyArena::ERROR)
                    }
                };
                let applied = origin.applied.min(params.len());
                let callee_sig = if applied == 0 {
                    fn_ty
                } else {
                    let remaining: SmallVec<[TyId; 4]> =
                        params.iter().skip(applied).copied().collect();
                    self.ty_arena.func(remaining, sig_ret)
                };
                self.unify(callee_sig, callee_ty, span);
                params.iter().skip(applied).zip(args.iter()).for_each(
                    |(&param, &arg)| {
                        self.unify(arg, param, span);
                    },
                );
                let used = applied + args.len();
                if used < params.len() {
                    let remaining: SmallVec<[TyId; 4]> =
                        params.iter().skip(used).copied().collect();
                    let partial = self.ty_arena.func(remaining, sig_ret);
                    self.unify(partial, ret, span);
                } else {
                    self.unify(sig_ret, ret, span);
                }
                let is_full = used >= params.len();
                match &spec {
                    MethodSpec::Standard(_) => {}
                    MethodSpec::Tracked { track, .. } => match track {
                        TrackKind::Default
                        | TrackKind::Zero
                        | TrackKind::One => {
                            self.track_concrete_return(call_id, sig_ret);
                        }
                        TrackKind::Convert if is_full => {
                            self.interp.set_expr_ty(call_id, sig_ret);
                            args.first()
                                .copied()
                                .filter(|_| origin.applied == 0)
                                .into_iter()
                                .for_each(|from| {
                                    self.newtype_edge_checks.push((
                                        call_id,
                                        from,
                                        sig_ret,
                                        span,
                                        self.current_module.clone(),
                                    ));
                                });
                        }
                        TrackKind::Convert => {}
                        TrackKind::ConvertResultInner if is_full => {
                            let inner = match self.ty_arena.get(sig_ret) {
                                Ty::Result(ok, _) => *ok,
                                _ => {
                                    self.error(TypeError::Custom {
                                        msg:
                                        "ConvertResultInner expects Result type"
                                            .into(),
                                        span,
                                    });
                                    TyArena::ERROR
                                }
                            };
                            self.interp.set_expr_ty(call_id, inner);
                            args.first()
                                .copied()
                                .filter(|_| origin.applied == 0)
                                .into_iter()
                                .for_each(|from| {
                                    self.newtype_edge_checks.push((
                                        call_id,
                                        from,
                                        inner,
                                        span,
                                        self.current_module.clone(),
                                    ));
                                });
                        }
                        TrackKind::ConvertResultInner => {}
                    },
                }
                self.record_method_ref_dispatch(
                    call_id,
                    origin,
                    &constraints,
                    &params,
                    args,
                    span,
                );
            }
            Err(e) => self.error(e),
        }
    }

    fn record_method_ref_dispatch(
        &mut self,
        call_id: ExprId,
        origin: MethodRefOrigin,
        constraints: &[(TyId, TypeClass<TyId>)],
        params: &SmallVec<[TyId; 4]>,
        args: &SmallVec<[TyId; 4]>,
        span: Span,
    ) {
        if origin.class.idx() >= ClassId::BUILTIN_COUNT {
            let lookup_ty = if self.method_ref_is_hkt_user(origin.class) {
                self.method_ref_hkt_current_arg(
                    origin.class,
                    constraints,
                    params,
                    origin.applied,
                    args,
                )
                .or_else(|| {
                    self.method_ref_hkt_container(origin.class, constraints)
                })
            } else {
                args.first().copied()
            };
            if let Some(ty) = lookup_ty {
                let type_id = self.nominal_type_id(ty);
                match type_id {
                    Some(tid)
                        if self
                            .check_instance_available(origin.class, tid, span)
                            .is_some() =>
                    {
                        self.set_instance_call(call_id, tid);
                    }
                    _ => {
                        self.deferred_inst_calls.push((
                            call_id,
                            ty,
                            origin.class,
                        ));
                    }
                }
            };

            if self.method_ref_is_param_user(origin.class) {
                args.first()
                    .copied()
                    .filter(|_| origin.applied == 0)
                    .zip(origin.class_arg.or_else(|| {
                        self.method_ref_param_class_arg(
                            origin.class,
                            constraints,
                        )
                    }))
                    .into_iter()
                    .for_each(|(recv, class_arg)| {
                        self.deferred_param_calls.push((
                            call_id,
                            origin.class,
                            origin.method,
                            recv,
                            class_arg,
                        ));
                    });
            }

            if self.method_ref_is_hkt_user(origin.class) {
                lookup_ty.into_iter().for_each(|ty| {
                    self.deferred_hkt_user_calls.push((
                        call_id,
                        origin.class,
                        origin.method,
                        ty,
                    ));
                });
            }
        }
    }

    fn method_ref_hkt_current_arg(
        &self,
        class: ClassId,
        constraints: &[(TyId, TypeClass<TyId>)],
        params: &SmallVec<[TyId; 4]>,
        applied: usize,
        args: &SmallVec<[TyId; 4]>,
    ) -> Option<TyId> {
        self.method_ref_hkt_recv_param(class, constraints, params)
            .and_then(|idx| idx.checked_sub(applied))
            .and_then(|idx| args.get(idx).copied())
    }

    fn method_ref_hkt_recv_param(
        &self,
        class: ClassId,
        constraints: &[(TyId, TypeClass<TyId>)],
        params: &SmallVec<[TyId; 4]>,
    ) -> Option<usize> {
        let var = constraints.iter().find_map(|(ty, c)| match c {
            TypeClass::Hkt { id, .. } if *id == class => {
                match self.ty_arena.get(*ty) {
                    Ty::Var(v) => Some(*v),
                    _ => None,
                }
            }
            _ => None,
        })?;
        params
            .iter()
            .position(|&ty| self.method_ref_ty_has_var(ty, var))
    }

    fn method_ref_ty_has_var(&self, ty: TyId, var: TyVar) -> bool {
        match self.ty_arena.get(ty) {
            Ty::Var(v) => *v == var,
            Ty::Array(t) | Ty::Option(t) => self.method_ref_ty_has_var(*t, var),
            Ty::Result(ok, err) | Ty::Map(ok, err) => {
                self.method_ref_ty_has_var(*ok, var)
                    || self.method_ref_ty_has_var(*err, var)
            }
            Ty::Tuple(ts) | Ty::Union(_, ts) | Ty::Named(_, ts) => {
                ts.iter().any(|&ty| self.method_ref_ty_has_var(ty, var))
            }
            Ty::Fn(params, ret) => {
                params.iter().any(|&ty| self.method_ref_ty_has_var(ty, var))
                    || self.method_ref_ty_has_var(*ret, var)
            }
            Ty::Object(fields) => fields
                .values()
                .any(|&ty| self.method_ref_ty_has_var(ty, var)),
            Ty::Apply(v, args) => {
                *v == var
                    || args
                        .iter()
                        .any(|&ty| self.method_ref_ty_has_var(ty, var))
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

    fn method_ref_hkt_container(
        &self,
        class: ClassId,
        constraints: &[(TyId, TypeClass<TyId>)],
    ) -> Option<TyId> {
        if matches!(
            self.env.class_registry().shape(class),
            ClassShape::Hkt { .. }
        ) {
            constraints.iter().find_map(|(ty, c)| match c {
                TypeClass::Hkt { id, .. } if *id == class => Some(*ty),
                _ => None,
            })
        } else {
            None
        }
    }

    fn method_ref_param_class_arg(
        &self,
        class: ClassId,
        constraints: &[(TyId, TypeClass<TyId>)],
    ) -> Option<TyId> {
        if matches!(
            self.env.class_registry().shape(class),
            ClassShape::Concrete { params } if params > 0
        ) {
            constraints.iter().find_map(|(_, c)| match c {
                TypeClass::Concrete { id, params } if *id == class => {
                    params.first().copied()
                }
                _ => None,
            })
        } else {
            None
        }
    }

    fn method_ref_is_param_user(&self, class: ClassId) -> bool {
        matches!(
            self.env.class_registry().shape(class),
            ClassShape::Concrete { params } if params > 0
        ) && class.idx() >= ClassId::BUILTIN_COUNT
    }

    fn method_ref_is_hkt_user(&self, class: ClassId) -> bool {
        matches!(
            self.env.class_registry().shape(class),
            ClassShape::Hkt { .. }
        ) && class.idx() >= ClassId::BUILTIN_COUNT
    }

    fn record_convert_ref_newtype_edge(
        &mut self,
        call_id: ExprId,
        callee_id: ExprId,
        args: &SmallVec<[TyId; 4]>,
        span: Span,
    ) {
        let callee = self.ast.get_expr(callee_id).cloned();
        let target = self
            .interp
            .expr_metadata
            .get(&callee_id)
            .and_then(|info| info.ty);
        if let (
            Some(Expr::ClassMethodRef(class, _, method)),
            Some(to),
            Some(from),
        ) = (callee, target, args.first().copied())
        {
            let class = self.env.class_registry().lookup_by_name(class);
            let method = self.env.resolve_str(method);
            if matches!((class, method), (Some(ClassId::INTO), "into")) {
                self.newtype_edge_checks.push((
                    call_id,
                    from,
                    to,
                    span,
                    self.current_module.clone(),
                ));
            }
        }
    }

    /// Infer type of an IF expression.
    ///
    /// # Type Checking Rules
    ///
    /// - Condition must be `Bool`
    /// - IF/ELSE: both branches must have the same type
    /// - Single-arm IF (no ELSE): body must be `Unit`, whole expression is `Unit`
    ///
    /// # `is` with Bindings
    ///
    /// If the condition is `expr is Pattern(bindings)`, the bindings are only
    /// visible in the then branch, not the else branch. The type checker
    /// extracts these bindings and adds them to the then-branch scope.
    fn r#if(
        &mut self,
        cond_id: ExprId,
        then_id: ExprId,
        else_id: Option<ExprId>,
        span: Span,
    ) -> TyId {
        // Check if condition is an `is` expression with variant bindings
        let cond_expr = self.normalize_is_expr(cond_id, span);

        let then_ty = match cond_expr {
            Some(Expr::Is(
                scrutinee_id,
                TypePattern::VariantBind(ref ty_name, var_name, names),
            )) => {
                // `is` with variant bindings: bindings only visible in then branch
                let scrutinee_ty = self.expr(scrutinee_id);

                // Check scrutinee is compatible with variant pattern
                if !self
                    .scrutinee_compatible_with_variant(scrutinee_ty, ty_name)
                {
                    let pat_ty = self.env.resolve_string(ty_name.local_name());
                    self.error(TypeError::IncompatibleVariantPattern {
                        pattern_ty: pat_ty,
                        scrutinee_ty,
                        span,
                    });
                }

                let payload_tys = self.variant_payload_types(
                    ty_name,
                    var_name,
                    scrutinee_ty,
                    span,
                );

                if payload_tys.len() != names.len() {
                    self.error(TypeError::ArityMismatch {
                        expected: payload_tys.len(),
                        got: names.len(),
                        span,
                    });
                }

                self.env.push_scope();
                names
                    .iter()
                    .zip(payload_tys.iter())
                    .for_each(|(name, &ty)| {
                        self.env.bind(*name, Scheme::mono(ty));
                    });
                let ty = self.expr(then_id);
                self.env.pop_scope();
                ty
            }
            _ => {
                // Regular condition: infer and unify with Bool
                let cond_ty = self.expr(cond_id);
                self.unify(cond_ty, TyArena::BOOL, span);
                self.expr(then_id)
            }
        };

        // Unify branches (or find common union type)
        if let Some(else_id) = else_id {
            let else_ty = self.expr(else_id);
            self.join_types(&[then_ty, else_ty], span)
        } else {
            self.unify(then_ty, TyArena::UNIT, span);
            TyArena::UNIT
        }
    }

    /// Infer type of a block expression.
    ///
    /// Executes statements for side effects, then evaluates to the trailing
    /// expression. Returns `Unit` if no trailing expression.
    fn block(&mut self, stmts: &[StmtId], tail: Option<ExprId>) -> TyId {
        self.env.push_scope();
        self.hoist_declarations(stmts);
        stmts.iter().for_each(|id| self.stmt(*id));
        let result_ty = tail.map_or(TyArena::UNIT, |id| self.expr(id));
        self.env.pop_scope();
        result_ty
    }

    /// Infer type of a match expression.
    ///
    /// Evaluates the scrutinee once, then checks each arm. All arm bodies must
    /// have the same type (or be members of a common union). Also performs
    /// exhaustiveness checking.
    fn r#match(
        &mut self,
        scrutinee_id: ExprId,
        arms: &[MatchArm],
        span: Span,
    ) -> TyId {
        let scrutinee_ty = self.expr(scrutinee_id);

        if arms.is_empty() {
            self.error(TypeError::NonExhaustiveMatch(span));
            TyArena::ERROR
        } else {
            // Infer all arm body types
            let arm_tys: Vec<TyId> = arms
                .iter()
                .map(|arm| self.match_arm(arm, scrutinee_ty, span))
                .collect();

            // Try to find a common type for all arms
            let result_ty = self.join_types(&arm_tys, span);

            // Exhaustiveness check
            self.check_exhaustiveness(arms, scrutinee_ty, span);

            result_ty
        }
    }

    /// Find a common type for a list of types.
    ///
    /// If all types are the same, returns that type. If they differ and contain
    /// type variables, unifies them (standard HM behavior). If all are primitive
    /// storable types, creates an anonymous union. Otherwise, unifies normally.
    ///
    /// Special case: type variables from integer literals (`numeric_vars`) are
    /// treated as storable for union creation. This allows patterns like
    /// `if cond { 42 } ELSE { "string" }` to produce `Int | String` instead
    /// of incorrectly unifying the literal's var with `String`.
    fn join_types(&mut self, tys: &[TyId], span: Span) -> TyId {
        let first = tys.first().copied().unwrap_or(TyArena::ERROR);
        let all_same = tys.iter().skip(1).all(|&t| t == first);

        // Check if a type is a storable primitive or a numeric literal type var
        let is_storable_or_numeric_var = |&t: &TyId| match self.ty_arena.get(t)
        {
            Ty::Bool
            | Ty::Int
            | Ty::Word
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Json => true,
            Ty::Var(v) => self.numeric_vars.contains(v),
            _ => false,
        };

        // Only create anonymous unions for primitive storable types
        // (Bool, Int, Float, Char, String, Json) and numeric literal vars.
        // This supports patterns like `if cond { 42 } ELSE { "string" }`.
        // For other types (Option, Result, user structs), unify normally.
        let all_storable_or_numeric_var =
            || tys.iter().all(is_storable_or_numeric_var);

        if all_same {
            first
        } else if all_storable_or_numeric_var() {
            // Deduplicate members; resolve numeric vars to Int (default)
            let members: SmallVec<[TyId; 4]> =
                tys.iter().fold(SmallVec::new(), |mut acc, &t| {
                    let resolved = match self.ty_arena.get(t) {
                        Ty::Var(v) if self.numeric_vars.contains(v) => {
                            TyArena::INT
                        }
                        _ => t,
                    };
                    if !acc.contains(&resolved) {
                        acc.push(resolved);
                    }
                    acc
                });
            self.ty_arena.alloc(Ty::Union(None, members))
        } else {
            // Unify normally; mismatches will error
            tys.iter().skip(1).for_each(|&ty| {
                self.unify(first, ty, span);
            });
            first
        }
    }

    /// Infer type of a single match arm.
    ///
    /// Checks the pattern, binds variables, evaluates guard (if any),
    /// and infers the body type.
    fn match_arm(
        &mut self,
        arm: &MatchArm,
        scrutinee_ty: TyId,
        span: Span,
    ) -> TyId {
        self.env.push_scope();

        // Check pattern and collect bindings
        self.pattern_bindings(arm.pattern, scrutinee_ty, span);

        // Check guard if present
        if let Some(guard_id) = arm.guard {
            let guard_ty = self.expr(guard_id);
            self.unify(guard_ty, TyArena::BOOL, span);
        }

        // Infer body
        let body_ty = self.expr(arm.body);
        self.env.pop_scope();
        body_ty
    }

    /// Infer type of a variant constructor: `Type.Variant(args)`.
    fn variant(
        &mut self,
        expr_id: ExprId,
        ty_name: QualifiedName,
        var_name: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        let arg_tys: Vec<TyId> = args.iter().map(|id| self.expr(*id)).collect();

        // Resolve type name using module-aware lookup
        let resolved = self.convert().resolve_type_name(&ty_name);

        match resolved {
            None => {
                let tn = ty_name.display(&self.env.strings);
                let vn = self.env.resolve_string(var_name);
                self.error(TypeError::UnknownType(format!("{tn}.{vn}"), span));
                TyArena::ERROR
            }
            Some((type_id, qid)) => {
                // Rewrite AST if name was resolved differently
                if qid != ty_name {
                    self.ast.set_expr(
                        expr_id,
                        Expr::Variant(qid.clone(), var_name, args.clone()),
                    );
                }

                // Look up variant in resolved type
                let lookup = self
                    .registry
                    .lookup_variant(type_id, var_name)
                    .map(|var_def| (type_id, var_def));

                match lookup {
                    None => {
                        let qn = qid.display(&self.env.strings);
                        let vn = self.env.resolve_string(var_name);
                        self.error(TypeError::UnknownType(
                            format!("{qn}.{vn}"),
                            span,
                        ));
                        TyArena::ERROR
                    }
                    Some((type_id, var_def)) => {
                        if var_def.arity as usize != arg_tys.len() {
                            self.error(TypeError::ArityMismatch {
                                expected: var_def.arity as usize,
                                got: arg_tys.len(),
                                span,
                            });
                        }

                        if type_id == TypeId::OPTION {
                            let inner = arg_tys
                                .first()
                                .copied()
                                .unwrap_or_else(|| self.fresh());
                            self.ty_arena.option(inner)
                        } else if type_id == TypeId::RESULT {
                            match var_def.idx {
                                0 => {
                                    let ok = arg_tys
                                        .first()
                                        .copied()
                                        .unwrap_or_else(|| self.fresh());
                                    let err = self.fresh();
                                    self.ty_arena.result(ok, err)
                                }
                                1 => {
                                    let err = arg_tys
                                        .first()
                                        .copied()
                                        .unwrap_or_else(|| self.fresh());
                                    let ok = self.fresh();
                                    self.ty_arena.result(ok, err)
                                }
                                _ => TyArena::ERROR,
                            }
                        } else if type_id == TypeId::ORDERING {
                            // Ordering has no type parameters; all variants
                            // are nullary
                            TyArena::ORDERING
                        } else {
                            match self.registry.get_def(type_id) {
                                Some(TypeDef::Sum { type_params, .. }) => {
                                    let type_args: SmallVec<[TyId; 4]> =
                                        type_params
                                            .iter()
                                            .map(|_| self.fresh())
                                            .collect();
                                    let subst: IndexMap<StringId, TyId> =
                                        type_params
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(p, &a)| (*p, a))
                                            .collect();

                                    let payloads = self
                                        .decls
                                        .variant_payloads(type_id, var_def.name)
                                        .cloned()
                                        .unwrap_or_default();

                                    payloads
                                        .iter()
                                        .zip(arg_tys.iter())
                                        .for_each(|(expected_id, &got)| {
                                            let expected =
                                                self.convert().ast_type_to_ty(
                                                    *expected_id,
                                                    &subst,
                                                );
                                            self.unify(expected, got, span);
                                        });

                                    self.ty_arena.named(type_id, type_args)
                                }
                                _ => {
                                    let tn = ty_name.display(&self.env.strings);
                                    let vn = self.env.resolve_string(var_name);
                                    self.error(TypeError::UnknownType(
                                        format!("{tn}.{vn}"),
                                        span,
                                    ));
                                    TyArena::ERROR
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// Infer type of a naked variant constructor: `.Variant(args)`.
    fn naked_variant(
        &mut self,
        expr_id: ExprId,
        var_name: StringId,
        args: &SmallVec<[ExprId; 4]>,
        span: Span,
    ) -> TyId {
        match self.resolve_naked_variant(var_name, span) {
            Some((type_id, qn)) => {
                let arity = self
                    .registry
                    .lookup_variant(type_id, var_name)
                    .map(|v| v.arity as usize)
                    .unwrap_or(0);
                if args.is_empty() && arity > 0 {
                    self.ast.set_expr(
                        expr_id,
                        Expr::Variant(qn.clone(), var_name, smallvec![]),
                    );
                    self.variant_ctor_fn_type(type_id, qn, var_name, span)
                } else {
                    self.ast.set_expr(
                        expr_id,
                        Expr::Variant(qn.clone(), var_name, args.clone()),
                    );
                    self.variant(expr_id, qn, var_name, args, span)
                }
            }
            None => TyArena::ERROR,
        }
    }

    /// Search visible sum types for a variant name.
    ///
    /// Returns `Some((TypeId, QualifiedName))` if exactly one visible type
    /// defines it, or `None` after emitting an error.
    pub(super) fn resolve_naked_variant(
        &mut self,
        var_name: StringId,
        span: Span,
    ) -> Option<(TypeId, QualifiedName)> {
        self.resolve_naked_variant_with(var_name, span, true)
    }

    fn resolve_naked_variant_silent(
        &mut self,
        var_name: StringId,
        span: Span,
    ) -> Option<(TypeId, QualifiedName)> {
        self.resolve_naked_variant_with(var_name, span, false)
    }

    fn resolve_naked_variant_with(
        &mut self,
        var_name: StringId,
        span: Span,
        emit: bool,
    ) -> Option<(TypeId, QualifiedName)> {
        let mut matches: Vec<_> = self
            .registry
            .lookup_variant_types(var_name)
            .into_iter()
            .filter(|(id, qn)| self.type_is_in_scope(*id, qn))
            .collect();
        matches.sort_by_key(|(_, qn)| qn.display(&self.env.strings));

        match matches.as_slice() {
            [] => {
                if emit {
                    let vn = self.env.resolve_str(var_name).to_owned();
                    self.error(TypeError::UnknownNakedVariant(vn, span));
                }
                None
            }
            [(id, qn)] => Some((*id, qn.clone())),
            _ => {
                if emit {
                    let vn = self.env.resolve_str(var_name).to_owned();
                    let types = matches
                        .iter()
                        .map(|(_, qn)| qn.display(&self.env.strings))
                        .collect();
                    self.error(TypeError::AmbiguousNakedVariant {
                        variant: vn,
                        types,
                        span,
                    });
                }
                None
            }
        }
    }

    fn type_is_in_scope(&mut self, id: TypeId, qn: &QualifiedName) -> bool {
        if !qn.is_qualified() || self.env.imports_type(qn) {
            true
        } else {
            self.convert()
                .resolve_type_name(&QualifiedName::local(qn.local_name()))
                .is_some_and(|(tid, eff)| tid == id && eff == *qn)
        }
    }

    fn resolve_type_pattern(
        &mut self,
        pat: &TypePattern,
        span: Span,
    ) -> TypePattern {
        self.resolve_type_pattern_with(pat, span, true)
    }

    fn resolve_type_pattern_silent(
        &mut self,
        pat: &TypePattern,
        span: Span,
    ) -> TypePattern {
        self.resolve_type_pattern_with(pat, span, false)
    }

    fn resolve_type_pattern_with(
        &mut self,
        pat: &TypePattern,
        span: Span,
        emit: bool,
    ) -> TypePattern {
        match pat {
            TypePattern::NakedVariant(var) => self
                .resolve_naked_variant_for_pattern(*var, span, emit)
                .map_or_else(
                    || pat.clone(),
                    |(_, qn)| TypePattern::Variant(qn, *var),
                ),
            TypePattern::NakedVariantWildcard(var) => self
                .resolve_naked_variant_for_pattern(*var, span, emit)
                .map_or_else(
                    || pat.clone(),
                    |(_, qn)| TypePattern::VariantWildcard(qn, *var),
                ),
            TypePattern::NakedVariantBind(var, names) => self
                .resolve_naked_variant_for_pattern(*var, span, emit)
                .map_or_else(
                    || pat.clone(),
                    |(_, qn)| TypePattern::VariantBind(qn, *var, names.clone()),
                ),
            _ => pat.clone(),
        }
    }

    fn resolve_naked_variant_for_pattern(
        &mut self,
        var_name: StringId,
        span: Span,
        emit: bool,
    ) -> Option<(TypeId, QualifiedName)> {
        if emit {
            self.resolve_naked_variant(var_name, span)
        } else {
            self.resolve_naked_variant_silent(var_name, span)
        }
    }

    fn normalize_is_expr(&mut self, id: ExprId, span: Span) -> Option<Expr> {
        self.ast.get_expr(id).cloned().map(|expr| match expr {
            Expr::Is(scrutinee, ref pat) => {
                let resolved = self.resolve_type_pattern_silent(pat, span);
                if resolved != *pat {
                    self.ast
                        .set_expr(id, Expr::Is(scrutinee, resolved.clone()));
                }
                Expr::Is(scrutinee, resolved)
            }
            _ => expr,
        })
    }

    /// Get the type for a nullary (zero-arity) variant constructor.
    ///
    /// Used when resolving `Type.Variant` access for variants with no payload.
    /// Returns the appropriate type for builtin types (`Option`, `Result`,
    /// `Ordering`) or a generic `Named` type with fresh type variables.
    fn variant_type_for_nullary(&mut self, type_id: TypeId) -> TyId {
        if type_id == TypeId::OPTION {
            // Option.None has a fresh inner type
            let inner = self.fresh();
            self.ty_arena.option(inner)
        } else if type_id == TypeId::RESULT {
            // Nullary Result variants are not common; use fresh vars
            let ok = self.fresh();
            let err = self.fresh();
            self.ty_arena.result(ok, err)
        } else if type_id == TypeId::ORDERING {
            TyArena::ORDERING
        } else {
            // User-defined sum type: create fresh type args
            match self.registry.get_def(type_id) {
                Some(TypeDef::Sum { type_params, .. }) => {
                    let type_args: SmallVec<[TyId; 4]> =
                        type_params.iter().map(|_| self.fresh()).collect();
                    self.ty_arena.named(type_id, type_args)
                }
                _ => TyArena::ERROR,
            }
        }
    }

    /// Get a function type for a non-zero-arity variant constructor.
    ///
    /// Returns `(PayloadTypes) -> ResultType` where `ResultType` is the sum type.
    /// Used when `Type.Variant` is accessed but not immediately called, e.g.
    /// passed as a function value or used in a call expression.
    fn variant_ctor_fn_type(
        &mut self,
        type_id: TypeId,
        ty_name: QualifiedName,
        var_name: StringId,
        span: Span,
    ) -> TyId {
        let var_def = self.registry.lookup_variant(type_id, var_name);

        match var_def {
            None => {
                let tn = ty_name.display(&self.env.strings);
                let vn = self.env.resolve_string(var_name);
                self.error(TypeError::UnknownType(format!("{tn}.{vn}"), span));
                TyArena::ERROR
            }
            Some(vd) => {
                let (res, subst) = self.sum_type_with_fresh_args(type_id);
                let params: SmallVec<[TyId; 4]> = if type_id == TypeId::OPTION {
                    match self.ty_arena.get(res) {
                        Ty::Option(inner) => smallvec![*inner],
                        _ => smallvec![TyArena::ERROR],
                    }
                } else if type_id == TypeId::RESULT {
                    match (vd.idx, self.ty_arena.get(res)) {
                        (0, Ty::Result(ok, _)) => smallvec![*ok],
                        (1, Ty::Result(_, err)) => smallvec![*err],
                        _ => smallvec![TyArena::ERROR],
                    }
                } else if type_id == TypeId::ERROR {
                    smallvec![TyArena::STRING]
                } else {
                    let payloads = self
                        .decls
                        .variant_payloads(type_id, vd.name)
                        .cloned()
                        .unwrap_or_default();
                    payloads
                        .iter()
                        .map(|&id| self.convert().ast_type_to_ty(id, &subst))
                        .collect()
                };

                self.ty_arena.func(params, res)
            }
        }
    }

    /// Create a sum type with fresh type arguments and substitution map.
    fn sum_type_with_fresh_args(
        &mut self,
        type_id: TypeId,
    ) -> (TyId, IndexMap<StringId, TyId>) {
        if type_id == TypeId::OPTION {
            let inner = self.fresh();
            let map = iter::once((self.env.intern("T"), inner)).collect();
            (self.ty_arena.option(inner), map)
        } else if type_id == TypeId::RESULT {
            let ok = self.fresh();
            let err = self.fresh();
            let map = [(self.env.intern("T"), ok), (self.env.intern("E"), err)]
                .into_iter()
                .collect();
            (self.ty_arena.result(ok, err), map)
        } else if type_id == TypeId::ERROR {
            (TyArena::RUNTIME_ERROR, IndexMap::new())
        } else {
            match self.registry.get_def(type_id) {
                Some(TypeDef::Sum { type_params, .. }) => {
                    let args: SmallVec<[TyId; 4]> =
                        type_params.iter().map(|_| self.fresh()).collect();
                    let map: IndexMap<_, _> = type_params
                        .iter()
                        .zip(args.iter())
                        .map(|(&param, &ty)| (param, ty))
                        .collect();
                    (self.ty_arena.named(type_id, args), map)
                }
                _ => (TyArena::ERROR, IndexMap::new()),
            }
        }
    }

    /// Infer type of postfix operators.
    ///
    /// Uses the operator's type scheme to generate constraints and determine
    /// the result type.
    fn postfix(
        &mut self,
        id: ExprId,
        op: PostfixOp,
        inner_id: ExprId,
        span: Span,
    ) -> TyId {
        let inner_ty = self.expr(inner_id);
        let scheme = op.def(&mut self.ty_arena).ty;
        let ret = self.apply_op_scheme(&scheme, &[inner_ty], span);

        // Track Fallible instance for `!` dispatch on user types.
        // Same pattern as binary class dispatch: try immediate resolution,
        // fall back to deferred for unresolved type variables.
        if matches!(op, PostfixOp::Unwrap) {
            let kind = ClassId::FALLIBLE;
            let type_id = self.nominal_type_id(inner_ty);
            let inside_same = self
                .class_context
                .as_ref()
                .is_some_and(|ctx| ctx.class == kind && ctx.type_id == type_id);
            if !inside_same {
                match type_id {
                    Some(tid)
                        if self
                            .check_instance_available(kind, tid, span)
                            .is_some() =>
                    {
                        self.set_instance_call(id, tid);
                    }
                    _ => {
                        self.deferred_inst_calls.push((id, inner_ty, kind));
                    }
                }
            }
        }

        ret
    }

    /// Check if scrutinee type is compatible with a variant pattern.
    ///
    /// Returns `true` if pattern matching is valid. For type variables,
    /// always returns `false` since polymorphic types cannot be refined
    /// by variant patterns (the concrete type is unknown at compile time).
    ///
    /// This enforces parametricity: a function with `F: Fallible[T]` cannot
    /// inspect whether `F` is `Option` or `Result` at runtime.
    pub(super) fn scrutinee_compatible_with_variant(
        &mut self,
        scrutinee_ty: TyId,
        name: &QualifiedName,
    ) -> bool {
        let s = self.env.resolve_str(name.local_name());
        let ty = self.ty_arena.get(scrutinee_ty).clone();
        match ty {
            // Concrete Option/Result: check type name matches
            Ty::Option(_) => s == "Option",
            Ty::Result(_, _) => s == "Result",
            Ty::Ordering => s == "Ordering",
            Ty::DataStatus => s == "DataStatus",
            Ty::RuntimeError => s == "Error",

            // Named types: resolve pattern type name and compare TypeIds
            Ty::Named(scrutinee_id, _) => self
                .convert()
                .resolve_type_name(name)
                .is_some_and(|(pattern_id, _)| pattern_id == scrutinee_id),

            // Union: at least one member must be compatible
            Ty::Union(_, members) => members
                .iter()
                .any(|&m| self.scrutinee_compatible_with_variant(m, name)),

            // Type variable: reject only polymorphic parameters (universally quantified);
            // inference variables (from calls) are allowed since they resolve to concrete types
            Ty::Var(v) => !self.poly_param_vars.contains(&v),

            // HKT type variable application: `F[T]` where `F` is a type var;
            // if `F` is a polymorphic parameter, pattern matching is unsound
            Ty::Apply(tv, _) => !self.poly_param_vars.contains(&tv),

            // Error/Unknown: allow to avoid cascading errors
            Ty::Error | Ty::Unknown => true,

            // Other types: incompatible with variant patterns
            _ => false,
        }
    }

    /// Infer type of `is` expression.
    ///
    /// Always returns `Bool`. Pattern bindings are extracted by `Expr::If`
    /// and added to the then-branch scope; they are not bound here.
    ///
    /// # Pattern Handling
    ///
    /// - `Type`: runtime type check
    /// - `Variant(ty, var)`: zero-arity variant check
    /// - `VariantWildcard(ty, var)`: variant check ignoring payload
    /// - `VariantBind(ty, var, names)`: variant check with payload bindings
    ///   (bindings handled by enclosing `if`)
    /// - `Object(fields)`: structural object check
    fn is_check(
        &mut self,
        id: ExprId,
        scrutinee_id: ExprId,
        pattern: &TypePattern,
        span: Span,
    ) -> TyId {
        let scrutinee_ty = self.expr(scrutinee_id);
        let pattern = self.resolve_type_pattern(pattern, span);
        self.ast
            .set_expr(id, Expr::Is(scrutinee_id, pattern.clone()));

        match &pattern {
            TypePattern::Type(ty_id) => {
                let target_ty = self.ast_ty(*ty_id);
                self.interp
                    .is_patterns
                    .insert(id, CheckedTypePatternInfo::Type(target_ty));
                // Function types cannot be inspected at runtime for opaque
                // callables (class method refs, module fn refs, partial
                // apps); reject them here (at any depth) so the interpreter's
                // `fn_value_matches` never has to answer this question
                // dynamically. This catches `(Int) -> Int` directly and also
                // containers that smuggle one in (`[(Int) -> Int]`, tuples,
                // unions, `Named` type args, etc.).
                if Self::type_contains_fn(target_ty, &self.ty_arena) {
                    self.error(TypeError::FnTypeInPattern(span));
                } else if let Some(members) =
                    self.expand_union_members(scrutinee_ty)
                {
                    // If scrutinee is a union, verify target is a member.
                    // Skip check if target is the union type itself
                    // (e.g., `x is Storable`) or if target is also a union
                    // that contains the scrutinee members.
                    let target_is_same_union = scrutinee_ty == target_ty;
                    let target_is_member = members.contains(&target_ty)
                        || target_ty == TyArena::UNKNOWN;
                    if !target_is_same_union && !target_is_member {
                        self.error(TypeError::NotAUnionMember {
                            member: target_ty,
                            union_ty: scrutinee_ty,
                            span,
                        });
                    }
                }
            }
            TypePattern::Variant(ty_name, var_name)
            | TypePattern::VariantWildcard(ty_name, var_name) => {
                // Check scrutinee is compatible with variant pattern
                if !self
                    .scrutinee_compatible_with_variant(scrutinee_ty, ty_name)
                {
                    let pat_ty = self.env.resolve_string(ty_name.local_name());
                    self.error(TypeError::IncompatibleVariantPattern {
                        pattern_ty: pat_ty,
                        scrutinee_ty,
                        span,
                    });
                }

                // Validate that the variant exists
                let exists = self
                    .convert()
                    .resolve_type_name(ty_name)
                    .and_then(|(type_id, _)| {
                        self.registry.lookup_variant(type_id, *var_name)
                    })
                    .is_some();
                if !exists {
                    let tn = self.env.resolve_string(ty_name.local_name());
                    let vn = self.env.resolve_string(*var_name);
                    self.error(TypeError::UnknownType(
                        format!("{tn}.{vn}"),
                        span,
                    ));
                }
            }
            TypePattern::VariantBind(ty_name, var_name, names) => {
                // Check scrutinee is compatible with variant pattern
                if !self
                    .scrutinee_compatible_with_variant(scrutinee_ty, ty_name)
                {
                    let pat_ty = self.env.resolve_string(ty_name.local_name());
                    self.error(TypeError::IncompatibleVariantPattern {
                        pattern_ty: pat_ty,
                        scrutinee_ty,
                        span,
                    });
                }

                // Validate variant and arity; bindings are handled by IF
                let lookup = self
                    .convert()
                    .resolve_type_name(ty_name)
                    .and_then(|(type_id, _)| {
                        self.registry
                            .lookup_variant(type_id, *var_name)
                            .map(|v| (type_id, v))
                    });
                match lookup {
                    None => {
                        let tn = self.env.resolve_string(ty_name.local_name());
                        let vn = self.env.resolve_string(*var_name);
                        self.error(TypeError::UnknownType(
                            format!("{tn}.{vn}"),
                            span,
                        ));
                    }
                    Some((_, var_def)) => {
                        if var_def.arity as usize != names.len() {
                            self.error(TypeError::ArityMismatch {
                                expected: var_def.arity as usize,
                                got: names.len(),
                                span,
                            });
                        }
                    }
                }
            }
            TypePattern::Object(fields) => {
                // Validate that scrutinee could be an object with these fields
                match self.ty_arena.get(scrutinee_ty) {
                    Ty::Object(_) | Ty::Var(_) | Ty::Unknown | Ty::Error => {}
                    Ty::Named(type_id, _) => {
                        let type_id = *type_id;
                        if self.object_alias(type_id).is_none() {
                            self.error(TypeError::NotAnObject(
                                scrutinee_ty,
                                span,
                            ));
                        }
                    }
                    _ => {
                        self.error(TypeError::NotAnObject(scrutinee_ty, span));
                    }
                }
                // Resolve field types (validates type expressions) and
                // reject fn types at any depth for the same reason as above.
                let ftys = fields
                    .iter()
                    .map(|(name, ty_id)| {
                        let fty = self.ast_ty(*ty_id);
                        (*name, fty)
                    })
                    .collect::<Vec<_>>();
                ftys.iter().for_each(|(_, fty)| {
                    if Self::type_contains_fn(*fty, &self.ty_arena) {
                        self.error(TypeError::FnTypeInPattern(span));
                    }
                });
                self.interp
                    .is_patterns
                    .insert(id, CheckedTypePatternInfo::Object(ftys));
            }
            TypePattern::NakedVariant(_)
            | TypePattern::NakedVariantWildcard(_)
            | TypePattern::NakedVariantBind(..) => {}
        }

        TyArena::BOOL
    }

    /// Infer type of `as` cast expression.
    ///
    /// Emits an `Into` constraint to verify the conversion is valid.
    /// The actual validation happens in `check_into` during constraint solving.
    ///
    /// Special case: when casting a type variable to a numeric type, also
    /// emit a `Numeric` constraint to ensure polymorphic expressions like
    /// `(-2.9) as Int` are properly constrained.
    fn as_cast(
        &mut self,
        id: ExprId,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> TyId {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_ty(ty_id);
        let tspan = self.ast.type_expr_span(ty_id).unwrap_or(span);
        self.interp.expr_targets.insert(id, target_ty);
        self.newtype_edge_checks.push((
            id,
            inner_ty,
            target_ty,
            tspan,
            self.current_module.clone(),
        ));

        // Emit Into constraint for validation
        self.constrain(Constraint::Class {
            ty: inner_ty,
            class: TypeClass::param(ClassId::INTO, target_ty),
            span: tspan,
        });

        // Special case: type variable cast to numeric requires Numeric constraint
        // This allows `(-2.9) as Int` where `-2.9` has polymorphic Numeric type
        let inner_is_var = matches!(self.ty_arena.get(inner_ty), Ty::Var(_));
        let target_is_numeric = matches!(
            self.ty_arena.get(target_ty),
            Ty::Int | Ty::Float | Ty::Word
        );
        if inner_is_var && target_is_numeric {
            self.constrain(Constraint::Class {
                ty: inner_ty,
                class: TypeClass::simple(ClassId::NUMERIC),
                span,
            });
        }

        // Error recovery: return Error type if either side is Error
        if inner_ty == TyArena::ERROR || target_ty == TyArena::ERROR {
            TyArena::ERROR
        } else if matches!(self.ty_arena.get(target_ty), Ty::Union(..)) {
            self.interp.union_value_reprs.insert(id, inner_ty);
            target_ty
        } else {
            target_ty
        }
    }

    /// Infer type of `read` conversion expression.
    ///
    /// `expr read T` returns `Result[T, String]`. The conversion is fallible;
    /// if the value cannot be converted to `T`, an error message is returned.
    ///
    /// Emits a `TryInto` constraint to validate that the conversion is possible
    /// at compile time; function types, regex, and refs cannot be used with `read`.
    fn read_conv(
        &mut self,
        id: ExprId,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> TyId {
        let inner_ty = self.expr(inner_id);
        let target_ty = self.ast_ty(ty_id);
        let tspan = self.ast.type_expr_span(ty_id).unwrap_or(span);
        self.interp.expr_targets.insert(id, target_ty);
        self.read_checks.push((
            id,
            inner_ty,
            target_ty,
            tspan,
            self.current_module.clone(),
        ));

        // Emit TryInto constraint for validation
        self.constrain(Constraint::Class {
            ty: inner_ty,
            class: TypeClass::param(ClassId::TRY_INTO, target_ty),
            span: tspan,
        });

        self.ty_arena.result(target_ty, TyArena::STRING)
    }

    /// If `ty` is `Ty::Named(id, args)` and `id` is an alias, expand it and
    /// store the solved alias mapping.
    pub(super) fn expand_alias_for_read(&mut self, ty: TyId) {
        self.collect_alias_expansions(ty, &mut HashSet::new());
        if let Ty::Named(type_id, args) = self.ty_arena.get(ty).clone() {
            if let Some(TypeDef::Alias { type_params, .. }) =
                self.registry.get_def(type_id)
            {
                let target = self.decls.alias_target(type_id);
                let type_params = type_params.clone();
                let subst: IndexMap<StringId, TyId> = type_params
                    .iter()
                    .zip(args.iter())
                    .map(|(&p, &a)| (p, a))
                    .collect();
                let expanded = self.convert().ast_type_to_ty(target, &subst);
                self.interp.alias_type_expansions.insert(ty, expanded);
                self.collect_alias_expansions(expanded, &mut HashSet::new());
            }
        }
    }

    pub(super) fn collect_alias_expansions(
        &mut self,
        ty: TyId,
        seen: &mut HashSet<TyId>,
    ) {
        if seen.insert(ty) {
            match self.ty_arena.get(ty).clone() {
                Ty::Named(type_id, args) => {
                    let alias = self.registry.get_def(type_id).and_then(
                        |def| match def {
                            TypeDef::Alias { type_params, .. } => Some((
                                self.decls.alias_target(type_id),
                                type_params.clone(),
                            )),
                            _ => None,
                        },
                    );
                    match alias {
                        Some((target, type_params)) => {
                            let subst: IndexMap<StringId, TyId> = type_params
                                .iter()
                                .zip(args.iter())
                                .map(|(&p, &a)| (p, a))
                                .collect();
                            let expanded =
                                self.convert().ast_type_to_ty(target, &subst);
                            self.interp
                                .alias_type_expansions
                                .insert(ty, expanded);
                            self.collect_alias_expansions(expanded, seen);
                        }
                        None => {
                            args.iter().for_each(|&arg| {
                                self.collect_alias_expansions(arg, seen)
                            });
                        }
                    }
                }
                Ty::Array(inner) | Ty::Option(inner) => {
                    self.collect_alias_expansions(inner, seen);
                }
                Ty::Result(ok, err) | Ty::Map(ok, err) => {
                    self.collect_alias_expansions(ok, seen);
                    self.collect_alias_expansions(err, seen);
                }
                Ty::Tuple(elems) | Ty::Union(_, elems) => {
                    elems.iter().for_each(|&elem| {
                        self.collect_alias_expansions(elem, seen)
                    });
                }
                Ty::Fn(params, ret) => {
                    params.iter().for_each(|&param| {
                        self.collect_alias_expansions(param, seen)
                    });
                    self.collect_alias_expansions(ret, seen);
                }
                Ty::Object(fields) => {
                    fields.values().for_each(|&field| {
                        self.collect_alias_expansions(field, seen)
                    });
                }
                Ty::Var(_)
                | Ty::Bool
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
                | Ty::Apply(_, _)
                | Ty::AssocType(_, _, _)
                | Ty::Unknown
                | Ty::Error => {}
            }
        }
    }

    /// Infer type of a database intrinsic (`@get`, `@set`, `@kill`, etc.).
    ///
    /// Validates transaction requirements for mutating intrinsics, typechecks
    /// the value expression for `@set`, and populates the `TxnId` field in the
    /// AST based on current transaction context.
    ///
    /// When the target is `RefTarget::Inline(DbRef::Local(name, []))` and `name`
    /// is a variable of type `Ref`, rewrites to `RefTarget::Expr`.
    fn intrinsic(
        &mut self,
        id: ExprId,
        op: Intrinsic,
        rt: &RefTarget,
        val: Option<ExprId>,
        span: Span,
    ) -> TyId {
        let def = op.def(&mut self.ty_arena);
        let resolved_rt = self.resolve_ref_target(rt, span).into_owned();

        // Validate transaction requirement for mutating intrinsics
        if def.txn == TxnReq::Globals {
            match op {
                Intrinsic::Set => self.set_validate(&resolved_rt, span),
                Intrinsic::Kill => self.kill_validate(&resolved_rt, span),
                _ => {}
            }
        }

        // For Set, also typecheck the value expression
        if let Some(v) = val {
            let v_ty = self.expr(v);
            let storable = self.ty_arena.storable();
            self.constrain(Constraint::Class {
                ty: v_ty,
                class: TypeClass::param(ClassId::INTO, storable),
                span,
            });
        }

        self.ast.set_expr(
            id,
            Expr::Intrinsic(op, resolved_rt, val, self.in_transaction),
        );

        def.ty.return_ty(&self.ty_arena).unwrap_or(TyArena::ERROR)
    }

    /// Resolve a `RefTarget`, checking for variable references.
    ///
    /// When the target is `RefTarget::Inline(DbRef::Local(name, []))` and `name`
    /// is a variable of type `Ref`, rewrites to `RefTarget::Expr` referencing
    /// that variable. Otherwise, type-checks subscripts and returns as-is.
    ///
    /// Returns `Cow::Borrowed` when unchanged, `Cow::Owned` when rewritten.
    pub(super) fn resolve_ref_target<'a>(
        &mut self,
        rt: &'a RefTarget,
        span: Span,
    ) -> Cow<'a, RefTarget> {
        match rt {
            RefTarget::Inline(dbref) => {
                match dbref {
                    DbRef::Local(name, subs) if subs.is_empty() => {
                        // Check if name is a variable of type Ref
                        let ref_ty = self.env.lookup(*name).and_then(|s| {
                            let (ty, _) =
                                s.instantiate(&mut self.uf, &mut self.ty_arena);
                            self.ty_arena.get(ty).is_ref().then_some(ty)
                        });
                        ref_ty.map_or_else(
                            // Not a Ref variable; treat as DB local
                            || Cow::Borrowed(rt),
                            |ty| {
                                // Create a Var expression and rewrite to Expr
                                match self.ast.add_expr(Expr::Var(*name), span)
                                {
                                    Ok(var_id) => {
                                        self.record_type(var_id, ty);
                                        Cow::Owned(RefTarget::Expr(var_id))
                                    }
                                    Err(e) => {
                                        // Arena overflow; report error and fall back
                                        self.error(TypeError::Custom {
                                            msg: e.to_string(),
                                            span,
                                        });
                                        Cow::Borrowed(rt)
                                    }
                                }
                            },
                        )
                    }
                    DbRef::Local(_, subs) | DbRef::Global(_, subs) => {
                        // Has subscripts or is global; check subscripts
                        self.check_subscript_elems(subs, span);
                        Cow::Borrowed(rt)
                    }
                }
            }
            RefTarget::Expr(e) => {
                let ty = self.expr(*e);
                // Ensure expression is a Ref type (Local, Global, or Ref union)
                let is_ref = self.ty_arena.get(ty).is_ref();
                let is_var_or_err =
                    matches!(self.ty_arena.get(ty), Ty::Var(_) | Ty::Error);
                if !is_ref && !is_var_or_err {
                    let expected = self.ty_arena.ref_ty();
                    self.error(TypeError::Mismatch {
                        // Display hint: use Ref union (Local | Global)
                        expected,
                        got: ty,
                        span,
                    });
                }
                Cow::Borrowed(rt)
            }
        }
    }

    /// Type-check subscript elements.
    ///
    /// For `Elem`, adds an `Into[Subscript]` constraint.
    /// For `Spread`, constrains to `Array[Subscript]`.
    pub(super) fn check_subscript_elems(
        &mut self,
        subs: &[SubscriptElem],
        span: Span,
    ) {
        subs.iter().for_each(|elem| match elem {
            SubscriptElem::Elem(id) => {
                let ty = self.expr(*id);
                let subscript = self.ty_arena.subscript();
                self.constrain(Constraint::Class {
                    ty,
                    class: TypeClass::param(ClassId::INTO, subscript),
                    span,
                });
            }
            SubscriptElem::Spread(id) => {
                let ty = self.expr(*id);
                // Spread must be Array[Subscript]
                let subscript = self.ty_arena.subscript();
                let expected = self.ty_arena.array(subscript);
                self.unify(ty, expected, span);
            }
        });
    }

    /// Infer type of type annotation expression `(expr) : Type`.
    ///
    /// Infers the inner expression type, parses the annotation, and unifies
    /// them. Returns the annotation type (which is the expected type).
    ///
    /// Special case: when the inner expression is an array literal and the
    /// annotation is `Array[UnionType]`, uses bidirectional typing to check
    /// each element against the union instead of inferring then unifying.
    /// This allows `[1, "a"]: Array[Int | String]` to produce a union-typed
    /// array rather than falling back to `Json`.
    fn annotate(
        &mut self,
        id: ExprId,
        inner_id: ExprId,
        ty_id: AstTypeExprId,
        span: Span,
    ) -> TyId {
        let ann_ty = self.ast_ty(ty_id);
        let tspan = self.ast.type_expr_span(ty_id).unwrap_or(span);
        self.interp.expr_targets.insert(id, ann_ty);

        // Clone inner expression to avoid borrow issues
        let inner_expr = self.ast.get_expr(inner_id).cloned();

        // Reject negative literals for Word type
        let is_word = matches!(self.ty_arena.get(ann_ty), Ty::Word);
        let is_neg =
            matches!(inner_expr.as_ref(), Some(Expr::Unary(UnOp::Neg, _)));
        let inner_ty = if is_word && is_neg {
            self.error(TypeError::NegativeWord(span));
            let inner_ty = self.expr(inner_id);
            (inner_ty, TyArena::WORD)
        }
        // Try special case: array literal with `Array[UnionType]`
        else {
            let arr_elem = match self.ty_arena.get(ann_ty) {
                Ty::Array(e) => Some(*e),
                _ => None,
            };
            match (arr_elem, inner_expr.as_ref()) {
                (Some(elem_ty), Some(Expr::Array(elems)))
                    if self.expand_union_members(elem_ty).is_some() =>
                {
                    let result = self.array_with_expected(elems, elem_ty, span);
                    self.record_type(inner_id, result);
                    (result, result)
                }
                _ => {
                    // Default: infer then unify
                    let inner_ty = self.expr(inner_id);
                    if !self.reject_private_repr_ann(inner_ty, ann_ty, tspan) {
                        self.unify(inner_ty, ann_ty, tspan);
                    }
                    if matches!(self.ty_arena.get(ann_ty), Ty::Union(..)) {
                        self.interp.union_value_reprs.insert(id, inner_ty);
                    }
                    (inner_ty, ann_ty)
                }
            }
        };
        self.newtype_edge_checks.push((
            id,
            inner_ty.0,
            ann_ty,
            tspan,
            self.current_module.clone(),
        ));
        inner_ty.1
    }

    /// Infer type of array literal with expected union element type.
    ///
    /// When annotating an array with `Array[UnionType]`, each element must be
    /// a member of the union. Returns `Array[expected_elem]` if all elements
    /// match, or `Ty::Error` if any element fails.
    pub(super) fn array_with_expected(
        &mut self,
        elems: &[ArrayElem],
        expected_elem: TyId,
        span: Span,
    ) -> TyId {
        let members = self.expand_union_members(expected_elem);

        elems.iter().for_each(|elem| match elem {
            ArrayElem::Elem(id) => {
                let elem_ty = self.expr(*id);
                // Check element is a member of the expected union
                if elem_ty != TyArena::ERROR {
                    let is_member = members.as_ref().is_some_and(|ms| {
                        ms.iter().any(|m| self.types_compatible(elem_ty, *m))
                    });
                    if !is_member {
                        self.error(TypeError::Mismatch {
                            expected: expected_elem,
                            got: elem_ty,
                            span,
                        });
                    } else {
                        self.record_type(*id, expected_elem);
                        self.interp.union_value_reprs.insert(*id, elem_ty);
                    }
                }
            }
            ArrayElem::Spread(id) => {
                let spread_ty = self.expr(*id);
                // Spread must be Array[T] where T is compatible with expected
                let spread_shape = self.ty_arena.get(spread_ty).clone();
                match spread_shape {
                    Ty::Array(inner) => {
                        let is_member = members.as_ref().is_some_and(|ms| {
                            ms.iter().any(|m| self.types_compatible(inner, *m))
                        });
                        if !is_member {
                            let exp_arr = self.ty_arena.array(expected_elem);
                            let got_arr = self.ty_arena.array(inner);
                            self.error(TypeError::Mismatch {
                                expected: exp_arr,
                                got: got_arr,
                                span,
                            });
                        }
                    }
                    Ty::Var(_) => {
                        let exp_arr = self.ty_arena.array(expected_elem);
                        self.unify(spread_ty, exp_arr, span);
                    }
                    Ty::Error => {}
                    _ => {
                        self.error(TypeError::NotAnArray(spread_ty, span));
                    }
                }
            }
        });

        self.ty_arena.array(expected_elem)
    }

    /// Infer type of `loop` expression.
    ///
    /// `loop seed (state, cont) => body` is a continuation-passing loop:
    /// - `seed` is the initial state value
    /// - `state` is bound to the current state in each iteration
    /// - `cont` is a pseudo-function that, when called with a new state,
    ///   continues the loop; not calling it exits and returns the body value
    ///
    /// Typing rules:
    /// - `state` has the same type as `seed` (or its annotation)
    /// - `cont` has type `(StateType) -> BodyType`
    /// - The overall expression returns `BodyType`
    pub(super) fn loop_expr(
        &mut self,
        seed: ExprId,
        state_param: &(StringId, Option<AstTypeExprId>),
        cont_param: &(StringId, Option<AstTypeExprId>),
        body: ExprId,
        span: Span,
    ) -> TyId {
        // Infer seed type
        let seed_ty = self.expr(seed);

        // State parameter type: annotation or unify with seed
        let state_ty =
            state_param.1.map(|id| self.ast_ty(id)).unwrap_or(seed_ty);

        // Unify seed with state type
        self.unify(seed_ty, state_ty, span);

        // Create fresh type variable for body/result type
        let body_ty = self.fresh();

        // Continuation type: (StateType) -> BodyType
        let cont_ty = self.ty_arena.func(smallvec![state_ty], body_ty);

        // Check cont_param annotation if present
        if let Some(ann_id) = cont_param.1 {
            let ann_ty = self.ast_ty(ann_id);
            self.unify(cont_ty, ann_ty, span);
        }

        // Push scope, bind parameters, infer body
        self.env.push_scope();
        self.env.bind(state_param.0, Scheme::mono(state_ty));
        self.env.bind(cont_param.0, Scheme::mono(cont_ty));
        let inferred_body_ty = self.expr(body);
        self.env.pop_scope();

        // Unify inferred body type with result type
        self.unify(inferred_body_ty, body_ty, span);

        body_ty
    }

    /// Typecheck a transaction block expression; assigns a unique `TxnId`.
    ///
    /// Returns `Result[T, String]` where `T` is the trailing expression type
    /// (or `Unit` if no trailing expression).
    ///
    /// Nested transactions are rejected at compile time (not runtime).
    pub(super) fn transaction(
        &mut self,
        id: ExprId,
        txn: &TransactionExpr,
        span: Span,
    ) -> TyId {
        // Nested transactions rejected at compile time
        if self.in_transaction.is_some() {
            self.error(TypeError::Custom {
                msg: "nested transactions are not supported".to_string(),
                span,
            });
            // Continue with a fresh ID anyway to allow further inference
        }

        // Assign unique ID
        let txn_id = TxnId::new(self.next_txn_id);
        self.next_txn_id += 1;

        // Set transaction context
        let prev = self.in_transaction.replace(txn_id);

        // Enter new scope for transaction body
        self.env.push_scope();

        // Typecheck all statements
        txn.stmts.iter().for_each(|&stmt_id| {
            self.stmt(stmt_id);
        });

        // Typecheck trailing expression or default to Unit
        let inner_ty =
            txn.expr.map_or(TyArena::UNIT, |expr_id| self.expr(expr_id));

        // Typecheck timeout modifier if present
        txn.modifiers.timeout.iter().for_each(|&timeout_id| {
            let timeout_ty = self.expr(timeout_id);
            self.unify(timeout_ty, TyArena::INT, span);
        });

        self.env.pop_scope();

        // Restore previous transaction context
        self.in_transaction = prev;

        // Update AST with assigned ID
        let updated = TransactionExpr {
            id: Some(txn_id),
            stmts: txn.stmts.clone(),
            expr: txn.expr,
            modifiers: txn.modifiers,
        };
        self.ast.set_expr(id, Expr::Transaction(updated));

        // Return Result[T, String]
        self.ty_arena.result(inner_ty, TyArena::STRING)
    }
}
