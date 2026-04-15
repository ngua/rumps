//! Declaration hoisting for forward references.
//!
//! Implements Pass 1 of the two-pass type inference: traverse statements and
//! register function/module names with provisional types before any body
//! inference. This enables forward references and mutual recursion.

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::{ClassInstanceInput, InferCtx};
use crate::ast::{
    AstTypeExpr, AstTypeExprId, BindingPattern, Expr, Import, Stmt, StmtId,
    TypeParam,
};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::Instance;
use crate::typecheck::ty::{
    ClassShape, Scheme, Ty, TyArena, TyId, TyVar, TypeClass,
};
use crate::value::{TypeDef, TypeId};
use crate::{ClassId, Span};

impl InferCtx<'_> {
    /// Pass 1: Register all function/module declarations with provisional types.
    ///
    /// This enables forward references: functions can call other functions
    /// defined later in the same scope, and modules can be referenced before
    /// their definition.
    ///
    /// Hoisting is done in three phases to ensure imported types are available
    /// when function signatures are processed:
    ///
    /// 1. **Phase 1**: Hoist modules only (registers module types like `M.T`)
    /// 2. **Phase 2**: Process imports (populates `imported_types` mapping)
    /// 3. **Phase 3**: Hoist functions and class instances
    pub(crate) fn hoist_declarations(&mut self, stmts: &[StmtId]) {
        // Phase 1: Hoist modules only (registers module types)
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Module { ref name, ref body }) = stmt {
                let span = self.ast.stmt_span(id).unwrap_or_default();
                self.hoist_module(QualifiedName::local(*name), body, span);
            }
        });

        // Phase 2: Process imports (populates imported_types).
        // Auto-import the `Prelude` module first so its members are always
        // in scope, then process user imports (which may shadow them).
        {
            let pid = self.env.intern(crate::env::PRELUDE_MODULE);
            self.import_stmt(&Import::wildcard(pid), Span::default());
        }
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Import(ref import)) = stmt {
                let span = self.ast.stmt_span(id).unwrap_or_default();
                self.import_stmt(import, span);
            }
        });

        // Phase 3: Hoist functions and class instances
        stmts.iter().for_each(|&id| self.hoist_non_module(id));
    }

    /// Hoist top-level `let` bindings.
    ///
    /// Only simple bindings (`BindingPattern::Var(name)`) are hoisted;
    /// destructuring patterns fall through and are bound normally in Pass 2.
    /// Intended to be called ONLY at script top level, after
    /// `hoist_declarations`, and ONLY in non-interactive mode.
    ///
    /// Two cases:
    ///
    /// 1. Closure RHS: hoisted via `hoist_fun` to obtain full parity with
    ///    `fun` hoisting (polymorphic schemes, fresh instantiation per
    ///    forward-reference call site, finalization tracking).
    ///
    /// 2. Non-closure RHS: hoisted with a provisional fresh `TyVar` and
    ///    tracked in `hoisted_lets` for the Pass 2 unify step.
    ///
    /// Collisions with previously bound names from top-level `fun` or an
    /// earlier `let` of the same name emit a duplicate-binding error.
    /// Collisions with imported names are allowed (shadowing imports is
    /// intentional).
    pub(crate) fn hoist_toplevel_lets(&mut self, stmts: &[StmtId]) {
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            let span = self.ast.stmt_span(id).unwrap_or_default();
            if let Some(Stmt::Let(BindingPattern::Var(name), ann, rhs, _)) = stmt {
                // Only reject collisions with hoisted funs and
                // earlier hoisted lets; allow shadowing imports.
                // Linear scan is fine; `hoisted_funs` is small (one per
                // top-level `FUN`) and this runs once per top-level `LET`.
                let is_hoisted_fun = self.env.lookup(name).is_some_and(|env_s| {
                    self.hoist.funs.values().any(|s| env_s == s)
                });
                let is_hoisted_let = self.hoist.lets.contains_key(&name);
                if is_hoisted_fun || is_hoisted_let {
                    let n = self.env.resolve_string(name);
                    self.error(TypeError::Custom {
                        msg: format!(
                            "duplicate top-level binding `{}`; \
                             a function or earlier `let` already binds this name",
                            n
                        ),
                        span,
                    });
                } else {
                    let closure = self
                        .ast
                        .get_expr(rhs)
                        .cloned()
                        .and_then(|e| match e {
                            Expr::Closure { type_params, params, ret, .. } => {
                                Some((type_params, params, ret))
                            }
                            _ => None,
                        });
                    match closure {
                        Some((type_params, params, ret)) => {
                            // Closure-RHS lets: parity with `fun`.
                            // `hoist_fun` populates `hoisted_funs` so Phase 4
                            // forward-ref tracking applies uniformly.
                            self.hoist_fun(
                                id,
                                name,
                                &type_params,
                                &params,
                                ret.as_ref(),
                                span,
                            );
                        }
                        None => {
                            let ty = match ann {
                                Some(aid) => {
                                    self.ast_type_to_ty(aid, &IndexMap::new())
                                }
                                None => self.fresh(),
                            };
                            self.env.bind(name, Scheme::mono(ty));
                            self.hoist.lets.insert(name, ty);
                        }
                    }
                }
            }
        });
    }

    /// Hoist non-module declarations (functions and class instances).
    ///
    /// Called in Phase 3 after modules have been hoisted and imports processed.
    fn hoist_non_module(&mut self, id: StmtId) {
        let span = self.ast.stmt_span(id).unwrap_or_default();
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::Fun {
                name,
                type_params,
                params,
                ret,
                ..
            }) => self.hoist_fun(
                id,
                name,
                &type_params,
                &params,
                ret.as_ref(),
                span,
            ),

            Some(Stmt::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types: _,
                methods,
            }) => self.hoist_class_instance(ClassInstanceInput {
                class_name,
                class_args: &class_args,
                type_params: &type_params,
                for_type,
                constraints: &constraints,
                methods: &methods,
                assoc_types: (),
                module: None,
                span,
            }),

            // Modules already hoisted in Phase 1; imports processed in Phase 2;
            // other statements don't need hoisting
            _ => {}
        }
    }

    /// Hoist a function declaration with a polymorphic type scheme.
    ///
    /// Creates fresh type variables for type parameters and unannotated
    /// params/returns, then generalizes over all free type variables not
    /// bound in the outer environment. Constraints are collected and stored
    /// in the scheme.
    fn hoist_fun(
        &mut self,
        stmt_id: StmtId,
        name: StringId,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(StringId, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        _span: Span,
    ) {
        // Create fresh type variables for all type parameters
        let type_param_vars: Vec<_> = type_params
            .iter()
            .map(|tp| {
                let tv = self.fresh_var();
                (tp, tv)
            })
            .collect();

        let type_param_subst: IndexMap<_, _> = type_param_vars
            .iter()
            .map(|(tp, tv)| {
                let id = tp.name;
                let ty_id = self.ty_arena.alloc(Ty::Var(*tv));
                (id, ty_id)
            })
            .collect();

        // Process type parameter constraints
        let mut scheme_constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
            SmallVec::new();
        type_param_vars.iter().for_each(|(tp, tv)| {
            tp.constraints.iter().for_each(|c| {
                let class = self.ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((*tv, class));
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys: SmallVec<[TyId; 4]> = params
            .iter()
            .map(|(_, ann)| match ann {
                Some(id) => self.ast_type_to_ty(*id, &type_param_subst),
                None => self.fresh(),
            })
            .collect();

        // Return type: use annotation if present, else fresh var
        let ret_ty = match ret {
            Some(id) => self.ast_type_to_ty(*id, &type_param_subst),
            None => self.fresh(),
        };

        // Build function type
        let fn_ty = self.ty_arena.func(param_tys, ret_ty);

        // Generalize over ALL free type variables in both the function type
        // and the constraints. This includes:
        // - Explicit type parameters (e.g., `T` in `fun f[T](x: T) -> T`)
        // - Inferred type variables from unannotated params/returns (e.g., `fun id(x) { x }`)
        // - Type variables that only appear in constraints (e.g., `T` in `fun f[T, F: Fallible[T]](x: F)`)
        let outer_free = self.env.free_vars(&self.ty_arena, &mut self.uf);
        let mut fn_free = self.uf.free_vars(fn_ty, &self.ty_arena);

        // Add free variables from constraints
        scheme_constraints.iter().for_each(|(tv, class)| {
            fn_free.insert(*tv);
            fn_free.extend(class.free_vars(&self.ty_arena, &mut self.uf));
        });

        let mut vars: SmallVec<[TyVar; 4]> = fn_free
            .into_iter()
            .filter(|v| !outer_free.contains(v))
            .collect();
        vars.sort_unstable();
        let scheme = Scheme {
            vars,
            ty: fn_ty,
            constraints: scheme_constraints,
        };
        // Clone is cheap: `Scheme` is a small struct with a `SmallVec`.
        self.env.bind(name, scheme.clone());
        self.hoist.fun_index.insert(scheme.ty, stmt_id);
        self.hoist.funs.insert(stmt_id, scheme);
    }

    /// Hoist a module declaration and its members.
    ///
    /// Registers the module name and hoists all function members with
    /// provisional types. Nested modules are processed recursively.
    ///
    /// Uses the same three-phase approach as top-level hoisting:
    /// 1. Process nested modules and type declarations
    /// 2. Process imports inside the module
    /// 3. Process functions, LETs, and class instances
    fn hoist_module(
        &mut self,
        mod_path: QualifiedName,
        body: &[StmtId],
        span: Span,
    ) {
        // Register the module name
        self.env.register_user_module(mod_path.clone());

        // Save and set current module for unqualified type resolution
        let prev_module = self.current_module.replace(mod_path.clone());

        // Phase 1: Process nested modules and type declarations
        body.iter().for_each(|&id| {
            let item_span = self.ast.stmt_span(id).unwrap_or(span);
            let item = self.ast.get_stmt(id).cloned();

            match item {
                Some(Stmt::Module { ref name, ref body }) => {
                    // Nested module; recurse with qualified path
                    self.hoist_module(mod_path.child(*name), body, item_span);
                }

                // TYPE/union/newtype: register visibility for imports.
                // Type definitions are processed by registry; we only need
                // to record visibility so imports can check access.
                Some(Stmt::Type { ref name, vis, .. })
                | Some(Stmt::Union { ref name, vis, .. })
                | Some(Stmt::NewType { ref name, vis, .. }) => {
                    let n =
                        self.env.resolve_str(*name).to_owned();
                    // Check for shadowing of builtin types
                    if self.named_type_to_ty(&n) != TyArena::UNKNOWN {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "type `{}` shadows a builtin type",
                                n
                            ),
                            span: item_span,
                        });
                    // Check for shadowing from parent modules
                    } else if let Some(parent) = mod_path.parent() {
                        // Temporarily set current_module to parent for lookup
                        let saved =
                            self.current_module.replace(parent);
                        if self.resolve_type_name(&QualifiedName::local(*name)).is_some() {
                            self.error(TypeError::Custom {
                                msg: format!(
                                    "type `{}` already in scope from outer module",
                                    n
                                ),
                                span: item_span,
                            });
                        }
                        self.current_module = saved;
                    }
                    let qn = mod_path.child(*name);
                    self.env.register_user_module_type_vis(qn, vis);
                }

                _ => {}
            }
        });

        // Phase 2: Process imports inside the module
        body.iter().for_each(|&id| {
            let item_span = self.ast.stmt_span(id).unwrap_or(span);
            let item = self.ast.get_stmt(id).cloned();

            if let Some(Stmt::Import(ref import)) = item {
                self.import_stmt(import, item_span);
            }
        });

        // Phase 3: Process functions, LETs, and class instances
        body.iter().for_each(|&id| {
            let item_span = self.ast.stmt_span(id).unwrap_or(span);
            let item = self.ast.get_stmt(id).cloned();

            match item {
                Some(Stmt::Fun {
                    ref name,
                    ref type_params,
                    ref params,
                    ref ret,
                    vis,
                    ..
                }) => {
                    // Hoist the function
                    self.hoist_fun(
                        id,
                        *name,
                        type_params,
                        params,
                        ret.as_ref(),
                        item_span,
                    );

                    // Register as module member with provisional type
                    if let Some(scheme) = self.env.lookup(*name).cloned() {
                        self.env.register_user_module_member(
                            mod_path.clone(),
                            *name,
                            scheme,
                            vis,
                        );
                    }
                }

                // Module let bindings: hoist with provisional type.
                // Only simple bindings are valid; destructuring rejected in Pass 2.
                Some(Stmt::Let(
                    BindingPattern::Var(ref const_name),
                    ref ann,
                    ref rhs,
                    vis,
                )) => {
                    let closure = self.ast.get_expr(*rhs).cloned().and_then(
                        |e| match e {
                            Expr::Closure {
                                type_params,
                                params,
                                ret,
                                ..
                            } => Some((type_params, params, ret)),
                            _ => None,
                        },
                    );
                    match closure {
                        Some((type_params, params, ret)) => {
                            // Closure-RHS: parity with `fun`. `hoist_fun`
                            // registers both the env binding AND the
                            // `hoisted_funs` entry.
                            self.hoist_fun(
                                id,
                                *const_name,
                                &type_params,
                                &params,
                                ret.as_ref(),
                                item_span,
                            );
                            if let Some(scheme) =
                                self.env.lookup(*const_name).cloned()
                            {
                                self.env.register_user_module_member(
                                    mod_path.clone(),
                                    *const_name,
                                    scheme,
                                    vis,
                                );
                            }
                        }
                        None => {
                            // Non-closure: provisional `TyVar`, tracked for
                            // Pass 2 unify.
                            let ty = match ann {
                                Some(aid) => {
                                    self.ast_type_to_ty(*aid, &IndexMap::new())
                                }
                                None => self.fresh(),
                            };
                            let scheme = Scheme::mono(ty);
                            self.env.bind(*const_name, scheme.clone());
                            self.env.register_user_module_member(
                                mod_path.clone(),
                                *const_name,
                                scheme,
                                vis,
                            );
                            // `QualifiedName` clone is typically stack-only (`SmallVec`).
                            self.hoist
                                .module_lets
                                .insert((mod_path.clone(), *const_name), ty);
                        }
                    }
                }

                Some(Stmt::ClassInstance {
                    ref class_name,
                    ref class_args,
                    ref type_params,
                    for_type,
                    ref constraints,
                    ref methods,
                    ..
                }) => {
                    self.hoist_class_instance(ClassInstanceInput {
                        class_name: *class_name,
                        class_args,
                        type_params,
                        for_type,
                        constraints,
                        methods,
                        assoc_types: (),
                        module: Some(mod_path.clone()),
                        span: item_span,
                    });
                }

                // Modules, types, and imports already processed in earlier phases
                _ => {}
            }
        });

        // Restore previous module
        self.current_module = prev_module;
    }

    /// Hoist a class instance declaration.
    ///
    /// Registers the instance in `instance_registry` so that class method
    /// calls can find user instances even when the CLASS statement appears
    /// after the call site (forward reference).
    ///
    /// The `module` field is `Some(path_id)` when the CLASS is inside a
    /// module, `None` for top-level instances.
    fn hoist_class_instance(&mut self, input: ClassInstanceInput<'_>) {
        let ClassInstanceInput {
            class_name,
            class_args,
            type_params,
            for_type,
            constraints,
            methods,
            assoc_types: _,
            module,
            span,
        } = input;

        // Parse class name; silently skip if invalid (error in Pass 2)
        let cn = self.env.resolve_str(class_name).to_owned();
        if let Some(class) = ClassId::from_name(&cn) {
            // Build type parameter substitution from WHERE constraints
            let mut type_param_subst: IndexMap<_, _> = if type_params.is_empty()
            {
                constraints
                    .iter()
                    .map(|(name, _)| {
                        let tv = self.fresh_var();
                        let ty_id = self.ty_arena.alloc(Ty::Var(tv));
                        (*name, ty_id)
                    })
                    .collect()
            } else {
                type_params
                    .iter()
                    .map(|tp| {
                        let tv = self.fresh_var();
                        let ty_id = self.ty_arena.alloc(Ty::Var(tv));
                        (tp.name, ty_id)
                    })
                    .collect()
            };

            // Merge type vars from `for_type` (e.g. `T` in `X[T]`)
            self.merge_for_type_vars(for_type, &mut type_param_subst);

            // Resolve for_type and class args; HKT classes use partial
            // application (fewer type args than the type expects)
            let resolved = match self.env.class_registry().shape(class) {
                ClassShape::Hkt { .. } => self.resolve_hkt_for_type(
                    class,
                    for_type,
                    class_args,
                    &mut type_param_subst,
                    &module,
                    span,
                ),
                _ => {
                    let for_ty =
                        self.ast_type_to_ty(for_type, &type_param_subst);

                    // Fallback for module-scoped unqualified type names
                    let for_ty = if for_ty == TyArena::UNKNOWN {
                        if let Some(ref mod_qn) = module {
                            let raw = self.extract_type_name_from_ast(for_type);
                            if raw.contains('.') {
                                for_ty
                            } else {
                                self.env
                                    .lookup_str(&raw)
                                    .map(|id| mod_qn.child(id))
                                    .and_then(|qn| self.registry.lookup(&qn))
                                    .map(|tid| {
                                        self.ty_arena.named(tid, smallvec![])
                                    })
                                    .unwrap_or(for_ty)
                            }
                        } else {
                            for_ty
                        }
                    } else {
                        for_ty
                    };

                    let class_arg_tys: SmallVec<[TyId; 2]> = class_args
                        .iter()
                        .map(|id| self.ast_type_to_ty(*id, &type_param_subst))
                        .collect();

                    let for_ty_ref = self.ty_arena.get(for_ty).clone();
                    match &for_ty_ref {
                        Ty::Named(id, _) | Ty::Union(Some(id), _) => {
                            Some((*id, for_ty, class_arg_tys))
                        }
                        _ => self
                            .primitive_type_id(&for_ty_ref)
                            .map(|id| (id, for_ty, class_arg_tys)),
                    }
                }
            };

            if let Some((type_id, for_ty, class_arg_tys)) = resolved {
                // Check if this is a forbidden builtin instance (same logic as stmt.rs)
                // Allow if any class arg is a user-defined type
                let is_forbidden_builtin = self.is_builtin_type(type_id)
                    && (class_arg_tys.is_empty()
                        || class_arg_tys
                            .iter()
                            .all(|&ty_id| self.is_builtin_ty(ty_id)));

                if !is_forbidden_builtin {
                    // Process constraints
                    let mut scheme_constraints: SmallVec<
                        [(TyVar, TypeClass<TyId>); 2],
                    > = SmallVec::new();
                    constraints.iter().for_each(
                        |(param_name, param_constraints)| {
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
                                    self.ast_class_to_ty_class(
                                        c,
                                        &type_param_subst,
                                    ),
                                ));
                            });
                        },
                    );

                    // Build method map (empty for hoisting; filled in Pass 2)
                    // Use qualified type name for function name generation to avoid collisions.
                    let type_name_for_fn =
                        match (self.ty_arena.get(for_ty), &module) {
                            (Ty::Named(_, _), Some(mod_qn)) => {
                                let raw_name =
                                    self.extract_type_name_from_ast(for_type);
                                if raw_name.contains('.') {
                                    raw_name
                                } else {
                                    let mp = mod_qn.display(&self.env.strings);
                                    format!("{}.{}", mp, raw_name)
                                }
                            }
                            _ => self.extract_type_name_from_ast(for_type),
                        };
                    let method_map: HashMap<_, _> = methods
                        .iter()
                        .map(|m| {
                            let mn = self.env.resolve_str(m.name);
                            let fn_name =
                                crate::interpreter::instance::instance_fn_name(
                                    class.name(),
                                    &type_name_for_fn,
                                    mn,
                                );
                            let fn_name_id = self.env.intern(&fn_name);
                            (m.name, fn_name_id)
                        })
                        .collect();

                    // Collect all type params (both `Ty::Var` and concrete)
                    // in positional order for 1:1 zip with `type_args`
                    let type_all_params: SmallVec<[TyId; 2]> =
                        type_param_subst.values().copied().collect();

                    // Register instance (ignore duplicate errors; caught in Pass 2)
                    let inst = Instance {
                        class,
                        class_args: class_arg_tys,
                        type_params: type_all_params,
                        constraints: scheme_constraints,
                        methods: method_map,
                        assoc_types: SmallVec::new(),
                        module,
                        span,
                    };
                    if let Err(e) =
                        self.instance_registry.register(type_id, inst)
                    {
                        self.error(e);
                    }
                }
            }
        }
    }

    /// Resolve `for_type` for an HKT class instance with partial application.
    ///
    /// Allows fewer type arguments than the type definition expects; the
    /// remaining positions become element type variables for the HKT class.
    /// Returns `(type_id, for_ty, class_arg_tys)` on success.
    pub(super) fn resolve_hkt_for_type(
        &mut self,
        class: ClassId,
        for_type: AstTypeExprId,
        class_args: &[AstTypeExprId],
        subst: &mut IndexMap<StringId, TyId>,
        module: &Option<QualifiedName>,
        span: Span,
    ) -> Option<(TypeId, TyId, SmallVec<[TyId; 2]>)> {
        let kind = match self.env.class_registry().shape(class) {
            ClassShape::Hkt { kind } => kind,
            _ => 0,
        };
        // HKT element types are inferred; explicit class args not allowed
        if !class_args.is_empty() {
            self.error(TypeError::Custom {
                msg: format!(
                    "class `{}` is higher-kinded; element types are \
                     inferred from the type's remaining parameters",
                    class.name(),
                ),
                span,
            });
        }

        // Extract base type name and explicit args from AST
        let (type_name, ast_args) = self
            .ast
            .get_type_expr(for_type)
            .cloned()
            .and_then(|te| match te {
                AstTypeExpr::Named(name) => {
                    Some((name, SmallVec::<[AstTypeExprId; 2]>::new()))
                }
                AstTypeExpr::App(name, args) => Some((name, args)),
                _ => None,
            })
            .or_else(|| {
                self.error(TypeError::Custom {
                    msg: "expected a named type for HKT class instance".into(),
                    span,
                });
                None
            })?;

        // Resolve type name (with module fallback)
        let (type_id, qn) = self
            .resolve_type_name(&type_name)
            .or_else(|| {
                module.as_ref().and_then(|mod_qn| {
                    let raw = type_name.display(&self.env.strings);
                    if raw.contains('.') {
                        None
                    } else {
                        self.env
                            .lookup_str(&raw)
                            .map(|id| mod_qn.child(id))
                            .and_then(|qn| {
                                self.registry.lookup(&qn).map(|tid| (tid, qn))
                            })
                    }
                })
            })
            .or_else(|| {
                self.error(TypeError::UnknownType(
                    type_name.display(&self.env.strings),
                    span,
                ));
                None
            })?;

        // Validate partial application
        let total = self.registry.type_param_count(type_id).unwrap_or(0);
        let supplied = ast_args.len();
        let k = kind as usize;
        let type_name_s = qn.display(&self.env.strings);

        if total < supplied {
            self.error(TypeError::TypeArityMismatch {
                name: type_name_s.clone(),
                expected: total,
                got: supplied,
                span,
            });
            None?
        }

        let remaining = total - supplied;
        if remaining != k {
            self.error(TypeError::Custom {
                msg: format!(
                    "type `{}` has {} type parameter(s); class `{}` \
                     requires exactly {} unfilled, but {} are unfilled",
                    type_name_s,
                    total,
                    class.name(),
                    k,
                    remaining,
                ),
                span,
            });
            None?
        }

        // Convert supplied type args
        let supplied_args: SmallVec<[TyId; 4]> = ast_args
            .iter()
            .map(|a| self.ast_type_to_ty(*a, subst))
            .collect();

        // Get param names from `TypeDef` for naming element vars
        let param_names: SmallVec<[StringId; 2]> = self
            .registry
            .get_def(type_id)
            .and_then(|d| match d {
                TypeDef::Sum { type_params, .. }
                | TypeDef::Alias { type_params, .. }
                | TypeDef::Union { type_params, .. } => {
                    Some(type_params.clone())
                }
                TypeDef::Builtin(_) => None,
            })
            .unwrap_or_default();

        // Create fresh vars for element positions (the unfilled params)
        let elem_tys: SmallVec<[TyId; 2]> = (0..remaining)
            .map(|i| {
                let tv = self.fresh_var();
                let ty = self.ty_arena.alloc(Ty::Var(tv));
                // Register under the `TypeDef`'s param name so method
                // signatures that reference it resolve correctly
                if let Some(&name) = param_names.get(supplied + i) {
                    subst.insert(name, ty);
                }
                ty
            })
            .collect();

        // Build `for_ty` with all args: `[...supplied, ...element_vars]`
        let full_args: SmallVec<[TyId; 4]> = supplied_args
            .iter()
            .chain(elem_tys.iter())
            .copied()
            .collect();

        Some((type_id, self.ty_arena.named(type_id, full_args), elem_tys))
    }
}
