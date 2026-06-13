//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use itertools::Itertools;
use smallvec::{smallvec, SmallVec};

use super::constraint_region::{ConstraintKey, ConstraintRegion};
use super::scheme::SchemePolicy;
use super::{
    ClassContext, ClassDefaultMethodInput, ClassInstanceInput, Constraint,
    HoistCtx, HoistState, InferCtx, InstanceMethodInput, MethodBodyInput,
    NewtypeIntoOverlap,
};
use crate::ast::{
    ArrayElem, AssocTypeDef, AstTypeExpr, AstTypeExprId, BindingPattern, DbRef,
    Expr, ExprId, Import, ImportItem, InstanceMethodDef, ObjectEntry,
    OutputFormat, OutputTarget, RefTarget, Stmt, StmtId, TypeDefAst, TypeParam,
    UnOp, Visibility, WriteExpr,
};
use crate::intern::{QualifiedName, StringId};
use crate::interpreter::instance::RuntimeInstance;
use crate::typecheck::env::MethodRefOrigin;
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::{self, Instance};
use crate::typecheck::ty::{
    ClassShape, Rename, Scheme, Ty, TyArena, TyId, TyVar, TypeClass,
};
use crate::value::{TypeDef, TypeId};
use crate::{ClassId, Span};

struct FunReq<'a> {
    stmt: StmtId,
    name: StringId,
    tps: &'a SmallVec<[TypeParam; 2]>,
    params: &'a SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
    ret: Option<&'a AstTypeExprId>,
    body: ExprId,
    span: Span,
}

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
                // Don't call import(); already processed during hoisting
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
                self.fun(FunReq {
                    stmt: id,
                    name,
                    tps: &type_params,
                    params: &params,
                    ret: ret.as_ref(),
                    body,
                    span,
                });
            }

            Some(Stmt::Let(pattern, ann, rhs, _)) => {
                self.env.mark_non_import();
                if self.final_let_done(id) {
                    self.restore_final_let(id);
                } else {
                    self.r#let(id, &pattern, ann.as_ref(), rhs, span);
                }
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
                    let ty = self.convert().ast_type_to_ty(*m, &subst);
                    let span = self.ast.type_expr_span(*m).unwrap_or(span);
                    self.require_wf_ty(ty, span);
                });
            }

            Some(Stmt::Newtype {
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
                let ty = self.convert().ast_type_to_ty(target, &subst);
                let span = self.ast.type_expr_span(target).unwrap_or(span);
                self.require_wf_ty(ty, span);
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

            // Class definitions are processed during hoisting (Pass 1).
            Some(Stmt::ClassDef { .. }) => {
                self.env.mark_non_import();
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
        self.env.register_user_module(mod_path.clone());

        // Save and set current module for unqualified type resolution
        let prev_module = self.current_module.replace(mod_path.clone());
        self.env.push_scope();
        self.install_final_let_schemes(body);

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
                            mod_path.clone(),
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
                            self.env.mark_non_import();
                            if self.final_let_done(id) {
                                self.restore_final_let(id);
                            } else {
                                self.stmt(id);
                            }
                            if let Some(scheme) =
                                self.env.lookup(*const_name).cloned()
                            {
                                let origin = self
                                    .env
                                    .lookup_method_ref_origin(*const_name);
                                self.env.register_user_module_member(
                                    mod_path.clone(),
                                    *const_name,
                                    scheme,
                                    vis,
                                );
                                origin.into_iter().for_each(|origin| {
                                    self.env
                                        .set_user_module_member_method_origin(
                                            &mod_path,
                                            *const_name,
                                            origin,
                                        );
                                });
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
                    ..
                }) => {
                    let qn = mod_path.child(*name);
                    self.env.register_user_module_type_vis(qn, vis);
                    self.validate_type_decl_body(type_params, def);
                }
                Some(Stmt::Union {
                    ref name,
                    vis,
                    ref type_params,
                    ref members,
                    ..
                }) => {
                    let qn = mod_path.child(*name);
                    self.env.register_user_module_type_vis(qn, vis);
                    let subst = self.type_param_subst(type_params);
                    members.iter().for_each(|m| {
                        self.convert().ast_type_to_ty(*m, &subst);
                    });
                }
                Some(Stmt::Newtype {
                    ref name,
                    vis,
                    ref type_params,
                    target,
                    ..
                }) => {
                    let qn = mod_path.child(*name);
                    self.env.register_user_module_type_vis(qn, vis);
                    let subst = self.type_param_subst(type_params);
                    self.convert().ast_type_to_ty(target, &subst);
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
                        module: Some(mod_path.clone()),
                        span: item_span,
                    });
                }
                // Class definitions are processed during hoisting.
                Some(Stmt::ClassDef { .. }) => {}
                None => {}
            }
        });

        // Restore previous module
        self.env.pop_scope();
        self.current_module = prev_module;
    }

    /// Process an import statement.
    ///
    /// Validates module and member existence, checks visibility, and binds
    /// imported names in the current scope with their types.
    ///
    /// Called during both hoisting (for type imports) and Pass 2 (for full
    /// processing). Visibility is `pub(super)` so `hoist.rs` can call it.
    pub(super) fn import(&mut self, import: &Import, span: Span) {
        let mod_qn = QualifiedName::new(import.path.to_vec());

        // Check if module exists (builtin or user-defined)
        let is_builtin = import
            .path
            .first()
            .is_some_and(|&name| self.runtime_env.is_builtin_module(name));
        let is_user = self.env.is_user_module(&mod_qn);

        if !is_builtin && !is_user {
            let mod_path_str = mod_qn.display(&self.env.strings);
            self.error(TypeError::Custom {
                msg: format!("unknown module `{}`", mod_path_str),
                span,
            });
            // Continue to gather more errors
        }

        // Mark valid modules as imported (enables class instance lookup).
        // `mark_module_imported` uses `StringId` (for scope-based tracking);
        // use the root path segment to track module imports.
        if is_builtin || is_user {
            if let Some(&name) = import.path.first() {
                self.env.mark_module_imported(name);
            }
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
            if is_user && self.defer_missing_import_members {
                self.deferred_imports.push((
                    import.clone(),
                    span,
                    self.current_module.clone(),
                ));
            }
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
                    .get_public_user_module_members(&mod_qn)
                    .into_iter()
                    .for_each(|(name, s, origin)| {
                        if !exclusions.contains(&name) {
                            self.bind_imported_member(name, s, origin);
                        }
                    });

                // Import public types
                self.env
                    .get_public_user_module_types(&mod_qn)
                    .into_iter()
                    .for_each(|(local_id, qn)| {
                        if !exclusions.contains(&local_id) {
                            self.env.import_type(local_id, qn);
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
                    self.env.lookup_user_module_member(&mod_qn, *name).cloned();

                let mod_path_str = mod_qn.display(&self.env.strings);
                match (builtin, user) {
                    (Some(s), _) => self.env.bind(bind_id, s),
                    (None, Some(m)) if m.vis == Visibility::Public => {
                        self.bind_imported_member(
                            bind_id,
                            m.scheme,
                            m.method_origin,
                        );
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
                        let type_qn = mod_qn.child(*name);
                        match self.env.lookup_user_module_type_vis(&type_qn) {
                            Some(Visibility::Public) => {
                                // Register as imported type
                                self.env.import_type(bind_id, type_qn);
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
                                if is_user && self.defer_missing_import_members {
                                    self.deferred_imports.push((
                                        Import {
                                            path: import.path.clone(),
                                            items: vec![ImportItem::Named {
                                                name: *name,
                                                alias: *alias,
                                            }],
                                        },
                                        span,
                                        self.current_module.clone(),
                                    ));
                                } else {
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
            }
        });
    }

    pub(super) fn replay_deferred_imports_for(
        &mut self,
        module: Option<&QualifiedName>,
    ) {
        self.replay_deferred_imports_matching(|_, m| {
            Self::deferred_module_matches(module, m)
        });
    }

    pub(super) fn replay_deferred_imports_for_paths(
        &mut self,
        module: Option<&QualifiedName>,
        paths: &HashSet<QualifiedName>,
    ) {
        self.replay_deferred_imports_matching(|import, m| {
            let path = QualifiedName::new(import.path.to_vec());
            Self::deferred_module_matches(module, m) && paths.contains(&path)
        });
    }

    fn deferred_module_matches(
        module: Option<&QualifiedName>,
        found: &Option<QualifiedName>,
    ) -> bool {
        match (module, found) {
            (Some(target), Some(found)) => found == target,
            (None, None) => true,
            _ => false,
        }
    }

    fn replay_deferred_imports_matching<F>(&mut self, mut f: F)
    where
        F: FnMut(&Import, &Option<QualifiedName>) -> bool,
    {
        let (ready, rest): (Vec<_>, Vec<_>) = self
            .deferred_imports
            .clone()
            .into_iter()
            .partition(|(import, _, m)| f(import, m));
        self.deferred_imports = rest;
        self.replay_deferred_imports(ready);
    }

    fn replay_deferred_imports(
        &mut self,
        imports: Vec<(Import, Span, Option<QualifiedName>)>,
    ) {
        imports.into_iter().for_each(|(import, span, m)| {
            let prev = match m {
                Some(ref qn) => self.current_module.replace(qn.clone()),
                None => self.current_module.take(),
            };
            self.replay_deferred_import(&import, span);
            self.current_module = prev;
        });
    }

    pub(super) fn replay_deferred_import(
        &mut self,
        import: &Import,
        span: Span,
    ) {
        let mod_qn = QualifiedName::new(import.path.to_vec());
        let is_user = self.env.is_user_module(&mod_qn);
        let mod_path_str = mod_qn.display(&self.env.strings);
        let mut exclusions: HashSet<StringId> = HashSet::new();

        import.items.iter().for_each(|item| {
            if let ImportItem::Exclude(name) = item {
                exclusions.insert(*name);
            }
        });

        import.items.iter().for_each(|item| match item {
            ImportItem::Wildcard if is_user => {
                self.env
                    .get_public_user_module_members(&mod_qn)
                    .into_iter()
                    .for_each(|(name, s, origin)| {
                        if !exclusions.contains(&name) {
                            self.bind_imported_member(name, s, origin);
                        }
                    });
            }
            ImportItem::Named { name, alias } if is_user => {
                let bind_id = alias.unwrap_or(*name);
                let n = self.env.resolve_str(*name).to_owned();
                match self
                    .env
                    .lookup_user_module_member(&mod_qn, *name)
                    .cloned()
                {
                    Some(m) if m.vis == Visibility::Public => {
                        self.bind_imported_member(
                            bind_id,
                            m.scheme,
                            m.method_origin,
                        );
                    }
                    Some(_) => {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "member `{}` is private in module `{}`",
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
            _ => {}
        });
    }

    fn bind_imported_member(
        &mut self,
        bind: StringId,
        s: Scheme,
        origin: Option<MethodRefOrigin>,
    ) {
        self.env.bind(bind, s);
        origin.into_iter().for_each(|origin| {
            self.env.bind_method_ref_origin(bind, origin);
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
    fn fun(&mut self, req: FunReq<'_>) {
        let FunReq {
            stmt: stmt_id,
            name,
            tps: type_params,
            params,
            ret,
            body,
            span,
        } = req;

        // Capture outer env free vars BEFORE binding function (for generalization)
        let outer_free = self.env.free_vars(&self.ty_arena, &mut self.uf);
        let constraint_start = self.constraints.len();

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
        // Convert `TypeClass<AstTypeExprId>` to `TypeClass<TyId>` for `Scheme`
        let mut scheme_constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
            SmallVec::new();

        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[&tp.name];
            let ty = self.ty_arena.alloc(Ty::Var(tv));

            tp.constraints.iter().for_each(|c| {
                let class =
                    self.convert().ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((tv, class.clone()));

                // Emit constraint for checking the function body
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
                            scheme_constraints.push((tv, sc.clone()));
                            self.constrain(Constraint::Class {
                                ty,
                                class: sc,
                                span,
                            });
                        }
                    });
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        // Declared return type annotation (if any)
        let declared_ret = ret.map(|id| {
            (
                self.convert().ast_type_to_ty(*id, &type_param_subst),
                self.ast.type_expr_span(*id).unwrap_or(span),
            )
        });

        // Fresh var for provisional return (supports recursive calls)
        let provisional_ret = self.fresh();
        let provisional_fn = self
            .ty_arena
            .func(param_tys.iter().copied().collect(), provisional_ret);
        self.env.bind(name, Scheme::mono(provisional_fn));

        self.env.push_scope();
        self.bind_params(params, &param_tys);
        self.ty_substs.push(type_param_subst.clone());

        // Register type param vars as polymorphic parameters (cannot be refined)
        name_to_tv.values().for_each(|&tv| {
            self.poly_param_vars.insert(tv);
        });

        // Infer body type
        let body_ty = self.expr(body);

        // Pop parameter scope
        self.ty_substs.pop();
        self.env.pop_scope();

        // Determine actual return type: use annotation if present, else body type
        let actual_ret = match declared_ret {
            Some((ret_ty, ret_span)) => {
                self.unify(body_ty, ret_ty, ret_span);
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
        self.interp.function_types.insert(body, fn_ty);
        let declared_tvs: HashSet<_> =
            name_to_tv.values().map(|&tv| self.uf.find(tv)).collect();
        let tv_names: HashMap<TyVar, StringId> = name_to_tv
            .iter()
            .map(|(&name, &tv)| (self.uf.find(tv), name))
            .collect();
        let policy = if type_params.is_empty() {
            SchemePolicy::InferredFun
        } else {
            SchemePolicy::ExplicitCallable {
                vars: &declared_tvs,
                names: &tv_names,
            }
        };
        let end = self.constraints.len();
        let out = self.qualified_scheme_in_env(
            fn_ty,
            constraint_start..end,
            policy,
            scheme_constraints,
            &outer_free,
        );
        self.constraints.truncate(constraint_start);
        self.constraints.extend(out.residual);
        self.env.bind(name, out.scheme);
        self.hoist.finalize_hoisted_fun(
            &mut HoistCtx {
                env: &mut self.env,
                uf: &mut self.uf,
                ty_arena: &mut self.ty_arena,
                constraints: &mut self.constraints,
                errors: &mut self.errors,
                current_module: &self.current_module,
            },
            stmt_id,
            name,
            declared_tvs,
            tv_names,
        );
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
    /// For simple variable bindings whose RHS has a function type, generalizes
    /// inferred variables and reachable class constraints into a scheme.
    pub(super) fn r#let(
        &mut self,
        stmt_id: StmtId,
        pattern: &BindingPattern,
        ann: Option<&AstTypeExprId>,
        rhs: ExprId,
        span: Span,
    ) {
        let constraint_start = self.constraints.len();
        self.push_let_tv_frame();

        // If annotation present, parse and unify.
        // Returns `None` if pattern was already bound (special case).
        let ty = match ann {
            None => Some(self.expr(rhs)),
            Some(id) => {
                let ann_ty = self.ast_ty(*id);
                let ann_span = self.ast.type_expr_span(*id).unwrap_or(span);
                self.interp.let_targets.insert(rhs, ann_ty);

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
                        self.infer_default_let(ann_ty, rhs, ann_span)
                    }
                } else {
                    self.infer_default_let(ann_ty, rhs, ann_span)
                }
            }
        };

        // Bind variables from the pattern (if not already done)
        if let Some(ty) = ty {
            match pattern {
                BindingPattern::Var(name) => {
                    let method_origin = self.method_ref_origin(rhs, ty);
                    if self.let_generalizes(rhs, ty) {
                        let end = self.constraints.len();
                        let out = self.qualified_scheme(
                            ty,
                            constraint_start..end,
                            SchemePolicy::InferredLet,
                            SmallVec::new(),
                        );
                        let has_open_residual =
                            self.has_open_residual(&out.scheme, &out.residual);
                        if has_open_residual {
                            self.env.bind(*name, Scheme::mono(ty));
                        } else {
                            self.constraints.truncate(constraint_start);
                            self.constraints.extend(out.residual);
                            self.env.bind(*name, out.scheme);
                            self.hoist.finalize_hoisted_fun(
                                &mut HoistCtx {
                                    env: &mut self.env,
                                    uf: &mut self.uf,
                                    ty_arena: &mut self.ty_arena,
                                    constraints: &mut self.constraints,
                                    errors: &mut self.errors,
                                    current_module: &self.current_module,
                                },
                                stmt_id,
                                *name,
                                HashSet::new(),
                                HashMap::new(),
                            );
                        }
                    } else {
                        self.env.bind(*name, Scheme::mono(ty));
                    }
                    if let Some(origin) = method_origin {
                        self.env.bind_method_ref_origin(*name, origin);
                    }
                }
                _ => {
                    if self.let_generalizes(rhs, ty) {
                        let end = self.constraints.len();
                        let out = self.qualified_scheme(
                            ty,
                            constraint_start..end,
                            SchemePolicy::InferredLet,
                            SmallVec::new(),
                        );
                        if self.has_open_residual(&out.scheme, &out.residual) {
                            self.bind_pattern(pattern, ty, span);
                        } else {
                            self.constraints.truncate(constraint_start);
                            self.constraints.extend(out.residual);
                            self.bind_pattern_scheme(
                                pattern,
                                &out.scheme,
                                Some(rhs),
                                span,
                            );
                        }
                    } else {
                        self.bind_pattern(pattern, ty, span);
                    }
                }
            }
        }

        self.pop_let_tv_frame();
    }

    fn let_generalizes(&self, rhs: ExprId, ty: TyId) -> bool {
        Self::type_contains_fn(ty, &self.ty_arena)
            || self.expr_has_empty_array(rhs)
    }

    fn expr_has_empty_array(&self, id: ExprId) -> bool {
        self.ast.get_expr(id).is_some_and(|expr| match expr {
            Expr::Array(elems) => {
                elems.is_empty()
                    || elems.iter().any(|e| match e {
                        ArrayElem::Elem(id) | ArrayElem::Spread(id) => {
                            self.expr_has_empty_array(*id)
                        }
                    })
            }
            Expr::Tuple(elems) => {
                elems.iter().any(|id| self.expr_has_empty_array(*id))
            }
            Expr::Object(entries) => entries.iter().any(|e| match e {
                ObjectEntry::Field(_, id) | ObjectEntry::Spread(id) => {
                    self.expr_has_empty_array(*id)
                }
            }),
            Expr::Annotate(inner, _) => self.expr_has_empty_array(*inner),
            _ => false,
        })
    }

    fn has_open_residual(
        &mut self,
        scheme: &Scheme,
        residual: &[(Constraint, Option<QualifiedName>)],
    ) -> bool {
        let vars: HashSet<_> = scheme.vars.iter().copied().collect();
        residual.iter().any(|(c, _)| match c {
            Constraint::Unify(a, b, _)
                if matches!(self.ty_arena.get(*a), Ty::Var(_))
                    && matches!(self.ty_arena.get(*b), Ty::Var(_)) =>
            {
                false
            }
            _ => c
                .free_vars(&self.ty_arena, &mut self.uf)
                .into_iter()
                .map(|v| self.uf.find(v))
                .any(|v| vars.contains(&v)),
        })
    }

    fn method_ref_origin(
        &mut self,
        rhs: ExprId,
        ty: TyId,
    ) -> Option<MethodRefOrigin> {
        self.ast.get_expr(rhs).cloned().and_then(|expr| match expr {
            Expr::ClassMethodRef(class, type_args, method) => {
                let class_arg = type_args.first().map(|ty| {
                    self.convert().ast_type_to_ty(*ty, &IndexMap::new())
                });
                self.env
                    .class_registry()
                    .lookup_by_name(class)
                    .map(|class| MethodRefOrigin {
                        class,
                        class_arg,
                        applied: 0,
                        method,
                    })
            }
            Expr::NakedClassMethodRef(method) => {
                let mut matches = self
                    .env
                    .class_registry()
                    .lookup_by_method(method)
                    .into_iter();
                match (matches.next(), matches.next()) {
                    (Some(class), None) => Some(MethodRefOrigin {
                        class,
                        class_arg: None,
                        applied: 0,
                        method,
                    }),
                    _ => None,
                }
            }
            Expr::Var(name) => self.env.lookup_method_ref_origin(name),
            Expr::Path(segments) => {
                segments.split_last().and_then(|(&member, mod_segments)| {
                    let mod_qn = QualifiedName::new(mod_segments.to_vec());
                    self.env
                        .lookup_user_module_member(&mod_qn, member)
                        .and_then(|member| member.method_origin)
                })
            }
            Expr::Field(base, member) => {
                self.ast.get_expr(base).and_then(|e| match e {
                    Expr::Var(module) => {
                        let mod_qn = QualifiedName::local(*module);
                        self.env
                            .lookup_user_module_member(&mod_qn, member)
                            .and_then(|member| member.method_origin)
                    }
                    _ => None,
                })
            }
            Expr::Call(callee, args) => {
                self.method_ref_origin(callee, ty).and_then(|origin| {
                    let applied = origin.applied + args.len();
                    self.stmt_method_ref_arity(origin)
                        .filter(|&arity| applied < arity)
                        .map(|_| MethodRefOrigin { applied, ..origin })
                })
            }
            _ => None,
        })
    }

    fn stmt_method_ref_arity(&self, origin: MethodRefOrigin) -> Option<usize> {
        self.env
            .class_def(origin.class)
            .method(origin.method, Span::default())
            .ok()
            .and_then(|spec| spec.scheme().arity(&self.ty_arena))
    }

    /// Default inference for `let` with type annotation.
    fn infer_default_let(
        &mut self,
        ann_ty: TyId,
        rhs: ExprId,
        ann_span: Span,
    ) -> Option<TyId> {
        let rhs_ty = self.expr(rhs);
        if !self.reject_private_repr_ann(rhs_ty, ann_ty, ann_span) {
            self.unify(rhs_ty, ann_ty, ann_span);
        }

        // Record for post-solve union narrowing check. At this point
        // `rhs_ty` may be a type variable (e.g. from a function call);
        // we defer the check until constraint solving resolves it.
        self.let_annotations.push((rhs, rhs_ty, ann_ty, ann_span));

        // Extensible records: if rhs is an object and annotation
        // is an alias to object, keep the full object type to
        // preserve extra fields
        let ann_shape = self.ty_arena.get(ann_ty).clone();
        let is_obj_alias = matches!(ann_shape, Ty::Named(id, _)
        if self.registry.get_def(id).is_some_and(|def| match def {
            TypeDef::Alias { .. } => self
                .ast
                .get_type_expr(self.decls.alias_target(id))
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

    pub(super) fn union_member_repr(
        &mut self,
        rhs: TyId,
        ann: TyId,
    ) -> Option<TyId> {
        match self.ty_arena.get(rhs) {
            Ty::Var(v) if self.numeric_vars.contains(v) => {
                self.expand_union_members(ann).and_then(|members| {
                    members.iter().copied().find(|&member| {
                        self.types_compatible(TyArena::INT, member)
                    })
                })
            }
            Ty::Var(_) => None,
            _ => self.expand_union_members(ann).and_then(|members| {
                members
                    .iter()
                    .copied()
                    .find(|&member| self.types_compatible(rhs, member))
            }),
        }
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

    fn bind_pattern_scheme(
        &mut self,
        pattern: &BindingPattern,
        scheme: &Scheme,
        rhs: Option<ExprId>,
        span: Span,
    ) {
        match pattern {
            BindingPattern::Var(name) => {
                self.env.bind(*name, scheme.clone());
                rhs.and_then(|rhs| self.method_ref_origin(rhs, scheme.ty))
                    .into_iter()
                    .for_each(|origin| {
                        self.env.bind_method_ref_origin(*name, origin);
                    });
            }

            BindingPattern::Wildcard => {}

            BindingPattern::Tuple(pats) => {
                let shape = self.ty_arena.get(scheme.ty).clone();
                match shape {
                    Ty::Tuple(ts) if ts.len() == pats.len() => {
                        pats.iter().zip(ts.iter()).enumerate().for_each(
                            |(i, (p, &ty))| {
                                let rhs = self.tuple_field_rhs(rhs, i);
                                let s = self.project_scheme(scheme, ty);
                                self.bind_pattern_scheme(p, &s, rhs, span);
                            },
                        );
                    }
                    Ty::Tuple(ts) => {
                        self.error(TypeError::ArityMismatch {
                            expected: pats.len(),
                            got: ts.len(),
                            span,
                        });
                        pats.iter().for_each(|p| {
                            self.bind_pattern_scheme(
                                p,
                                &Scheme::mono(TyArena::ERROR),
                                None,
                                span,
                            );
                        });
                    }
                    Ty::Var(_) | Ty::Error => {
                        self.bind_pattern(pattern, scheme.ty, span);
                    }
                    _ => {
                        self.error(TypeError::NotATuple(scheme.ty, span));
                        pats.iter().for_each(|p| {
                            self.bind_pattern_scheme(
                                p,
                                &Scheme::mono(TyArena::ERROR),
                                None,
                                span,
                            );
                        });
                    }
                }
            }

            BindingPattern::Object(fields) => {
                fields.iter().for_each(|(name, sub)| {
                    let n =
                        self.env.get_str(*name).unwrap_or_default().to_owned();
                    let ty = self.field_type(scheme.ty, &n, span);
                    let rhs = self.object_field_rhs(rhs, *name);
                    let s = self.project_scheme(scheme, ty);
                    self.bind_pattern_scheme(sub, &s, rhs, span);
                });
            }

            BindingPattern::Array(_, _) => {
                self.error(TypeError::ArrayPatternInLet(span));
            }
        }
    }

    fn tuple_field_rhs(
        &self,
        rhs: Option<ExprId>,
        idx: usize,
    ) -> Option<ExprId> {
        rhs.and_then(|rhs| {
            self.ast.get_expr(rhs).and_then(|expr| match expr {
                Expr::Tuple(elems) => elems.get(idx).copied(),
                _ => None,
            })
        })
    }

    fn object_field_rhs(
        &self,
        rhs: Option<ExprId>,
        field: StringId,
    ) -> Option<ExprId> {
        rhs.and_then(|rhs| {
            self.ast.get_expr(rhs).and_then(|expr| match expr {
                Expr::Object(entries) => {
                    entries.iter().find_map(|entry| match entry {
                        ObjectEntry::Field(name, id) if *name == field => {
                            Some(*id)
                        }
                        ObjectEntry::Field(_, _) | ObjectEntry::Spread(_) => {
                            None
                        }
                    })
                }
                _ => None,
            })
        })
    }

    fn project_scheme(&mut self, scheme: &Scheme, ty: TyId) -> Scheme {
        let pvars: HashSet<_> =
            scheme.vars.iter().map(|&v| self.uf.find(v)).collect();
        let mut vars: HashSet<_> = self
            .uf
            .free_vars(ty, &self.ty_arena)
            .into_iter()
            .map(|v| self.uf.find(v))
            .filter(|v| pvars.contains(v))
            .collect();
        let mut cs = SmallVec::new();

        (0..scheme.constraints.len()).for_each(|_| {
            scheme.constraints.iter().for_each(|(v, class)| {
                let v = self.uf.find(*v);
                let class =
                    class.resolve_inner(&mut self.uf, &mut self.ty_arena);
                let entry = (v, class.clone());
                if vars.contains(&v) && !cs.contains(&entry) {
                    class
                        .free_vars(&self.ty_arena, &mut self.uf)
                        .into_iter()
                        .map(|fv| self.uf.find(fv))
                        .filter(|fv| pvars.contains(fv))
                        .for_each(|fv| {
                            vars.insert(fv);
                        });
                    cs.push(entry);
                }
            });
        });

        let mut vars: SmallVec<[TyVar; 4]> = vars.into_iter().collect();
        vars.sort_unstable();
        Scheme {
            vars,
            ty: self.uf.resolve(ty, &mut self.ty_arena),
            constraints: cs,
        }
    }

    /// Validate a `set` operation with a resolved `RefTarget`.
    ///
    /// Global writes require transaction context.
    pub(super) fn set_validate(&mut self, rt: &RefTarget, span: Span) {
        let needs_txn = match rt {
            RefTarget::Inline(dbref) => matches!(dbref, DbRef::Global(..)),
            RefTarget::Expr(e) => self.ref_may_be_global(*e),
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
            RefTarget::Expr(e) => self.ref_may_be_global(*e),
        };
        if needs_txn && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }
    }

    /// Check if a ref expression could be a global; defaults to `true`
    /// (conservative) when the type is unknown or is the `Ref` union.
    fn ref_may_be_global(&self, e: ExprId) -> bool {
        self.get_type(e).is_none_or(|id| {
            matches!(
                self.ty_arena.get(id),
                Ty::Global | Ty::Union(Some(TypeId::REF), _)
            )
        })
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
                    class: TypeClass::param(ClassId::INTO, TyArena::STRING),
                    span,
                });
            }
            OutputFormat::Json => {
                // Must be convertible to Json
                self.constrain(Constraint::Class {
                    ty: expr_ty,
                    class: TypeClass::param(ClassId::INTO, TyArena::JSON),
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
                let union_ty = self.ty_arena.alloc(Ty::Union(
                    None,
                    smallvec![TyArena::FILEPATH, TyArena::STRING,],
                ));
                self.unify(path_ty, union_ty, span);
            }
        }
    }

    /// Infer types for a `class ... FOR ...` instance declaration.
    ///
    /// Validates:
    /// 1. The class name is a valid `ClassId`
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

        // 1. Resolve class name to `ClassId`
        let class = self
            .env
            .class_registry()
            .lookup_by_name(class_name)
            .unwrap_or_else(|| {
                let cn = self.env.resolve_str(class_name).to_owned();
                self.error(TypeError::UnknownClass(cn, span));
                ClassId::DISPLAY
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
        self.convert()
            .merge_for_type_vars(for_type, &mut type_param_subst);

        // 3. Resolve for_type and get its TypeId
        //    HKT classes need partial application logic (bare name or fewer
        //    type args than the type definition expects).
        let (for_ty, type_id, class_arg_tys) =
            match self.env.class_registry().shape(class) {
                ClassShape::Hkt { .. } => match self.resolve_hkt_for_type(
                    class,
                    for_type,
                    class_args,
                    &mut type_param_subst,
                    &module,
                    span,
                ) {
                    Some((tid, fty, catys)) => (fty, Some(tid), catys),
                    None => (TyArena::UNKNOWN, None, SmallVec::new()),
                },
                _ => {
                    let for_ty = self
                        .convert()
                        .ast_type_to_ty(for_type, &type_param_subst);
                    let type_id = self.extract_type_id(for_ty);
                    let class_arg_tys: SmallVec<[TyId; 2]> = class_args
                        .iter()
                        .map(|id| {
                            self.convert()
                                .ast_type_to_ty(*id, &type_param_subst)
                        })
                        .collect();
                    (for_ty, type_id, class_arg_tys)
                }
            };

        // Check for forbidden `newtype` representation overlap.
        let into_repr_overlap = if class == ClassId::INTO {
            class_arg_tys.first().copied().is_some_and(|to| {
                self.newtype_edge_overlaps_into(
                    for_ty,
                    to,
                    module.clone(),
                    span,
                )
                .is_some()
            })
        } else {
            false
        };
        if into_repr_overlap {
            let overlap = class_arg_tys.first().copied().and_then(|to| {
                self.newtype_edge_overlaps_into(
                    for_ty,
                    to,
                    module.clone(),
                    span,
                )
            });
            let span = self.repr_overlap_span(
                for_ty,
                for_type,
                &class_arg_tys,
                class_args,
                span,
            );
            match overlap {
                Some(NewtypeIntoOverlap::Public) => {
                    self.error(TypeError::PublicReprIntoOverlap { span });
                }
                Some(NewtypeIntoOverlap::Private) => {
                    self.error(TypeError::PrivateReprIntoExposure { span });
                }
                None => {}
            }
        }

        let bad_try = if class == ClassId::TRY_INTO {
            class_arg_tys.first().copied().is_some_and(|to| {
                self.private_try_into_external(for_ty, to, module.clone(), span)
            })
        } else {
            false
        };
        if bad_try {
            let span = self.repr_overlap_span(
                for_ty,
                for_type,
                &class_arg_tys,
                class_args,
                span,
            );
            self.error(TypeError::PrivateReprTryIntoExternal { span });
        }

        // Check for forbidden builtin instance.
        //
        // We allow implementing classes for builtin types IF the class is
        // user-defined OR if the class has type args that include user-defined
        // types. For example:
        //   - `class Display FOR Int` is forbidden (builtin has Display)
        //   - `class Into[String] FOR Int` is forbidden (builtin has Into[String])
        //   - `class Into[UserId] FOR Int` is ALLOWED (no builtin Into[UserId])
        //   - `class MyClass FOR Int` is ALLOWED (user-defined class)
        //
        // The heuristic: if the class is builtin AND for_type is builtin AND
        // all class args are builtin, reject. User-defined classes can always
        // be implemented for any type.
        if let Some(tid) = type_id {
            let is_builtin_class = class.idx() < ClassId::BUILTIN_COUNT;
            if is_builtin_class && self.is_builtin_type(tid) {
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

        // 5.5. Validate superclass instances exist
        if let Some(tid) = type_id {
            self.env
                .class_registry()
                .transitive_supers(class)
                .into_iter()
                .for_each(|sup| {
                    if self.instance_registry.lookup(sup, tid).is_none() {
                        self.error(TypeError::MissingSuperclassInstance {
                            class,
                            superclass: sup,
                            type_id: tid,
                            span,
                        });
                    }
                });
        }

        // 6. Process WHERE constraints
        let mut scheme_constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
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
                        self.convert()
                            .ast_class_to_ty_class(c, &type_param_subst),
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
                let ty = self
                    .convert()
                    .ast_type_to_ty(def.target, &type_param_subst);
                (def.name, ty)
            })
            .collect();

        // 6.6. Validate all required associated types are provided
        let req_assocs = self.env.class_def(class).assoc_types.clone();
        req_assocs.iter().for_each(|&req| {
            if !assoc_type_map.contains_key(&req) {
                self.error(TypeError::MissingAssocType {
                    class,
                    assoc: req,
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
        let required: Vec<StringId> =
            self.env.class_def(class).required_method_names().collect();
        let required_hint = required
            .iter()
            .map(|&s| self.env.resolve_str(s).to_owned())
            .join(", ");
        required.iter().for_each(|&req| {
            if !provided_methods.contains(&req) {
                self.error(TypeError::MissingInstanceMethod {
                    class,
                    method: self.env.resolve_str(req).to_owned(),
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
                assoc_types: &assoc_type_map,
                type_param_subst: &type_param_subst,
                method: m,
            });
        });

        // 9.5. Clear class context after method processing
        self.class_context = None;

        // 10. Register instance (if we have a valid type_id)
        let type_name = self.extract_type_name_from_ast(for_type);
        if let Some(tid) = type_id {
            let class_name_str = self
                .env
                .resolve_str(self.env.class_registry().name(class))
                .to_owned();
            let ca_names: Vec<String> = class_args
                .iter()
                .map(|id| self.extract_type_name_from_ast(*id))
                .collect();
            let mut method_map: HashMap<_, _> = methods
                .iter()
                .map(|m| {
                    let mn = self.env.resolve_str(m.name);
                    let fn_name = RuntimeInstance::fn_name_owned(
                        &class_name_str,
                        &type_name,
                        mn,
                        &ca_names,
                    );
                    let fn_name_id = self.env.intern(&fn_name);
                    (m.name, fn_name_id)
                })
                .collect();
            let all_methods: Vec<_> =
                self.env.class_def(class).method_names().collect();
            all_methods.iter().copied().for_each(|method| {
                if method_map.contains_key(&method)
                    || required.contains(&method)
                    || !self.has_default_method_body(class, method)
                {
                } else {
                    let mn = self.env.resolve_str(method).to_owned();
                    let fn_name =
                        RuntimeInstance::default_fn_name(&class_name_str, &mn);
                    let fn_name_id = self.env.intern(&fn_name);
                    method_map.insert(method, fn_name_id);
                }
            });

            let type_var_params: SmallVec<[TyId; 2]> =
                type_param_subst.values().copied().collect();

            // Skip registration if already hoisted (avoid duplicate error)
            if !into_repr_overlap
                && !bad_try
                && !self.instance_registry.has_with_args(
                    class,
                    tid,
                    &class_arg_tys,
                )
            {
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
                                    self.convert().ast_class_to_ty_class(
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

    fn repr_overlap_span(
        &self,
        for_ty: TyId,
        for_type: AstTypeExprId,
        args: &[TyId],
        arg_ids: &[AstTypeExprId],
        fallback: Span,
    ) -> Span {
        let for_alias = self
            .type_id_args(for_ty)
            .is_some_and(|(id, _)| self.decls.is_alias(id));
        if for_alias {
            self.ast.type_expr_span(for_type).unwrap_or(fallback)
        } else {
            args.first()
                .copied()
                .zip(arg_ids.first().copied())
                .and_then(|(ty, id)| {
                    self.type_id_args(ty)
                        .filter(|(tid, _)| self.decls.is_alias(*tid))
                        .and_then(|_| self.ast.type_expr_span(id))
                })
                .unwrap_or(fallback)
        }
    }

    fn instance_method(&mut self, input: InstanceMethodInput<'_>) {
        let InstanceMethodInput {
            class,
            for_ty,
            class_arg_tys,
            assoc_types,
            type_param_subst,
            method,
        } = input;

        self.check_method_body(MethodBodyInput {
            class,
            for_ty,
            class_arg_tys,
            assoc_types,
            type_param_subst,
            method,
        });
    }

    pub(super) fn class_default_method(
        &mut self,
        input: ClassDefaultMethodInput<'_>,
    ) {
        let ClassDefaultMethodInput {
            class,
            class_params,
            method,
        } = input;
        let shape = self.env.class_registry().shape(class);
        let self_tv = self.fresh_var();
        let for_ty = self.ty_arena.alloc(Ty::Var(self_tv));
        let class_arg_tys: SmallVec<[TyId; 2]> = match shape {
            ClassShape::Concrete { params }
            | ClassShape::Hkt { params, .. } => (0..params)
                .map(|_| {
                    let tv = self.fresh_var();
                    self.ty_arena.alloc(Ty::Var(tv))
                })
                .collect(),
        };
        let assoc_types: HashMap<_, _> = self
            .env
            .class_def(class)
            .assoc_types
            .iter()
            .copied()
            .map(|name| {
                let ty =
                    self.ty_arena.alloc(Ty::AssocType(self_tv, class, name));
                (name, ty)
            })
            .collect();
        let type_param_subst: IndexMap<_, _> = class_params
            .iter()
            .map(|tp| {
                let tv = self.fresh_var();
                (tp.name, self.ty_arena.alloc(Ty::Var(tv)))
            })
            .collect();

        let prev = self.class_context.replace(ClassContext {
            class,
            type_id: None,
            assoc_types: assoc_types.clone(),
        });
        self.check_method_body(MethodBodyInput {
            class,
            for_ty,
            class_arg_tys: &class_arg_tys,
            assoc_types: &assoc_types,
            type_param_subst: &type_param_subst,
            method,
        });
        self.class_context = prev;
    }

    fn check_method_body(&mut self, input: MethodBodyInput<'_>) {
        let MethodBodyInput {
            class,
            for_ty,
            class_arg_tys,
            assoc_types,
            type_param_subst,
            method,
        } = input;

        let m_span = method.span;

        // Get expected method signature from class
        let expected = self
            .env
            .class_def(class)
            .method(method.name, m_span)
            .cloned();
        let class_shape = self.env.class_registry().shape(class);
        let class_param_len = match class_shape {
            ClassShape::Concrete { params }
            | ClassShape::Hkt { params, .. } => params as usize,
        };

        // Handle unknown method error
        let (
            expected_param_tys,
            expected_ret_ty,
            expected_cs,
            expected_tps,
            self_var,
        ) = match expected {
            Ok(spec) => {
                let scheme = spec.scheme();
                if let Some(max) = scheme.vars.iter().map(|v| v.idx()).max() {
                    self.uf.reserve_through(max);
                }
                let shape = self.ty_arena.get(scheme.ty).clone();
                match shape {
                    Ty::Fn(params, ret) => match class_shape {
                        ClassShape::Hkt { .. } => {
                            // For HKT classes the LAST scheme var is the
                            // container constructor; all preceding vars are
                            // independent element type variables.
                            //
                            // Build a `Rename` that:
                            //  - maps each element var to a fresh type var
                            //  - maps the container var to the partially
                            //    applied `Named(type_id, supplied_args)` so
                            //    `Apply(container_var, elems)` resolves to
                            //    `Named(type_id, [supplied... elems...])`
                            if let Some((&container_var, elem_vars)) =
                                scheme.vars.split_last()
                            {
                                // First, rewrite `for_ty` so that any vars
                                // inside it that collide with scheme vars are
                                // replaced with fresh vars. This prevents a
                                // cycle in the rename (e.g. `for_ty` contains
                                // `Var(TyVar(2))` which is also the container
                                // var being mapped to `for_ty`).
                                let mut pre_rename = HashMap::new();
                                scheme.vars.iter().for_each(|&sv| {
                                    pre_rename.insert(sv, self.fresh());
                                });
                                let pre_rename = Rename(pre_rename);
                                let ctor_ty =
                                    self.ty_arena.apply(for_ty, &pre_rename);

                                let mut rename = HashMap::new();
                                rename.insert(container_var, ctor_ty);
                                elem_vars.iter().for_each(|&ev| {
                                    rename.insert(ev, self.fresh());
                                });
                                let rename = Rename(rename);

                                let ps: Vec<_> = params
                                    .iter()
                                    .map(|&p| self.ty_arena.apply(p, &rename))
                                    .collect();
                                let r = self.ty_arena.apply(ret, &rename);
                                let cs = self.expected_method_constraints(
                                    scheme,
                                    Some(container_var),
                                    &rename,
                                );
                                let tps = self.expected_method_type_params(
                                    scheme,
                                    class_shape,
                                    class_param_len,
                                    &rename,
                                );
                                (ps, r, cs, tps, Some(container_var))
                            } else {
                                self.error(TypeError::Custom {
                                    msg:
                                        "HKT class scheme has no type variables"
                                            .into(),
                                    span: m_span,
                                });
                                (
                                    vec![],
                                    TyArena::UNKNOWN,
                                    SmallVec::new(),
                                    SmallVec::new(),
                                    None,
                                )
                            }
                        }
                        _ => {
                            // Simple/Parameterized: first var is `Self`,
                            // subsequent vars are class type params.
                            let self_var = scheme.vars.first().copied();
                            let class_arg_vars: Vec<_> =
                                scheme.vars.iter().skip(1).copied().collect();
                            let mut map = HashMap::new();
                            if let Some(var) = self_var {
                                map.insert(var, for_ty);
                            }
                            class_arg_vars
                                .iter()
                                .zip(class_arg_tys.iter())
                                .for_each(|(&var, &arg_ty)| {
                                    map.insert(var, arg_ty);
                                });
                            scheme
                                .vars
                                .iter()
                                .skip(1 + class_arg_tys.len())
                                .for_each(|&var| {
                                    map.insert(var, self.fresh());
                                });
                            let rename = Rename(map);
                            let ps: Vec<_> = params
                                .iter()
                                .map(|&p| self.ty_arena.apply(p, &rename))
                                .collect();
                            let r = self.ty_arena.apply(ret, &rename);
                            let cs = self.expected_method_constraints(
                                scheme, self_var, &rename,
                            );
                            let tps = self.expected_method_type_params(
                                scheme,
                                class_shape,
                                class_param_len,
                                &rename,
                            );
                            (ps, r, cs, tps, self_var)
                        }
                    },
                    _ => (
                        vec![],
                        TyArena::UNKNOWN,
                        SmallVec::new(),
                        SmallVec::new(),
                        None,
                    ),
                }
            }
            Err(e) => {
                self.error(e);
                (
                    vec![],
                    TyArena::UNKNOWN,
                    SmallVec::new(),
                    SmallVec::new(),
                    None,
                )
            }
        };
        let expected_param_tys: Vec<_> = expected_param_tys
            .into_iter()
            .map(|ty| {
                self.normalize_instance_assoc_ty(
                    ty,
                    self_var,
                    class,
                    assoc_types,
                )
            })
            .collect();
        let expected_ret_ty = self.normalize_instance_assoc_ty(
            expected_ret_ty,
            self_var,
            class,
            assoc_types,
        );
        let expected_cs: SmallVec<[_; 2]> = expected_cs
            .into_iter()
            .map(|(ty, cls)| {
                (
                    self.normalize_instance_assoc_ty(
                        ty,
                        self_var,
                        class,
                        assoc_types,
                    ),
                    cls.map(|ty| {
                        self.normalize_instance_assoc_ty(
                            ty,
                            self_var,
                            class,
                            assoc_types,
                        )
                    }),
                )
            })
            .collect();
        let expected_tps: SmallVec<[_; 2]> = expected_tps
            .into_iter()
            .map(|ty| {
                self.normalize_instance_assoc_ty(
                    ty,
                    self_var,
                    class,
                    assoc_types,
                )
            })
            .collect();

        // Check arity
        if method.params.len() != expected_param_tys.len() {
            self.error(TypeError::MethodSignatureMismatch {
                class,
                method: self.env.resolve_str(method.name).to_owned(),
                expected: expected_param_tys.len(),
                got: method.params.len(),
                span: m_span,
            });
        }

        if !method.type_params.is_empty()
            && method.type_params.len() != expected_tps.len()
        {
            let cn = self
                .env
                .resolve_str(self.env.class_registry().name(class))
                .to_owned();
            let mn = self.env.resolve_str(method.name).to_owned();
            self.error(TypeError::Custom {
                msg: format!(
                    "method `{}` of class `{}` has {} type parameters but the class method signature has {}",
                    mn,
                    cn,
                    method.type_params.len(),
                    expected_tps.len(),
                ),
                span: m_span,
            });
        }

        let tp_tvs: HashMap<StringId, TyVar> = method
            .type_params
            .iter()
            .map(|tp| (tp.name, self.fresh_var()))
            .collect();
        let mut method_subst = type_param_subst.clone();
        method.type_params.iter().for_each(|tp| {
            let tv = tp_tvs[&tp.name];
            method_subst.insert(tp.name, self.ty_arena.alloc(Ty::Var(tv)));
        });

        if matches!(
            self.env.class_registry().shape(class),
            ClassShape::Hkt { .. }
        ) {
            method.params.iter().for_each(|(_, ann)| {
                if let Some(id) = ann {
                    self.convert().merge_for_type_vars(*id, &mut method_subst);
                }
            });
            if let Some(ret) = method.ret {
                self.convert().merge_for_type_vars(ret, &mut method_subst);
            }
        }

        let type_param_subst = &method_subst;

        // Typecheck method body
        self.env.push_scope();
        self.ty_substs.push((*type_param_subst).clone());
        tp_tvs.values().for_each(|&tv| {
            self.poly_param_vars.insert(tv);
        });

        // Bind parameters with user-provided types (or inferred).
        // For unannotated params, use expected types directly so that
        // the body can rely on concrete type info (e.g. for match
        // exhaustiveness) without waiting for deferred constraint solving.
        let method_vars: HashSet<_> = if method.type_params.is_empty() {
            method_subst
                .values()
                .flat_map(|&ty| self.uf.free_vars(ty, &self.ty_arena))
                .collect()
        } else {
            tp_tvs.values().copied().collect()
        };
        let mut impl_map = Self::method_type_param_map(
            &method.type_params,
            &tp_tvs,
            &expected_tps,
        );
        let mut sig_bad = false;
        let param_tys: Vec<TyId> = method
            .params
            .iter()
            .zip(expected_param_tys.iter())
            .map(|((_, ann), &exp_ty)| match ann {
                Some(id) => {
                    let user_ty =
                        self.convert().ast_type_to_ty(*id, type_param_subst);
                    self.match_method_ty_vars(
                        user_ty,
                        exp_ty,
                        &method_vars,
                        &mut impl_map,
                        &mut sig_bad,
                    );
                    self.unify(user_ty, exp_ty, m_span);
                    user_ty
                }
                None => exp_ty,
            })
            .collect();

        if let Some(ret_id) = method.ret {
            let user_ret =
                self.convert().ast_type_to_ty(ret_id, type_param_subst);
            self.match_method_ty_vars(
                user_ret,
                expected_ret_ty,
                &method_vars,
                &mut impl_map,
                &mut sig_bad,
            );
        }
        if sig_bad {
            self.method_signature_mismatch(class, method.name, m_span);
        }

        let body_map = if method.type_params.is_empty() {
            self.method_type_param_body_map(&expected_tps, &impl_map)
        } else {
            impl_map.clone()
        };
        body_map.keys().for_each(|&tv| {
            self.poly_param_vars.insert(tv);
        });
        let (got_cs, cs_bad) = if method.type_params.is_empty() {
            (expected_cs.clone(), false)
        } else {
            let got_cs =
                self.method_constraints(&method.type_params, type_param_subst);
            let got_cs =
                self.apply_method_constraint_rename(got_cs, &Rename(impl_map));
            let cs_bad = self.check_method_constraints_match(
                class,
                method.name,
                &expected_cs,
                &got_cs,
                m_span,
            );
            (got_cs, cs_bad)
        };
        got_cs.iter().for_each(|&(ty, ref cls)| {
            self.constrain(Constraint::Class {
                ty,
                class: cls.clone(),
                span: m_span,
            });
        });

        self.bind_params(&method.params, &param_tys);

        let body_constraint_start = self.constraints.len();

        // Infer body type
        let body_ty = self.expr(method.body);
        if cs_bad {
        } else {
            self.check_method_body_constraints_match(
                class,
                method.name,
                &expected_cs,
                &body_map,
                body_constraint_start,
                m_span,
            );
        }

        // Determine expected return type (user annotation or class signature)
        let ret = method.ret.map(|ret_id| {
            (
                self.convert().ast_type_to_ty(ret_id, type_param_subst),
                self.ast.type_expr_span(ret_id).unwrap_or(m_span),
            )
        });
        let ret_ty = ret.map(|(ty, _)| ty).unwrap_or(expected_ret_ty);
        let ret_span = ret.map(|(_, span)| span).unwrap_or(m_span);

        // Unify body with return type
        self.unify(body_ty, ret_ty, ret_span);

        // Also unify with class's expected return type (catches wrong annotation)
        if expected_ret_ty != TyArena::UNKNOWN {
            self.unify(ret_ty, expected_ret_ty, ret_span);
        }

        let fn_ty = self
            .ty_arena
            .func(param_tys.iter().copied().collect(), ret_ty);
        self.interp.function_types.insert(method.body, fn_ty);

        self.ty_substs.pop();
        self.env.pop_scope();
    }

    /// Class method schemes can mention `Self:Class:Assoc`. While checking a
    /// concrete instance, that projection is equivalent to the instance
    /// associated type RHS after applying the instance type parameter
    /// substitution.
    fn normalize_instance_assoc_ty(
        &mut self,
        ty: TyId,
        self_var: Option<TyVar>,
        class: ClassId,
        assoc_types: &HashMap<StringId, TyId>,
    ) -> TyId {
        let ty_shape = self.ty_arena.get(ty).clone();
        match ty_shape {
            Ty::AssocType(tv, assoc_class, name)
                if Some(tv) == self_var && assoc_class == class =>
            {
                assoc_types.get(&name).copied().unwrap_or(ty)
            }
            Ty::Array(elem) => {
                let elem = self.normalize_instance_assoc_ty(
                    elem,
                    self_var,
                    class,
                    assoc_types,
                );
                self.ty_arena.array(elem)
            }
            Ty::Option(inner) => {
                let inner = self.normalize_instance_assoc_ty(
                    inner,
                    self_var,
                    class,
                    assoc_types,
                );
                self.ty_arena.option(inner)
            }
            Ty::Result(ok, err) => {
                let ok = self.normalize_instance_assoc_ty(
                    ok,
                    self_var,
                    class,
                    assoc_types,
                );
                let err = self.normalize_instance_assoc_ty(
                    err,
                    self_var,
                    class,
                    assoc_types,
                );
                self.ty_arena.result(ok, err)
            }
            Ty::Map(k, v) => {
                let k = self.normalize_instance_assoc_ty(
                    k,
                    self_var,
                    class,
                    assoc_types,
                );
                let v = self.normalize_instance_assoc_ty(
                    v,
                    self_var,
                    class,
                    assoc_types,
                );
                self.ty_arena.map_ty(k, v)
            }
            Ty::Tuple(elems) => {
                let elems = elems
                    .into_iter()
                    .map(|elem| {
                        self.normalize_instance_assoc_ty(
                            elem,
                            self_var,
                            class,
                            assoc_types,
                        )
                    })
                    .collect();
                self.ty_arena.alloc(Ty::Tuple(elems))
            }
            Ty::Fn(params, ret) => {
                let params = params
                    .into_iter()
                    .map(|param| {
                        self.normalize_instance_assoc_ty(
                            param,
                            self_var,
                            class,
                            assoc_types,
                        )
                    })
                    .collect();
                let ret = self.normalize_instance_assoc_ty(
                    ret,
                    self_var,
                    class,
                    assoc_types,
                );
                self.ty_arena.func(params, ret)
            }
            Ty::Object(fields) => {
                let fields = fields
                    .into_iter()
                    .map(|(name, field)| {
                        (
                            name,
                            self.normalize_instance_assoc_ty(
                                field,
                                self_var,
                                class,
                                assoc_types,
                            ),
                        )
                    })
                    .collect();
                self.ty_arena.alloc(Ty::Object(fields))
            }
            Ty::Union(prov, members) => {
                let members = members
                    .into_iter()
                    .map(|member| {
                        self.normalize_instance_assoc_ty(
                            member,
                            self_var,
                            class,
                            assoc_types,
                        )
                    })
                    .collect();
                self.ty_arena.alloc(Ty::Union(prov, members))
            }
            Ty::Named(id, args) => {
                let args = args
                    .into_iter()
                    .map(|arg| {
                        self.normalize_instance_assoc_ty(
                            arg,
                            self_var,
                            class,
                            assoc_types,
                        )
                    })
                    .collect();
                self.ty_arena.named(id, args)
            }
            Ty::Apply(tv, args) => {
                let args = args
                    .into_iter()
                    .map(|arg| {
                        self.normalize_instance_assoc_ty(
                            arg,
                            self_var,
                            class,
                            assoc_types,
                        )
                    })
                    .collect();
                self.ty_arena.hkt(tv, args)
            }
            Ty::AssocType(..)
            | Ty::Var(_)
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
            | Ty::Unknown
            | Ty::Error => ty,
        }
    }

    fn match_method_ty_vars(
        &mut self,
        got: TyId,
        expected: TyId,
        vars: &HashSet<TyVar>,
        map: &mut HashMap<TyVar, TyId>,
        bad: &mut bool,
    ) {
        let got_ty = self.ty_arena.get(got).clone();
        let exp_ty = self.ty_arena.get(expected).clone();
        match (got_ty, exp_ty) {
            (Ty::Var(v), _) if vars.contains(&v) => {
                Self::match_method_ty_var(v, expected, map, bad);
            }
            (Ty::Array(a), Ty::Array(b)) | (Ty::Option(a), Ty::Option(b)) => {
                self.match_method_ty_vars(a, b, vars, map, bad);
            }
            (Ty::Result(a1, b1), Ty::Result(a2, b2))
            | (Ty::Map(a1, b1), Ty::Map(a2, b2)) => {
                self.match_method_ty_vars(a1, a2, vars, map, bad);
                self.match_method_ty_vars(b1, b2, vars, map, bad);
            }
            (Ty::Tuple(a), Ty::Tuple(b))
            | (Ty::Named(_, a), Ty::Named(_, b)) => {
                a.iter().zip(b.iter()).for_each(|(&x, &y)| {
                    self.match_method_ty_vars(x, y, vars, map, bad);
                });
            }
            (Ty::Fn(a_ps, a_ret), Ty::Fn(b_ps, b_ret)) => {
                a_ps.iter().zip(b_ps.iter()).for_each(|(&x, &y)| {
                    self.match_method_ty_vars(x, y, vars, map, bad);
                });
                self.match_method_ty_vars(a_ret, b_ret, vars, map, bad);
            }
            (Ty::Object(a), Ty::Object(b)) => {
                a.iter().for_each(|(&name, &x)| {
                    if let Some(&y) = b.get(&name) {
                        self.match_method_ty_vars(x, y, vars, map, bad);
                    }
                });
            }
            (Ty::Union(_, a), Ty::Union(_, b)) => {
                a.iter().zip(b.iter()).for_each(|(&x, &y)| {
                    self.match_method_ty_vars(x, y, vars, map, bad);
                });
            }
            (Ty::Apply(v, a), Ty::Apply(w, b)) if vars.contains(&v) => {
                let ty = self.ty_arena.alloc(Ty::Var(w));
                Self::match_method_ty_var(v, ty, map, bad);
                a.iter().zip(b.iter()).for_each(|(&x, &y)| {
                    self.match_method_ty_vars(x, y, vars, map, bad);
                });
            }
            (Ty::Apply(_, a), Ty::Apply(_, b)) => {
                a.iter().zip(b.iter()).for_each(|(&x, &y)| {
                    self.match_method_ty_vars(x, y, vars, map, bad);
                });
            }
            _ => {}
        }
    }

    fn match_method_ty_var(
        var: TyVar,
        ty: TyId,
        map: &mut HashMap<TyVar, TyId>,
        bad: &mut bool,
    ) {
        match map.get(&var).copied() {
            Some(prev) if prev == ty => {}
            Some(_) => {
                *bad = true;
            }
            None => {
                map.insert(var, ty);
            }
        }
    }

    fn method_type_param_map(
        tps: &[TypeParam],
        tvs: &HashMap<StringId, TyVar>,
        expected: &[TyId],
    ) -> HashMap<TyVar, TyId> {
        tps.iter()
            .zip(expected.iter())
            .filter_map(|(tp, &ty)| {
                tvs.get(&tp.name).copied().map(|tv| (tv, ty))
            })
            .collect()
    }

    fn method_type_param_self_map(
        &mut self,
        tps: &[TyId],
    ) -> HashMap<TyVar, TyId> {
        let tvs: HashSet<_> = tps
            .iter()
            .flat_map(|&ty| self.uf.free_vars(ty, &self.ty_arena))
            .collect();
        tvs.into_iter()
            .map(|tv| (tv, self.ty_arena.alloc(Ty::Var(tv))))
            .collect()
    }

    fn method_type_param_body_map(
        &mut self,
        tps: &[TyId],
        map: &HashMap<TyVar, TyId>,
    ) -> HashMap<TyVar, TyId> {
        let self_map = self.method_type_param_self_map(tps);
        if map.is_empty() {
            self_map
        } else {
            let tps: HashSet<_> = self_map.keys().copied().collect();
            map.iter()
                .filter_map(|(&tv, &ty)| {
                    let vars = self.uf.free_vars(ty, &self.ty_arena);
                    if vars.iter().any(|v| tps.contains(v)) {
                        Some((tv, ty))
                    } else {
                        None
                    }
                })
                .collect()
        }
    }

    fn apply_method_constraint_rename(
        &mut self,
        cs: SmallVec<[(TyId, TypeClass<TyId>); 2]>,
        rename: &Rename,
    ) -> SmallVec<[(TyId, TypeClass<TyId>); 2]> {
        cs.into_iter()
            .map(|(ty, cls)| {
                (
                    self.ty_arena.apply(ty, rename),
                    cls.apply(rename, &mut self.ty_arena),
                )
            })
            .collect()
    }

    fn expected_method_type_params(
        &mut self,
        scheme: &Scheme,
        shape: ClassShape,
        class_arg_len: usize,
        rename: &Rename,
    ) -> SmallVec<[TyId; 2]> {
        let n = scheme.vars.len().saturating_sub(class_arg_len + 1);
        let start = if matches!(shape, ClassShape::Hkt { .. }) {
            0
        } else {
            class_arg_len + 1
        };
        scheme
            .vars
            .iter()
            .skip(start)
            .take(n)
            .map(|v| {
                rename
                    .0
                    .get(v)
                    .copied()
                    .unwrap_or_else(|| self.ty_arena.alloc(Ty::Var(*v)))
            })
            .collect()
    }

    fn expected_method_constraints(
        &mut self,
        scheme: &Scheme,
        self_var: Option<TyVar>,
        rename: &Rename,
    ) -> SmallVec<[(TyId, TypeClass<TyId>); 2]> {
        scheme
            .constraints
            .iter()
            .filter(|(tv, _)| Some(*tv) != self_var)
            .map(|(tv, cls)| {
                let ty = rename
                    .0
                    .get(tv)
                    .copied()
                    .unwrap_or_else(|| self.ty_arena.alloc(Ty::Var(*tv)));
                (ty, cls.apply(rename, &mut self.ty_arena))
            })
            .collect()
    }

    fn method_constraints(
        &mut self,
        tps: &[TypeParam],
        subst: &IndexMap<StringId, TyId>,
    ) -> SmallVec<[(TyId, TypeClass<TyId>); 2]> {
        tps.iter().fold(SmallVec::new(), |mut acc, tp| {
            if let Some(ty) = subst.get(&tp.name).copied() {
                tp.constraints.iter().for_each(|c| {
                    let cls = self.convert().ast_class_to_ty_class(c, subst);
                    acc.push((ty, cls.clone()));
                    self.env
                        .class_registry()
                        .transitive_supers(cls.tag())
                        .into_iter()
                        .filter_map(|sup| cls.with_tag(sup).map(|sc| (ty, sc)))
                        .for_each(|pair| {
                            acc.push(pair);
                        });
                });
            }
            acc
        })
    }

    fn check_method_body_constraints_match(
        &mut self,
        class: ClassId,
        method: StringId,
        expected: &[(TyId, TypeClass<TyId>)],
        map: &HashMap<TyVar, TyId>,
        start: usize,
        span: Span,
    ) {
        if map.is_empty() {
        } else {
            let end = self.constraints.len();
            let snap = self.uf.snapshot();
            ConstraintRegion::build_unions(
                &self.constraints,
                start..end,
                &mut self.uf,
                &self.ty_arena,
            );
            let rename = Rename(map.clone());
            let got = ConstraintRegion::reachable_constraint_pairs(
                &self.constraints,
                start..end,
                map,
                &rename,
                &mut self.uf,
                &mut self.ty_arena,
            );
            let want = self.constraint_keys(expected);
            let got = self.constraint_keys(&got);
            self.uf.rollback(snap);
            if got.iter().all(|key| want.contains(key)) {
            } else {
                self.method_constraints_mismatch(class, method, span);
            }
        }
    }

    fn check_method_constraints_match(
        &mut self,
        class: ClassId,
        method: StringId,
        expected: &[(TyId, TypeClass<TyId>)],
        got: &[(TyId, TypeClass<TyId>)],
        span: Span,
    ) -> bool {
        let want = self.constraint_keys(expected);
        let got = self.constraint_keys(got);
        if want == got {
            false
        } else {
            self.method_constraints_mismatch(class, method, span);
            true
        }
    }

    fn method_signature_mismatch(
        &mut self,
        class: ClassId,
        method: StringId,
        span: Span,
    ) {
        let cn = self
            .env
            .resolve_str(self.env.class_registry().name(class))
            .to_owned();
        let mn = self.env.resolve_str(method).to_owned();
        self.error(TypeError::Custom {
            msg: format!(
                "method `{}` of class `{}` has signature that does not match the class method signature",
                mn, cn,
            ),
            span,
        });
    }

    fn method_constraints_mismatch(
        &mut self,
        class: ClassId,
        method: StringId,
        span: Span,
    ) {
        let cn = self
            .env
            .resolve_str(self.env.class_registry().name(class))
            .to_owned();
        let mn = self.env.resolve_str(method).to_owned();
        self.error(TypeError::Custom {
            msg: format!(
                "method `{}` of class `{}` has constraints that do not match the class method signature",
                mn, cn,
            ),
            span,
        });
    }

    fn constraint_keys(
        &mut self,
        cs: &[(TyId, TypeClass<TyId>)],
    ) -> Vec<ConstraintKey> {
        ConstraintRegion::keys(cs, &mut self.uf, &mut self.ty_arena)
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
                AstTypeExpr::TupleConstructor { arity, .. } => {
                    Some(format!("Tuple{arity}"))
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

    /// Resolve a `TyId` to its nominal `TypeId`; handles `Named`, `Union`,
    /// and primitive types.
    pub(super) fn nominal_type_id(&self, ty: TyId) -> Option<TypeId> {
        match self.ty_arena.get(ty) {
            Ty::Named(tid, _) | Ty::Union(Some(tid), _) => Some(*tid),
            other => self.primitive_type_id(other),
        }
    }

    /// Get the `TypeId` for a primitive `Ty`.
    pub(super) fn primitive_type_id(&self, ty: &Ty) -> Option<TypeId> {
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
            Ty::Array(_) => Some(TypeId::ARRAY),
            Ty::Option(_) => Some(TypeId::OPTION),
            Ty::Result(_, _) => Some(TypeId::RESULT),
            Ty::Map(_, _) => Some(TypeId::MAP),
            Ty::Tuple(_) => Some(TypeId::TUPLE),
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
    /// Delegates to `TyArena::apply` with a singleton rename.
    fn subst_tyvar(&mut self, ty: TyId, var: TyVar, replacement: TyId) -> TyId {
        let rename = Rename::singleton(var, replacement);
        self.ty_arena.apply(ty, &rename)
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

    /// Validate that all type expressions in a `variant` declaration body are
    /// fully saturated (no unsaturated type synonyms like bare `Array`).
    fn validate_type_decl_body(&mut self, tps: &[TypeParam], def: &TypeDefAst) {
        let subst = self.type_param_subst(tps);
        match def {
            TypeDefAst::Sum(variants) => {
                variants.iter().for_each(|v| {
                    v.payloads.iter().for_each(|p| {
                        let ty = self.convert().ast_type_to_ty(*p, &subst);
                        let span =
                            self.ast.type_expr_span(*p).unwrap_or_default();
                        self.require_wf_ty(ty, span);
                    });
                });
            }
        }
    }
}
