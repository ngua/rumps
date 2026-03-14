//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::{
    ClassContext, ClassInstanceInput, Constraint, InferCtx, InstanceMethodInput,
};
use crate::ast::{
    AssocTypeDef, AstTypeExpr, AstTypeExprId, BindingPattern, DbRef, Expr,
    ExprId, Import, ImportItem, OutputFormat, OutputTarget, RefTarget, Stmt,
    StmtId, TypeDefAst, TypeParam, UnOp, Visibility, WriteExpr,
};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::{self, Instance};
use crate::typecheck::ty::{
    BuiltinClass, BuiltinClassTag, Scheme, Subst, Ty, TyArena, TyId, TyVar,
};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Infer types for a statement.
    ///
    /// Most statements don't produce a type, but function definitions
    /// bind the function name with its inferred type scheme in the environment.
    pub(crate) fn stmt(&mut self, id: StmtId) {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::Import(_)) => {
                // Imports are processed during hoisting; here we only check
                // ordering (imports must appear at top of scope)
                if !self.env.imports_allowed() {
                    self.error(TypeError::Custom {
                        msg: "imports must appear at the top of a scope"
                            .to_string(),
                        span,
                    });
                }
                // Don't call import_stmt(); already processed during hoisting
            }

            Some(Stmt::Fun {
                name,
                type_params,
                params,
                ret,
                body,
                ..
            }) => {
                self.env.mark_non_import();
                self.fun(name, &type_params, &params, ret.as_ref(), body, span);
            }

            Some(Stmt::Let(pattern, ann, rhs, _)) => {
                self.env.mark_non_import();
                self.r#let(&pattern, ann.as_ref(), rhs, span);
            }

            Some(Stmt::Expr(expr)) => {
                self.env.mark_non_import();
                self.expr(expr);
            }

            Some(Stmt::Type {
                ref type_params,
                ref def,
                ..
            }) => {
                self.env.mark_non_import();
                // Type definitions are registered in the registry, but we
                // still validate that all type expressions in variant
                // payloads are fully saturated.
                self.validate_type_decl_body(type_params, def);
            }

            Some(Stmt::Union {
                ref type_params,
                ref members,
                ..
            }) => {
                self.env.mark_non_import();
                // Union members are registered in the registry, but we
                // still validate that all member type expressions are
                // fully saturated.
                let subst = self.type_param_subst(type_params);
                members.iter().for_each(|m| {
                    self.ast_type_to_ty(*m, &subst);
                });
            }

            Some(Stmt::NewType {
                ref type_params,
                target,
                ..
            }) => {
                self.env.mark_non_import();
                // Aliases are registered in the registry, but we still
                // validate that the target type expression is fully
                // saturated (e.g. `newtype G = Array` is invalid because
                // `Array` expects a type argument).
                let subst = self.type_param_subst(type_params);
                self.ast_type_to_ty(target, &subst);
            }

            Some(Stmt::Module { name, body }) => {
                self.env.mark_non_import();
                self.user_module_with_path(name, &body, span);
            }

            Some(Stmt::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types,
                methods,
            }) => {
                self.env.mark_non_import();
                self.class_instance(ClassInstanceInput {
                    class_name,
                    class_args: &class_args,
                    type_params: &type_params,
                    for_type,
                    constraints: &constraints,
                    methods: &methods,
                    assoc_types: &assoc_types,
                    module: None,
                    span,
                });
            }

            None => {}
        }
    }

    /// Infer types for a top-level user-defined module.
    ///
    /// Delegates to `user_module` with an empty path prefix.
    fn user_module_with_path(
        &mut self,
        mod_name: StringId,
        body: &[StmtId],
        span: Span,
    ) {
        self.user_module(QualifiedName::local(mod_name), body, span)
    }

    /// Infer types for a user-defined module.
    ///
    /// Validates that only `fun`, `let`, and nested `module` statements appear
    /// inside, typechecks each item, and registers the module's types so they
    /// can be accessed via `ModuleName.fn(...)` or `ModuleName.const`.
    ///
    /// The `mod_path` is the fully-qualified module path (e.g., `"Outer.Inner"`
    /// for a nested module).
    fn user_module(
        &mut self,
        mod_path: QualifiedName,
        body: &[StmtId],
        span: Span,
    ) {
        // Register the module name FIRST so self-references like
        // `Geometry.pi` from within `Geometry.area` resolve correctly.
        let disp = mod_path.display(&self.env.strings);
        let mod_path_id = self.env.intern(&disp);
        self.env.register_user_module(mod_path_id);

        // Save and set current module for unqualified type resolution
        let prev_module = self.current_module.replace(mod_path.clone());

        // Typecheck each statement and validate it's an allowed item type.
        // We also collect type information for registration.
        body.iter().for_each(|&id| {
            let item_span = self.ast.stmt_span(id).unwrap_or(span);
            let item = self.ast.get_stmt(id).cloned();

            match item {
                Some(Stmt::Fun { ref name, vis, .. }) => {
                    // Typecheck the function (binds it in current scope)
                    self.stmt(id);
                    // Register as module member with visibility
                    if let Some(scheme) = self.env.lookup(*name).cloned() {
                        self.env.register_user_module_member(
                            mod_path_id,
                            *name,
                            scheme,
                            vis,
                        );
                    }
                }

                Some(Stmt::Let(ref pat, _, _, vis)) => {
                    // Module constants must be simple bindings (not destructuring)
                    match pat {
                        BindingPattern::Var(ref const_name) => {
                            self.stmt(id);
                            // Register as module member with visibility
                            if let Some(scheme) =
                                self.env.lookup(*const_name).cloned()
                            {
                                self.env.register_user_module_member(
                                    mod_path_id,
                                    *const_name,
                                    scheme,
                                    vis,
                                );
                            }
                        }
                        _ => {
                            self.error(TypeError::Custom {
                                msg:
                                    "module constants must be simple bindings, \
                                     not destructuring patterns"
                                        .to_string(),
                                span: item_span,
                            });
                        }
                    }
                }

                Some(Stmt::Module { ref name, ref body }) => {
                    // Nested module; recurse with qualified path
                    self.user_module(mod_path.child(*name), body, item_span);
                }

                // Invalid statements inside a module (SET/kill/write are now
                // `Stmt::Expr` wrapping their expression forms)
                Some(Stmt::Expr(..)) => {
                    self.error(TypeError::Custom {
                        msg: "expression statements are not allowed inside a \
                              module"
                            .to_string(),
                        span: item_span,
                    });
                }
                Some(Stmt::Type {
                    ref name,
                    vis,
                    ref type_params,
                    ref def,
                }) => {
                    let qn = mod_path.child(*name);
                    let qn_disp = qn.display(&self.env.strings);
                    let qname_id = self.env.intern(&qn_disp);
                    self.env.register_user_module_type_vis(qname_id, vis);
                    self.validate_type_decl_body(type_params, def);
                }
                Some(Stmt::Union {
                    ref name,
                    vis,
                    ref type_params,
                    ref members,
                }) => {
                    let qn = mod_path.child(*name);
                    let qn_disp = qn.display(&self.env.strings);
                    let qname_id = self.env.intern(&qn_disp);
                    self.env.register_user_module_type_vis(qname_id, vis);
                    let subst = self.type_param_subst(type_params);
                    members.iter().for_each(|m| {
                        self.ast_type_to_ty(*m, &subst);
                    });
                }
                Some(Stmt::NewType {
                    ref name,
                    vis,
                    ref type_params,
                    target,
                }) => {
                    let qn = mod_path.child(*name);
                    let qn_disp = qn.display(&self.env.strings);
                    let qname_id = self.env.intern(&qn_disp);
                    self.env.register_user_module_type_vis(qname_id, vis);
                    let subst = self.type_param_subst(type_params);
                    self.ast_type_to_ty(target, &subst);
                }
                Some(Stmt::Import(_)) => {
                    // Imports inside modules are processed during hoisting;
                    // nothing to do here in Pass 2
                }
                Some(Stmt::ClassInstance {
                    ref class_name,
                    ref class_args,
                    ref type_params,
                    for_type,
                    ref constraints,
                    ref assoc_types,
                    ref methods,
                }) => {
                    self.class_instance(ClassInstanceInput {
                        class_name: *class_name,
                        class_args,
                        type_params,
                        for_type,
                        constraints,
                        methods,
                        assoc_types,
                        module: Some(mod_path_id),
                        span: item_span,
                    });
                }
                None => {}
            }
        });

        // Restore previous module
        self.current_module = prev_module;
    }

    /// Process an import statement.
    ///
    /// Validates module and member existence, checks visibility, and binds
    /// imported names in the current scope with their types.
    ///
    /// Called during both hoisting (for type imports) and Pass 2 (for full
    /// processing). Visibility is `pub(super)` so `hoist.rs` can call it.
    pub(super) fn import_stmt(&mut self, import: &Import, span: Span) {
        let mod_path_str = self.env.strings.join_path(&import.path);
        let mod_path_id = self.env.intern(&mod_path_str);

        // Check if module exists (builtin or user-defined)
        let is_builtin = import
            .path
            .first()
            .is_some_and(|&name| self.runtime_env.is_builtin_module(name));
        let is_user = self.env.is_user_module(mod_path_id);

        if !is_builtin && !is_user {
            self.error(TypeError::Custom {
                msg: format!("unknown module `{}`", mod_path_str),
                span,
            });
            // Continue to gather more errors
        }

        // Mark valid modules as imported (enables class instance lookup)
        if is_builtin || is_user {
            self.env.mark_module_imported(mod_path_id);
        }

        // Collect exclusions and check for wildcard
        let mut has_wildcard = false;
        let mut exclusions: HashSet<StringId> = HashSet::new();

        import.items.iter().for_each(|item| match item {
            ImportItem::Wildcard => has_wildcard = true,
            ImportItem::Exclude(ref name) => {
                if !has_wildcard {
                    self.error(TypeError::Custom {
                        msg: "exclusions (`-name`) only valid after `...`"
                            .to_string(),
                        span,
                    });
                }
                exclusions.insert(*name);
            }
            ImportItem::Named { .. } => {}
        });

        // Process wildcard: import all public members
        if has_wildcard {
            // Get public members from builtin module
            if is_builtin {
                if let Some(m) = import
                    .path
                    .first()
                    .and_then(|&name| self.runtime_env.get_builtin_module(name))
                {
                    m.public_members().into_iter().for_each(
                        |(name, scheme)| {
                            if !exclusions.contains(&name) {
                                self.env.bind(name, scheme);
                            }
                        },
                    );
                }
            }
            // Get public members from user module
            if is_user {
                self.env
                    .get_public_user_module_members(mod_path_id)
                    .into_iter()
                    .for_each(|(name_id, scheme)| {
                        if !exclusions.contains(&name_id) {
                            self.env.bind(name_id, scheme);
                        }
                    });

                // Import public types
                self.env
                    .get_public_user_module_types(mod_path_id)
                    .into_iter()
                    .for_each(|(local_id, qname_id)| {
                        if !exclusions.contains(&local_id) {
                            self.env.import_type(local_id, qname_id);
                        }
                    });
            }
        }

        // Process named imports
        import.items.iter().for_each(|item| {
            if let ImportItem::Named { name, alias } = item {
                let bind_id = alias.unwrap_or(*name);
                let n = self.env.resolve_str(*name).to_owned();
                let mut full_path: SmallVec<[StringId; 4]> =
                    import.path.iter().copied().collect();
                full_path.push(*name);

                // Try builtin module first (always public)
                let builtin = self
                    .runtime_env
                    .get_module_fn_type(&full_path)
                    .cloned()
                    .or_else(|| {
                        self.runtime_env
                            .get_module_const_type(&full_path)
                            .map(Scheme::mono)
                    });

                // Then try user module (check visibility)
                let user =
                    self.env.lookup_user_module_member(mod_path_id, *name);

                match (builtin, user) {
                    (Some(s), _) => self.env.bind(bind_id, s),
                    (None, Some(m)) if m.vis == Visibility::Public => {
                        self.env.bind(bind_id, m.scheme.clone());
                    }
                    (None, Some(_)) => {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "member `{}` is private in module `{}`",
                                n, mod_path_str
                            ),
                            span,
                        });
                    }
                    (None, None) => {
                        // Check if it's a type
                        let qname = format!("{}.{}", mod_path_str, n);
                        let qname_id = self.env.intern(&qname);
                        match self.env.lookup_user_module_type_vis(qname_id) {
                            Some(Visibility::Public) => {
                                // Register as imported type
                                self.env.import_type(bind_id, qname_id);
                            }
                            Some(Visibility::Private) => {
                                self.error(TypeError::Custom {
                                    msg: format!(
                                        "type `{}` is private in module `{}`",
                                        n, mod_path_str
                                    ),
                                    span,
                                });
                            }
                            None => {
                                self.error(TypeError::Custom {
                                    msg: format!(
                                        "member `{}` not found in module `{}`",
                                        n, mod_path_str
                                    ),
                                    span,
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    /// Infer type of a named function definition.
    ///
    /// Named functions support recursion: the function name is bound with a
    /// provisional type (fresh vars for params/return) before inferring the body.
    /// After inference, the type is generalized and the binding is updated.
    ///
    /// For generic functions (`fn foo[T](x: T) -> T`), explicit type parameters
    /// are bound as fresh type variables before inferring parameter/return types.
    fn fun(
        &mut self,
        name: StringId,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) {
        // Capture outer env free vars BEFORE binding function (for generalization)
        let outer_free = self.env.free_vars(&self.ty_arena);

        // First pass: create fresh type variables for all type parameters
        let name_to_tv: HashMap<StringId, TyVar> = type_params
            .iter()
            .map(|tp| (tp.name, self.fresh_var()))
            .collect();

        // Build `type_param_subst` (`StringId -> TyId`) for type resolution
        let type_param_subst: IndexMap<_, _> = type_params
            .iter()
            .map(|tp| {
                let tv = name_to_tv[&tp.name];
                (tp.name, self.ty_arena.alloc(Ty::Var(tv)))
            })
            .collect();

        // Second pass: process constraints now that all type params are known
        // Convert `BuiltinClass<AstTypeExprId>` to `BuiltinClass<TyId>` for `Scheme`
        let mut scheme_constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]> =
            SmallVec::new();

        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[&tp.name];
            let ty = self.ty_arena.alloc(Ty::Var(tv));

            tp.constraints.iter().for_each(|c| {
                let class = self.ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((tv, class.clone()));

                // Emit constraint for checking the function body
                self.constrain(Constraint::Class { ty, class, span });
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        // Declared return type annotation (if any)
        let declared_ret =
            ret.map(|id| self.ast_type_to_ty(*id, &type_param_subst));

        // Fresh var for provisional return (supports recursive calls)
        let provisional_ret = self.fresh();
        let provisional_fn = self
            .ty_arena
            .func(param_tys.iter().copied().collect(), provisional_ret);
        self.env.bind(name, Scheme::mono(provisional_fn));

        self.env.push_scope();
        self.bind_params(params, &param_tys);

        // Register type param vars as polymorphic parameters (cannot be refined)
        name_to_tv.values().for_each(|&tv| {
            self.poly_param_vars.insert(tv);
        });

        // Infer body type
        let body_ty = self.expr(body);

        // Pop parameter scope
        self.env.pop_scope();

        // Determine actual return type: use annotation if present, else body type
        let actual_ret = match declared_ret {
            Some(ret_ty) => {
                self.unify(body_ty, ret_ty, span);
                ret_ty
            }
            None => body_ty,
        };

        // Link provisional return var with actual (for recursive call consistency)
        self.unify(provisional_ret, actual_ret, span);

        // Build final function type and generalize
        let fn_ty = self
            .ty_arena
            .func(param_tys.iter().copied().collect(), actual_ret);
        let ty_vars = self.ty_arena.free_vars(fn_ty);
        // Include all declared type params (they may only appear in constraints,
        // not in the function type itself; e.g. `T` in `[T, F: Fallible[T]]`)
        let declared_tvs: HashSet<_> = name_to_tv.values().copied().collect();
        let vars: Vec<_> = ty_vars
            .union(&declared_tvs)
            .copied()
            .filter(|v| !outer_free.contains(v))
            .collect();
        let scheme = Scheme {
            vars,
            ty: fn_ty,
            constraints: scheme_constraints,
        };
        self.env.bind(name, scheme);
    }

    /// Infer types for a `let` statement.
    ///
    /// Infers the RHS type, optionally unifies with an annotation, then
    /// binds variables from the pattern with appropriate types.
    ///
    /// Special case: when RHS is an array literal and annotation is
    /// `Array[UnionType]`, uses bidirectional typing to allow heterogeneous
    /// arrays that match the union members.
    ///
    /// For extensible records: when the RHS is a structural object and the
    /// annotation is a named struct, we bind with the full object type
    /// (preserving extra fields) rather than the narrower annotation type.
    ///
    /// For polymorphic closures: uses the stored scheme from `closure_schemes`
    /// for proper generalization instead of monomorphizing.
    fn r#let(
        &mut self,
        pattern: &BindingPattern,
        ann: Option<&AstTypeExprId>,
        rhs: ExprId,
        span: Span,
    ) {
        // If annotation present, parse and unify.
        // Returns `None` if pattern was already bound (special case).
        let ty = match ann {
            None => Some(self.expr(rhs)),
            Some(id) => {
                let ann_ty = self.ast_type_to_ty(*id, &IndexMap::new());

                // Clone to avoid borrow issues with mutable self
                let rhs_expr = self.ast.get_expr(rhs).cloned();

                // Copy what we need from arena before mutable calls
                let ann_shape = self.ty_arena.get(ann_ty).clone();

                // Reject negative literals for Word type
                if let (Ty::Word, Some(Expr::Unary(UnOp::Neg, _))) =
                    (&ann_shape, &rhs_expr)
                {
                    self.error(TypeError::NegativeWord(span));
                    self.expr(rhs);
                    self.bind_pattern(pattern, TyArena::WORD, span);
                    None
                // Try special case: array literal with `Array[UnionType]`
                } else if let (Ty::Array(elem_id), Some(Expr::Array(elems))) =
                    (&ann_shape, &rhs_expr)
                {
                    let elem_id = *elem_id;
                    if self.expand_union_members(elem_id).is_some() {
                        let result =
                            self.array_with_expected(elems, elem_id, span);
                        self.record_type(rhs, result);
                        self.bind_pattern(pattern, result, span);
                        None // Already bound
                    } else {
                        self.infer_default_let(ann_ty, rhs, span)
                    }
                } else {
                    self.infer_default_let(ann_ty, rhs, span)
                }
            }
        };

        // Check if RHS is a polymorphic closure (has stored scheme)
        // This is done AFTER inferring since closure() stores the scheme
        let closure_scheme = self.closure_schemes.remove(&rhs);

        // Bind variables from the pattern (if not already done)
        if let Some(ty) = ty {
            // For polymorphic closures, use the stored scheme directly
            match (&pattern, closure_scheme) {
                (BindingPattern::Var(name), Some(scheme)) => {
                    self.env.bind(*name, scheme);
                }
                _ => self.bind_pattern(pattern, ty, span),
            }
        }
    }

    /// Default inference for `let` with type annotation.
    fn infer_default_let(
        &mut self,
        ann_ty: TyId,
        rhs: ExprId,
        span: Span,
    ) -> Option<TyId> {
        let rhs_ty = self.expr(rhs);
        self.unify(rhs_ty, ann_ty, span);

        // Extensible records: if rhs is an object and annotation
        // is an alias to object, keep the full object type to
        // preserve extra fields
        let ann_shape = self.ty_arena.get(ann_ty).clone();
        let is_obj_alias = matches!(ann_shape, Ty::Named(id, _)
        if self.registry.get_def(id).is_some_and(|def| match def {
            TypeDef::Alias { target, .. } => self
                .ast
                .get_type_expr(*target)
                .is_some_and(|te| matches!(te, AstTypeExpr::Object(_))),
            _ => false,
        }));

        // Preserve concrete type for:
        // 1. Extensible records (object rhs with object alias annotation)
        // 2. Ref types (Local/Global rhs with Ref union annotation)
        let rhs_shape = self.ty_arena.get(rhs_ty).clone();
        let use_rhs_ty = (matches!(&rhs_shape, Ty::Object(_)) && is_obj_alias)
            || (matches!(&rhs_shape, Ty::Local | Ty::Global)
                && matches!(ann_shape, Ty::Named(id, _) if id == TypeId::REF));

        Some(if use_rhs_ty { rhs_ty } else { ann_ty })
    }

    /// Bind variables from a binding pattern to types in the environment.
    ///
    /// Recursively descends into the pattern, extracting types from the
    /// value type and binding each variable with the appropriate type.
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &BindingPattern,
        ty: TyId,
        span: Span,
    ) {
        match pattern {
            BindingPattern::Var(name) => {
                self.env.bind(*name, Scheme::mono(ty));
            }

            BindingPattern::Wildcard => {
                // No binding
            }

            BindingPattern::Tuple(pats) => {
                // Copy the shape out before mutable calls
                let shape = self.ty_arena.get(ty).clone();
                let elem_tys: SmallVec<[TyId; 4]> = match shape {
                    Ty::Tuple(ts) => {
                        if ts.len() != pats.len() {
                            self.error(TypeError::ArityMismatch {
                                expected: pats.len(),
                                got: ts.len(),
                                span,
                            });
                            smallvec![TyArena::ERROR; pats.len()]
                        } else {
                            ts
                        }
                    }
                    Ty::Var(_) => {
                        // Create fresh vars for each element and constrain
                        let fresh: SmallVec<[TyId; 4]> =
                            (0..pats.len()).map(|_| self.fresh()).collect();
                        let tup = self.ty_arena.alloc(Ty::Tuple(fresh.clone()));
                        self.unify(ty, tup, span);
                        fresh
                    }
                    Ty::Error => smallvec![TyArena::ERROR; pats.len()],
                    _ => {
                        self.error(TypeError::NotATuple(ty, span));
                        smallvec![TyArena::ERROR; pats.len()]
                    }
                };
                pats.iter()
                    .zip(elem_tys.iter())
                    .for_each(|(p, &t)| self.bind_pattern(p, t, span));
            }

            BindingPattern::Object(fields) => {
                fields.iter().for_each(|(name, sub)| {
                    let n =
                        self.env.get_str(*name).unwrap_or_default().to_owned();
                    let fty = self.field_type(ty, &n, span);
                    self.bind_pattern(sub, fty, span);
                });
            }

            BindingPattern::Array(_, _) => {
                // Array destructuring is only allowed in match expressions
                self.error(TypeError::ArrayPatternInLet(span));
            }
        }
    }

    /// Validate a `set` operation with a resolved `RefTarget`.
    ///
    /// Global writes require transaction context.
    pub(super) fn set_validate(&mut self, rt: &RefTarget, span: Span) {
        let needs_txn = match rt {
            RefTarget::Inline(dbref) => matches!(dbref, DbRef::Global(..)),
            RefTarget::Expr(e) => {
                // Look up the type to determine scope. The expression should
                // already be typechecked by `resolve_ref_target`; if not found,
                // default to requiring transaction (safer; produces an error
                // rather than silently allowing an unsafe global write).
                self.get_type(*e).is_none_or(|id| {
                    matches!(self.ty_arena.get(id), Ty::Global)
                })
            }
        };
        if needs_txn && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }
    }

    /// Validate a `kill` operation with a resolved `RefTarget`.
    ///
    /// Global writes require transaction context.
    pub(super) fn kill_validate(&mut self, rt: &RefTarget, span: Span) {
        let needs_txn = match rt {
            RefTarget::Inline(dbref) => matches!(dbref, DbRef::Global(..)),
            RefTarget::Expr(e) => {
                // Look up the type to determine scope. The expression should
                // already be typechecked by `resolve_ref_target`; if not found,
                // default to requiring transaction (safer; produces an error
                // rather than silently allowing an unsafe global write).
                self.get_type(*e).is_none_or(|id| {
                    matches!(self.ty_arena.get(id), Ty::Global)
                })
            }
        };
        if needs_txn && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }
    }

    /// Infer types for a `write` statement or expression.
    ///
    /// Type-checks the expression and adds constraints based on format and target:
    /// - `Into[String]` for default format
    /// - `Into[Json]` for JSON format
    /// - `FilePath | String` for file target path
    pub(super) fn write(&mut self, output: &WriteExpr, span: Span) {
        let expr_ty = self.expr(output.expr);

        // Format constraint: must be convertible to target format
        match output.format {
            OutputFormat::Default | OutputFormat::Raw => {
                // Must be convertible to String
                self.constrain(Constraint::Class {
                    ty: expr_ty,
                    class: BuiltinClass::Parameterized(
                        BuiltinClassTag::Into,
                        TyArena::STRING,
                    ),
                    span,
                });
            }
            OutputFormat::Json => {
                // Must be convertible to Json
                self.constrain(Constraint::Class {
                    ty: expr_ty,
                    class: BuiltinClass::Parameterized(
                        BuiltinClassTag::Into,
                        TyArena::JSON,
                    ),
                    span,
                });
            }
        }

        // Target constraint
        match output.target {
            OutputTarget::Stdout | OutputTarget::Stderr => {}
            OutputTarget::File(path_expr) => {
                // Path must be FilePath or String
                let path_ty = self.expr(path_expr);
                let union_ty = self.ty_arena.alloc(Ty::Union(smallvec![
                    TyArena::FILEPATH,
                    TyArena::STRING,
                ]));
                self.unify(path_ty, union_ty, span);
            }
        }
    }

    /// Infer types for a `class ... FOR ...` instance declaration.
    ///
    /// Validates:
    /// 1. The class name is a valid `BuiltinClassTag`
    /// 2. The `for_type` is NOT a builtin type
    /// 3. All required methods are present
    /// 4. Method signatures match the class definition (arity)
    /// 5. Method bodies typecheck correctly
    ///
    /// Registers the instance in `InstanceRegistry` on success.
    fn class_instance(
        &mut self,
        input: ClassInstanceInput<'_, &SmallVec<[AssocTypeDef; 2]>>,
    ) {
        let ClassInstanceInput {
            class_name,
            class_args,
            type_params,
            for_type,
            constraints,
            methods,
            assoc_types,
            module,
            span,
        } = input;

        // 1. Resolve class name to BuiltinClassTag
        let cn = self.env.resolve_str(class_name).to_owned();
        let class = BuiltinClassTag::from_str(&cn).unwrap_or_else(|| {
            self.error(TypeError::UnknownClass(cn.clone(), span));
            BuiltinClassTag::Display // Default to `Display` to avoid cascading errors
        });

        // 2. Build type parameter substitution map (BEFORE resolving for_type)
        //    If `type_params` is empty, extract type param names from the
        //    WHERE constraints (e.g., `L: Display, R: Display` gives `[L, R]`)
        let mut type_param_subst: IndexMap<_, _> = if type_params.is_empty() {
            constraints
                .iter()
                .map(|(name, _)| {
                    let tv = self.fresh_var();
                    (*name, self.ty_arena.alloc(Ty::Var(tv)))
                })
                .collect()
        } else {
            type_params
                .iter()
                .map(|tp| {
                    let tv = self.fresh_var();
                    (tp.name, self.ty_arena.alloc(Ty::Var(tv)))
                })
                .collect()
        };

        // Merge type vars from `for_type` (e.g. `T` in `X[T]`)
        self.merge_for_type_vars(for_type, &mut type_param_subst);

        // 3. Resolve for_type and get its TypeId
        let for_ty = self.ast_type_to_ty(for_type, &type_param_subst);
        let type_id = self.extract_type_id(for_ty);

        // 4. Convert class args to `TyId` (needed for builtin check)
        let class_arg_tys: SmallVec<[TyId; 2]> = class_args
            .iter()
            .map(|id| self.ast_type_to_ty(*id, &type_param_subst))
            .collect();

        // 5. Check for forbidden builtin instance
        //
        // We allow implementing classes for builtin types IF the class has
        // type args that include user-defined types. For example:
        //   - `class Display FOR Int` is forbidden (builtin has Display)
        //   - `class Into[String] FOR Int` is forbidden (builtin has Into[String])
        //   - `class Into[UserId] FOR Int` is ALLOWED (no builtin Into[UserId])
        //
        // The heuristic: if for_type is builtin AND all class args are builtin,
        // reject. If any class arg is a user type, we allow it.
        if let Some(tid) = type_id {
            if self.is_builtin_type(tid) {
                let all_args_builtin = class_arg_tys.is_empty()
                    || class_arg_tys.iter().all(|&ty| self.is_builtin_ty(ty));
                if all_args_builtin {
                    self.error(TypeError::BuiltinInstanceForbidden {
                        class,
                        type_id: tid,
                        span,
                    });
                }
            }
        }

        // 6. Process WHERE constraints
        let mut scheme_constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]> =
            SmallVec::new();
        constraints
            .iter()
            .for_each(|(param_name, param_constraints)| {
                let ty_id = type_param_subst
                    .get(param_name)
                    .copied()
                    .unwrap_or(TyArena::UNKNOWN);
                let tv = match self.ty_arena.get(ty_id) {
                    Ty::Var(v) => *v,
                    _ => self.fresh_var(),
                };
                param_constraints.iter().for_each(|c| {
                    scheme_constraints.push((
                        tv,
                        self.ast_class_to_ty_class(c, &type_param_subst),
                    ));
                });
            });

        // 6.5. Process associated type definitions and set class context
        //
        // This enables bare `:Index` references inside method bodies to resolve
        // to the concrete type defined in this instance.
        let assoc_type_map: HashMap<_, _> = assoc_types
            .iter()
            .map(|def| {
                let ty = self.ast_type_to_ty(def.target, &type_param_subst);
                (def.name, ty)
            })
            .collect();

        // 6.6. Validate all required associated types are provided
        let req_assocs = self.env.class_def(class).assoc_types;
        req_assocs.iter().for_each(|req| {
            let req_id = self.env.intern(req);
            if !assoc_type_map.contains_key(&req_id) {
                self.error(TypeError::MissingAssocType {
                    class,
                    assoc: req_id,
                    span,
                });
            }
        });

        // Set class context for method body type checking
        self.class_context = Some(ClassContext {
            class,
            type_id,
            assoc_types: assoc_type_map.clone(),
        });

        // 7. Collect provided method names
        let provided_methods: HashSet<StringId> =
            methods.iter().map(|m| m.name).collect();

        // 8. Check all required methods are present
        let required: Vec<&str> =
            self.env.class_def(class).method_names().collect();
        let required_hint = required.join(", ");
        required.iter().for_each(|req| {
            let req_id = self.env.intern(req);
            if !provided_methods.contains(&req_id) {
                self.error(TypeError::MissingInstanceMethod {
                    class,
                    method: req.to_string(),
                    required_hint: required_hint.clone(),
                    span,
                });
            }
        });

        // 9. Typecheck each method
        methods.iter().for_each(|m| {
            self.instance_method(InstanceMethodInput {
                class,
                for_ty,
                class_arg_tys: &class_arg_tys,
                type_param_subst: &type_param_subst,
                method: m,
                inst_span: span,
            });
        });

        // 9.5. Clear class context after method processing
        self.class_context = None;

        // 10. Register instance (if we have a valid type_id)
        let type_name = self.extract_type_name_from_ast(for_type);
        if let Some(tid) = type_id {
            let method_map: HashMap<_, _> = methods
                .iter()
                .map(|m| {
                    let mn = self.env.resolve_str(m.name);
                    let fn_name =
                        crate::interpreter::instance::instance_fn_name(
                            class, &type_name, mn,
                        );
                    let fn_name_id = self.env.intern(&fn_name);
                    (m.name, fn_name_id)
                })
                .collect();

            let type_var_params: SmallVec<[TyVar; 2]> = type_param_subst
                .values()
                .filter_map(|&id| match self.ty_arena.get(id) {
                    Ty::Var(v) => Some(*v),
                    _ => None,
                })
                .collect();

            // Skip registration if already hoisted (avoid duplicate error)
            if self.instance_registry.lookup(class, tid).is_none() {
                // Convert AST associated types to instance associated types
                let inst_assoc_types: SmallVec<[instance::AssocTypeDef; 1]> =
                    assoc_types
                        .iter()
                        .map(|def| {
                            let ty = assoc_type_map
                                .get(&def.name)
                                .copied()
                                .unwrap_or(TyArena::UNKNOWN);
                            let constraints = def
                                .constraint
                                .as_ref()
                                .map(|c| {
                                    self.ast_class_to_ty_class(
                                        c,
                                        &type_param_subst,
                                    )
                                })
                                .into_iter()
                                .collect();
                            instance::AssocTypeDef {
                                name: def.name,
                                ty,
                                constraints,
                                span: def.span,
                            }
                        })
                        .collect();

                let inst = Instance {
                    class,
                    class_args: class_arg_tys,
                    type_params: type_var_params,
                    constraints: scheme_constraints,
                    methods: method_map,
                    assoc_types: inst_assoc_types,
                    module,
                    span,
                };

                if let Err(e) = self.instance_registry.register(tid, inst) {
                    self.error(e);
                }
            }
        }
    }

    /// Typecheck a single instance method definition.
    ///
    /// Validates that the method signature matches the class definition and
    /// typechecks the method body.
    fn instance_method(&mut self, input: InstanceMethodInput<'_>) {
        let InstanceMethodInput {
            class,
            for_ty,
            class_arg_tys,
            type_param_subst,
            method,
            inst_span,
        } = input;

        let m_span = method.span;

        // Get expected method signature from class
        let mn = self.env.resolve_str(method.name).to_owned();
        let expected = self.env.class_def(class).method(&mn, m_span).cloned();

        // Handle unknown method error
        let (expected_param_tys, expected_ret_ty) = match expected {
            Ok(spec) => {
                let scheme = spec.scheme();
                // The first quantified var represents `Self` in class methods.
                // Subsequent vars represent class type parameters (e.g., `U` in
                // `Into[U]`). We substitute both `Self` and class arg types.
                let self_var = scheme.vars.first().copied();
                let class_arg_vars: Vec<_> =
                    scheme.vars.iter().skip(1).copied().collect();
                // Extract param and return types, substituting vars
                let shape = self.ty_arena.get(scheme.ty).clone();
                match shape {
                    Ty::Fn(params, ret) => {
                        let subst_id = |ctx: &mut Self, ty: TyId| -> TyId {
                            let mut r =
                                ctx.subst_self_type(ty, self_var, for_ty);
                            class_arg_vars
                                .iter()
                                .zip(class_arg_tys.iter())
                                .for_each(|(&var, &arg_ty)| {
                                    r = ctx.subst_tyvar(r, var, arg_ty);
                                });
                            r
                        };
                        let ps: Vec<_> =
                            params.iter().map(|&p| subst_id(self, p)).collect();
                        let r = subst_id(self, ret);
                        (ps, r)
                    }
                    _ => (vec![], TyArena::UNKNOWN),
                }
            }
            Err(e) => {
                self.error(e);
                (vec![], TyArena::UNKNOWN)
            }
        };

        // Check arity
        if method.params.len() != expected_param_tys.len() {
            self.error(TypeError::MethodSignatureMismatch {
                class,
                method: mn.clone(),
                expected: expected_param_tys.len(),
                got: method.params.len(),
                span: m_span,
            });
        }

        // Typecheck method body
        self.env.push_scope();

        // Bind parameters with user-provided types (or inferred)
        let param_tys =
            self.param_tys_with_subst(&method.params, type_param_subst);

        // Unify user param types with expected param types
        param_tys.iter().zip(expected_param_tys.iter()).for_each(
            |(&user_ty, &exp_ty)| {
                self.unify(user_ty, exp_ty, m_span);
            },
        );

        self.bind_params(&method.params, &param_tys);

        // Infer body type
        let body_ty = self.expr(method.body);

        // Determine expected return type (user annotation or class signature)
        let ret_ty = method
            .ret
            .map(|ret_id| self.ast_type_to_ty(ret_id, type_param_subst))
            .unwrap_or(expected_ret_ty);

        // Unify body with return type
        self.unify(body_ty, ret_ty, m_span);

        // Also unify with class's expected return type (catches wrong annotation)
        if expected_ret_ty != TyArena::UNKNOWN {
            self.unify(ret_ty, expected_ret_ty, inst_span);
        }

        self.env.pop_scope();
    }

    /// Extract a `TypeId` from a `TyId`, if it represents a named/aliased type.
    fn extract_type_id(&self, id: TyId) -> Option<TypeId> {
        match self.ty_arena.get(id) {
            Ty::Named(tid, _) => Some(*tid),
            Ty::Unknown | Ty::Error => None,
            ty => self.primitive_type_id(ty),
        }
    }

    /// Get the type name from an `AstTypeExprId` for function name generation.
    pub(super) fn extract_type_name_from_ast(
        &self,
        id: AstTypeExprId,
    ) -> String {
        self.ast
            .get_type_expr(id)
            .and_then(|te| match te {
                AstTypeExpr::Named(name) | AstTypeExpr::App(name, _) => {
                    Some(name.display(&self.env.strings))
                }
                _ => None,
            })
            .unwrap_or_else(|| "Unknown".to_string())
    }

    /// Check if a `TypeId` represents a builtin type.
    pub(super) fn is_builtin_type(&self, id: TypeId) -> bool {
        self.registry
            .get_def(id)
            .is_some_and(|def| matches!(def, TypeDef::Builtin(_)))
    }

    /// Check if a `Ty` represents a builtin type.
    ///
    /// Returns `true` for primitive types (`Int`, `String`, etc.) and for
    /// `Ty::Named` referencing a builtin. Returns `false` for user-defined
    /// types and type variables.
    pub(super) fn is_builtin_ty(&self, id: TyId) -> bool {
        match self.ty_arena.get(id) {
            // Primitives are builtin
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
            | Ty::Local
            | Ty::Global => true,
            // Parameterized builtins
            Ty::Array(_)
            | Ty::Map(_, _)
            | Ty::Tuple(_)
            | Ty::Option(_)
            | Ty::Result(_, _) => true,
            // Named: check registry
            Ty::Named(tid, _) => self.is_builtin_type(*tid),
            // Everything else (Var, Fn, Object, Union, Unknown, Error)
            _ => false,
        }
    }

    /// Get the `TypeId` for a primitive `Ty`.
    pub(crate) fn primitive_type_id(&self, ty: &Ty) -> Option<TypeId> {
        match ty {
            Ty::Bool => Some(TypeId::BOOL),
            Ty::Int => Some(TypeId::INT),
            Ty::Word => Some(TypeId::WORD),
            Ty::Float => Some(TypeId::FLOAT),
            Ty::Char => Some(TypeId::CHAR),
            Ty::String => Some(TypeId::STRING),
            Ty::Unit => Some(TypeId::UNIT),
            Ty::Time => Some(TypeId::TIME),
            Ty::Range => Some(TypeId::RANGE),
            Ty::Json => Some(TypeId::JSON),
            Ty::Ordering => Some(TypeId::ORDERING),
            Ty::DataStatus => Some(TypeId::DATA_STATUS),
            Ty::FilePath => Some(TypeId::FILEPATH),
            Ty::Path => Some(TypeId::PATH),
            Ty::Regex => Some(TypeId::REGEX),
            Ty::Local => Some(TypeId::LOCAL),
            Ty::Global => Some(TypeId::GLOBAL),
            _ => None,
        }
    }

    /// Substitute the `Self` type variable in a class method signature.
    ///
    /// Class methods like `Display:display` have signature `forall T: Display. (T) -> String`
    /// where `T` represents the implementing type. When checking an instance, we substitute
    /// this type var with the actual `for_ty`.
    fn subst_self_type(
        &mut self,
        ty: TyId,
        self_var: Option<TyVar>,
        for_ty: TyId,
    ) -> TyId {
        self_var.map_or(ty, |sv| self.subst_tyvar(ty, sv, for_ty))
    }

    /// Recursively substitute a type variable with a concrete type.
    ///
    /// Delegates to `TyArena::apply` with a singleton substitution.
    fn subst_tyvar(&mut self, ty: TyId, var: TyVar, replacement: TyId) -> TyId {
        let subst = Subst::singleton(var, replacement);
        self.ty_arena.apply(ty, &subst)
    }

    /// Build a type parameter substitution map from a list of `TypeParam`s.
    ///
    /// Each type parameter is mapped to a fresh type variable.
    fn type_param_subst(
        &mut self,
        tps: &[TypeParam],
    ) -> IndexMap<StringId, TyId> {
        tps.iter()
            .map(|tp| {
                let tv = self.fresh_var();
                (tp.name, self.ty_arena.alloc(Ty::Var(tv)))
            })
            .collect()
    }

    /// Validate that all type expressions in a `type` declaration body are
    /// fully saturated (no unsaturated type synonyms like bare `Array`).
    fn validate_type_decl_body(&mut self, tps: &[TypeParam], def: &TypeDefAst) {
        let subst = self.type_param_subst(tps);
        match def {
            TypeDefAst::Sum(variants) => {
                variants.iter().for_each(|v| {
                    v.payloads.iter().for_each(|p| {
                        self.ast_type_to_ty(*p, &subst);
                    });
                });
            }
        }
    }
}
