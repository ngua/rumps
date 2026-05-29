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
    AstClassAssocTypeDecl, AstClassMethodSig, AstTypeExpr, AstTypeExprId,
    BindingPattern, Expr, Import, Stmt, StmtId, TypeParam,
};
use crate::env::PRELUDE_MODULE;
use crate::intern::{QualifiedName, StringId};
use crate::interpreter::instance::RuntimeInstance;
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::Instance;
use crate::typecheck::ty::{
    ClassDef, ClassShape, MethodSpec, Scheme, Ty, TyArena, TyId, TyVar,
    TypeClass,
};
use crate::value::{TypeDef, TypeId};
use crate::{ClassId, Span};

/// AST-level class definition fields, bundled for `hoist_class_def`.
struct ClassDefInput<'a> {
    name: StringId,
    class_params: &'a [TypeParam],
    self_var: StringId,
    supers: &'a SmallVec<[TypeClass<AstTypeExprId>; 2]>,
    assoc_types: &'a [AstClassAssocTypeDecl],
    methods: &'a [AstClassMethodSig],
    span: Span,
}

/// Resolved class-level context shared by all methods in a class def.
///
/// Built once in `hoist_class_def`; passed to `build_class_method_spec`.
struct ClassDefCtx<'a> {
    class_id: ClassId,
    shape: ClassShape,
    self_var: StringId,
    class_params: &'a [TypeParam],
    assoc_types: &'a [AstClassAssocTypeDecl],
}

/// Per-method context for resolving types in a user class definition.
///
/// Built once per method in `build_class_method_spec`; passed by reference
/// to `resolve_class_method_type` for each parameter/return type expression.
struct ClassMethodCtx {
    class_id: ClassId,
    shape: ClassShape,
    self_var: StringId,
    self_var_idx: u32,
    subst: IndexMap<StringId, TyId>,
    assoc_map: HashMap<StringId, TyId>,
}

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
        // Phase 0: Register user-defined class stubs so class names
        // are available for constraint resolution and method lookup
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::ClassDef {
                ref name,
                ref class_params,
                self_var,
                ref assoc_types,
                ref methods,
                ..
            }) = stmt
            {
                self.register_class_stub(
                    *name,
                    class_params,
                    self_var,
                    assoc_types,
                    methods,
                    self.ast.stmt_span(id).unwrap_or_default(),
                );
            }
        });

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
            let pid = self.env.intern(PRELUDE_MODULE);
            self.import(&Import::wildcard(pid), Span::default());
        }
        stmts.iter().for_each(|&id| {
            let stmt = self.ast.get_stmt(id).cloned();
            if let Some(Stmt::Import(ref import)) = stmt {
                let span = self.ast.stmt_span(id).unwrap_or_default();
                self.import(import, span);
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
                            );
                        }
                        None => {
                            let ty = match ann {
                                Some(aid) => {
                                    self.convert().ast_type_to_ty(aid, &IndexMap::new())
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
            }) => self.hoist_fun(id, name, &type_params, &params, ret.as_ref()),

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

            Some(Stmt::ClassDef {
                name,
                class_params,
                self_var,
                supers,
                assoc_types,
                methods,
            }) => self.hoist_class_def(ClassDefInput {
                name,
                class_params: &class_params,
                self_var,
                supers: &supers,
                assoc_types: &assoc_types,
                methods: &methods,
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
                let class =
                    self.convert().ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((*tv, class));
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys: SmallVec<[TyId; 4]> = params
            .iter()
            .map(|(_, ann)| match ann {
                Some(id) => {
                    self.convert().ast_type_to_ty(*id, &type_param_subst)
                }
                None => self.fresh(),
            })
            .collect();

        // Return type: use annotation if present, else fresh var
        let ret_ty = match ret {
            Some(id) => self.convert().ast_type_to_ty(*id, &type_param_subst),
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

                // `variant`/`union`/`newtype`: register visibility for imports.
                // Type definitions are processed by registry; we only need
                // to record visibility so imports can check access.
                Some(Stmt::Type { ref name, vis, .. })
                | Some(Stmt::Union { ref name, vis, .. })
                | Some(Stmt::Newtype { ref name, vis, .. }) => {
                    let n =
                        self.env.resolve_str(*name).to_owned();
                    // Check for shadowing of builtin types
                    if self.convert().named_type_to_ty(&n) != TyArena::UNKNOWN {
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
                        if self.convert().resolve_type_name(&QualifiedName::local(*name)).is_some() {
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
                self.import(import, item_span);
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
                                Some(aid) => self
                                    .convert()
                                    .ast_type_to_ty(*aid, &IndexMap::new()),
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

    /// Register a user-defined class stub in the class registry.
    ///
    /// Called during Phase 0 of hoisting to make class names available
    /// for constraint resolution and method lookup in later phases.
    fn register_class_stub(
        &mut self,
        name: StringId,
        class_params: &[TypeParam],
        self_var: StringId,
        assoc_types: &[AstClassAssocTypeDecl],
        methods: &[AstClassMethodSig],
        span: Span,
    ) {
        let is_param = !class_params.is_empty();
        let kind = self.self_var_hkt_kind(self_var, methods, span);

        let shape = if is_param && kind > 0 {
            ClassShape::Hkt {
                kind,
                params: class_params.len() as u8,
            }
        } else if is_param {
            ClassShape::Concrete {
                params: class_params.len() as u8,
            }
        } else if kind > 0 {
            ClassShape::Hkt { kind, params: 0 }
        } else {
            ClassShape::Concrete { params: 0 }
        };

        let assoc_names = assoc_types.iter().map(|a| a.name).collect();
        let stub = ClassDef {
            name,
            shape,
            assoc_types: assoc_names,
            methods: vec![],
            supers: smallvec![],
        };

        if let Err(e) = self.env.class_registry.register(stub) {
            let nm = self.env.resolve_str(e.name).to_owned();
            self.error(TypeError::Custom {
                msg: format!("duplicate class definition `{nm}`"),
                span,
            });
        }
    }

    /// Compute the HKT kind of `sv` from AST method signatures.
    ///
    /// Returns `0` if `sv` is never used as a type constructor, or
    /// `n` if it is consistently applied to `n` type arguments.
    /// Emits an error if different methods use inconsistent arities.
    fn self_var_hkt_kind(
        &mut self,
        sv: StringId,
        methods: &[AstClassMethodSig],
        span: Span,
    ) -> u8 {
        let arities: SmallVec<[u8; 4]> = methods
            .iter()
            .flat_map(|m| {
                m.params
                    .iter()
                    .filter_map(|(_, ty)| ty.as_ref())
                    .chain(m.ret.as_ref())
                    .map(|&te| self.ast_type_expr_hkt_arity(sv, te))
            })
            .filter(|&a| a > 0)
            .collect();

        let first = arities.first().copied();
        let mismatch = arities.iter().find(|&&a| first.is_some_and(|f| a != f));

        if let (Some(f), Some(&m)) = (first, mismatch) {
            self.error(TypeError::Custom {
                msg: format!(
                    "inconsistent HKT arity for self type: \
                     used as kind-{f} and kind-{m}"
                ),
                span,
            });
        }

        first.unwrap_or(0)
    }

    /// Return the arity of `sv` when used as a type constructor in `te`,
    /// or `0` if it does not appear in head position.
    /// Propagates the max across children.
    fn ast_type_expr_hkt_arity(&self, sv: StringId, te: AstTypeExprId) -> u8 {
        match self.ast.get_type_expr(te).cloned() {
            Some(AstTypeExpr::VarApp(name, args)) => {
                let head = if !name.is_qualified() && name.local_name() == sv {
                    args.len() as u8
                } else {
                    0
                };
                args.iter()
                    .map(|&a| self.ast_type_expr_hkt_arity(sv, a))
                    .fold(head, u8::max)
            }
            Some(AstTypeExpr::App(_, args)) => args
                .iter()
                .map(|&a| self.ast_type_expr_hkt_arity(sv, a))
                .fold(0, u8::max),
            Some(AstTypeExpr::Fn(params, ret)) => params
                .iter()
                .map(|&p| self.ast_type_expr_hkt_arity(sv, p))
                .fold(self.ast_type_expr_hkt_arity(sv, ret), u8::max),
            Some(AstTypeExpr::Tuple(elems) | AstTypeExpr::Union(elems)) => {
                elems
                    .iter()
                    .map(|&e| self.ast_type_expr_hkt_arity(sv, e))
                    .fold(0, u8::max)
            }
            Some(AstTypeExpr::Object(fields)) => fields
                .iter()
                .map(|&(_, te)| self.ast_type_expr_hkt_arity(sv, te))
                .fold(0, u8::max),
            Some(AstTypeExpr::TupleConstructor { .. }) => 0,
            _ => 0,
        }
    }

    /// Hoist a user-defined class definition.
    ///
    /// Builds method schemes for each method signature and updates the
    /// stub `ClassDef` (registered during Phase 0) with full
    /// methods and resolved superclass constraints.
    fn hoist_class_def(&mut self, input: ClassDefInput<'_>) {
        if let Some(class_id) =
            self.env.class_registry().lookup_by_name(input.name)
        {
            let shape = self.env.class_registry().shape(class_id);
            let def_ctx = ClassDefCtx {
                class_id,
                shape,
                self_var: input.self_var,
                class_params: input.class_params,
                assoc_types: input.assoc_types,
            };

            // Build method specs
            let method_specs: Vec<(StringId, MethodSpec)> = input
                .methods
                .iter()
                .filter_map(|m| {
                    self.build_class_method_spec(&def_ctx, m, input.span)
                })
                .collect();

            // Resolve superclass constraints
            let empty_subst = IndexMap::new();
            let resolved_supers: SmallVec<[ClassId; 2]> = input
                .supers
                .iter()
                .map(|sup| {
                    self.convert()
                        .ast_class_to_ty_class(sup, &empty_subst)
                        .tag()
                })
                .collect();

            // Update the stub ClassDef in the registry
            let def = self.env.class_registry.get_mut(class_id);
            def.methods = method_specs;
            def.supers = resolved_supers;
        }
    }

    /// Build a `MethodSpec` for a single method signature in a user class def.
    fn build_class_method_spec(
        &mut self,
        dc: &ClassDefCtx<'_>,
        method: &AstClassMethodSig,
        span: Span,
    ) -> Option<(StringId, MethodSpec)> {
        // (a) Allocate type variables and build substitution map
        let mut subst: IndexMap<StringId, TyId> = IndexMap::new();
        let (total_vars, self_var_idx, class_constraint) = match dc.shape {
            ClassShape::Concrete { params } => {
                // Self var -> TyVar(0), class params -> TyVar(1..n),
                // method-local -> TyVar(n+1..)
                let p_start = 1u32;
                let sv_ty = self.ty_arena.var(0);
                subst.insert(dc.self_var, sv_ty);
                let total = dc
                    .class_params
                    .iter()
                    .chain(method.type_params.iter())
                    .fold(p_start, |i, tp| {
                        subst.insert(tp.name, self.ty_arena.var(i));
                        i + 1
                    });
                let cp: SmallVec<[TyId; 1]> = (p_start
                    ..p_start + params as u32)
                    .map(|i| self.ty_arena.var(i))
                    .collect();
                let constraint = (
                    TyVar::new(0),
                    TypeClass::Concrete {
                        id: dc.class_id,
                        params: cp,
                    },
                );
                (total, 0u32, constraint)
            }
            ClassShape::Hkt { params, .. } => {
                // Method-local type params -> TyVar(0..m-1),
                // class (fixed) params -> TyVar(m..m+p-1),
                // self var -> TyVar(m+p)
                let p_start = method.type_params.iter().fold(0u32, |i, tp| {
                    subst.insert(tp.name, self.ty_arena.var(i));
                    i + 1
                });
                let sv_idx = dc.class_params.iter().fold(p_start, |i, tp| {
                    subst.insert(tp.name, self.ty_arena.var(i));
                    i + 1
                });
                let sv_ty = self.ty_arena.var(sv_idx);
                subst.insert(dc.self_var, sv_ty);
                let total = sv_idx + 1;
                let cp: SmallVec<[TyId; 1]> = (p_start
                    ..p_start + params as u32)
                    .map(|i| self.ty_arena.var(i))
                    .collect();
                let constraint = (
                    TyVar::new(sv_idx),
                    TypeClass::Hkt {
                        id: dc.class_id,
                        elems: smallvec![],
                        params: cp,
                    },
                );
                (total, sv_idx, constraint)
            }
        };

        // (c) Pre-allocate associated type nodes
        let assoc_map: HashMap<StringId, TyId> = dc
            .assoc_types
            .iter()
            .map(|a| {
                let sv_tv = TyVar::new(self_var_idx);
                let ty = self.ty_arena.alloc(Ty::AssocType(
                    sv_tv,
                    dc.class_id,
                    a.name,
                ));
                (a.name, ty)
            })
            .collect();

        let ctx = ClassMethodCtx {
            class_id: dc.class_id,
            shape: dc.shape,
            self_var: dc.self_var,
            self_var_idx,
            subst,
            assoc_map,
        };

        // (d) Resolve method param/return types
        let param_tys: SmallVec<[TyId; 4]> = method
            .params
            .iter()
            .map(|(_, ty_opt)| match ty_opt {
                Some(te) => self.resolve_class_method_type(*te, &ctx),
                None => {
                    self.error(TypeError::Custom {
                        msg: "class method parameter requires a type \
                              annotation"
                            .into(),
                        span,
                    });
                    TyArena::ERROR
                }
            })
            .collect();

        let ret_ty = method.ret.map_or(TyArena::UNIT, |te| {
            self.resolve_class_method_type(te, &ctx)
        });

        // (e) Build function type
        let fn_ty = self.ty_arena.func(param_tys, ret_ty);

        let mut cs = smallvec![class_constraint];
        method.type_params.iter().for_each(|tp| {
            let tv = ctx.subst.get(&tp.name).copied().and_then(|ty| {
                if let Ty::Var(tv) = self.ty_arena.get(ty) {
                    Some(*tv)
                } else {
                    None
                }
            });
            if let Some(tv) = tv {
                tp.constraints.iter().for_each(|c| {
                    let class =
                        self.convert().ast_class_to_ty_class(c, &ctx.subst);
                    cs.push((tv, class.clone()));
                    self.env
                        .class_registry()
                        .transitive_supers(class.tag())
                        .into_iter()
                        .for_each(|sup| {
                            if let Some(sc) = class.with_tag(sup) {
                                cs.push((tv, sc));
                            }
                        });
                });
            }
        });

        // (g) Assemble scheme
        let scheme = Scheme {
            vars: (0..total_vars).map(TyVar::new).collect(),
            ty: fn_ty,
            constraints: cs,
        };

        Some((method.name, MethodSpec::Standard(scheme)))
    }

    /// Resolve a type expression in a class method signature.
    ///
    /// Handles the substitution map for type variables, HKT self var
    /// usage (producing `Ty::Apply`), and associated type references.
    fn resolve_class_method_type(
        &mut self,
        te: AstTypeExprId,
        ctx: &ClassMethodCtx,
    ) -> TyId {
        let expr = self.ast.get_type_expr(te).cloned();
        match expr {
            Some(AstTypeExpr::Named(ref name))
                if !name.is_qualified()
                    && ctx.subst.contains_key(&name.local_name()) =>
            {
                ctx.subst
                    .get(&name.local_name())
                    .copied()
                    .unwrap_or(TyArena::ERROR)
            }
            Some(AstTypeExpr::App(ref name, ref args))
                if !name.is_qualified()
                    && name.local_name() == ctx.self_var
                    && matches!(ctx.shape, ClassShape::Hkt { .. }) =>
            {
                // HKT self var applied to args: C[T] -> Apply(TyVar(sv), [args])
                let arg_tys: SmallVec<[TyId; 4]> = args
                    .iter()
                    .map(|&a| self.resolve_class_method_type(a, ctx))
                    .collect();
                self.ty_arena.hkt(TyVar::new(ctx.self_var_idx), arg_tys)
            }
            Some(AstTypeExpr::VarApp(ref name, ref args))
                if !name.is_qualified()
                    && ctx.subst.contains_key(&name.local_name()) =>
            {
                let base = ctx
                    .subst
                    .get(&name.local_name())
                    .copied()
                    .unwrap_or(TyArena::ERROR);
                let base_ty = self.ty_arena.get(base).clone();
                let arg_tys: SmallVec<[TyId; 4]> = args
                    .iter()
                    .map(|&a| self.resolve_class_method_type(a, ctx))
                    .collect();
                match base_ty {
                    Ty::Var(tv) => self.ty_arena.hkt(tv, arg_tys),
                    _ => self.convert().apply_type_args(base, arg_tys),
                }
            }
            Some(AstTypeExpr::AssocType { class: None, name }) => {
                ctx.assoc_map.get(&name).copied().unwrap_or_else(|| {
                    self.error(TypeError::NoSuchAssocType {
                        class: ctx.class_id,
                        name,
                        span: self.ast.type_expr_span(te).unwrap_or_default(),
                    });
                    TyArena::ERROR
                })
            }
            _ => {
                // Fall through to standard resolution with subst
                self.convert().ast_type_to_ty(te, &ctx.subst)
            }
        }
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
        if let Some(class) =
            self.env.class_registry().lookup_by_name(class_name)
        {
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
            self.convert()
                .merge_for_type_vars(for_type, &mut type_param_subst);

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
                    let for_ty = self
                        .convert()
                        .ast_type_to_ty(for_type, &type_param_subst);

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
                        .map(|id| {
                            self.convert()
                                .ast_type_to_ty(*id, &type_param_subst)
                        })
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
                let bad_try = if class == ClassId::TRY_INTO {
                    class_arg_tys.first().copied().is_some_and(|to| {
                        self.private_try_into_external(
                            for_ty,
                            to,
                            module.clone(),
                            span,
                        )
                    })
                } else {
                    false
                };

                if !is_forbidden_builtin && !into_repr_overlap && !bad_try {
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
                                    self.convert().ast_class_to_ty_class(
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
                    let class_name_str = self
                        .env
                        .resolve_str(self.env.class_registry().name(class))
                        .to_owned();
                    let ca_names: Vec<String> = class_args
                        .iter()
                        .map(|id| self.extract_type_name_from_ast(*id))
                        .collect();
                    let method_map: HashMap<_, _> = methods
                        .iter()
                        .map(|m| {
                            let mn = self.env.resolve_str(m.name);
                            let fn_name = RuntimeInstance::fn_name_owned(
                                &class_name_str,
                                &type_name_for_fn,
                                mn,
                                &ca_names,
                            );
                            let fn_name_id = self.env.intern(&fn_name);
                            (m.name, fn_name_id)
                        })
                        .collect();

                    // Collect all type params in positional order for 1:1
                    // zip with `type_args`. For tuple constructors, use
                    // the full positional list from `for_ty` so that
                    // element vars at interleaved positions are included.
                    let type_all_params: SmallVec<[TyId; 2]> = if type_id
                        == TypeId::TUPLE
                    {
                        match self.ty_arena.get(for_ty).clone() {
                            Ty::Tuple(ts) => ts.iter().copied().collect(),
                            _ => type_param_subst.values().copied().collect(),
                        }
                    } else {
                        type_param_subst.values().copied().collect()
                    };

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
    /// Returns `(type_id, for_ty, elem_tys)` on success.
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
            ClassShape::Hkt { kind, .. } => kind,
            _ => 0,
        };
        // HKT element types are inferred; explicit class args not allowed
        if !class_args.is_empty() {
            self.error(TypeError::Custom {
                msg: format!(
                    "class `{}` is higher-kinded; element types are \
                     inferred from the type's remaining parameters",
                    self.env.resolve_str(self.env.class_registry().name(class)),
                ),
                span,
            });
        }

        enum ForHead {
            Named(QualifiedName, SmallVec<[AstTypeExprId; 2]>),
            TupleCtor {
                arity: u8,
                fixed: SmallVec<[(u8, AstTypeExprId); 2]>,
            },
        }

        let head = self
            .ast
            .get_type_expr(for_type)
            .cloned()
            .and_then(|te| match te {
                AstTypeExpr::Named(name) => {
                    Some(ForHead::Named(name, SmallVec::new()))
                }
                AstTypeExpr::App(name, args) => {
                    Some(ForHead::Named(name, args))
                }
                AstTypeExpr::TupleConstructor { arity, fixed } => {
                    Some(ForHead::TupleCtor { arity, fixed })
                }
                _ => None,
            })
            .or_else(|| {
                self.error(TypeError::Custom {
                    msg: "expected a named type or tuple constructor \
                          for HKT class instance"
                        .into(),
                    span,
                });
                None
            })?;

        match head {
            ForHead::TupleCtor { arity, fixed } => self.resolve_hkt_tuple_ctor(
                class, kind, arity, &fixed, subst, span,
            ),
            ForHead::Named(type_name, ast_args) => self.resolve_hkt_named(
                class, kind, type_name, ast_args, subst, module, span,
            ),
        }
    }

    fn resolve_hkt_tuple_ctor(
        &mut self,
        class: ClassId,
        kind: u8,
        arity: u8,
        fixed: &[(u8, AstTypeExprId)],
        subst: &mut IndexMap<StringId, TyId>,
        span: Span,
    ) -> Option<(TypeId, TyId, SmallVec<[TyId; 2]>)> {
        let elem_count = arity as usize - fixed.len();
        let class_name =
            self.env.resolve_str(self.env.class_registry().name(class));
        if elem_count != kind as usize {
            self.error(TypeError::Custom {
                msg: format!(
                    "tuple constructor has {} element position(s) \
                     but class `{}` expects kind {}",
                    elem_count, class_name, kind,
                ),
                span,
            });
            None?
        }

        // Convert fixed position types and validate they are type variables
        let fixed_tys: SmallVec<[TyId; 2]> = fixed
            .iter()
            .map(|&(_, ast_te)| self.convert().ast_type_to_ty(ast_te, subst))
            .collect();

        // Safe to check raw `Ty::Var` here because types are freshly allocated
        // and have not been unified yet; post-unification this would need normalization
        let concrete = fixed_tys
            .iter()
            .any(|&t| !matches!(self.ty_arena.get(t), Ty::Var(_)));
        if concrete {
            self.error(TypeError::Custom {
                msg: "tuple constructor instances must use type \
                      variables for fixed positions"
                    .into(),
                span,
            });
            None?
        }

        // Fresh vars for element positions (unfilled slots)
        let elem_tys: SmallVec<[TyId; 2]> = (0..elem_count)
            .map(|_| {
                let tv = self.fresh_var();
                self.ty_arena.alloc(Ty::Var(tv))
            })
            .collect();

        // Build tuple with types at their actual positions; fixed types
        // go at their declared positions, element vars fill the rest
        let mut elem_iter = elem_tys.iter().copied();
        let full: SmallVec<[TyId; 4]> = (0..arity)
            .map(|i| {
                fixed
                    .iter()
                    .zip(fixed_tys.iter())
                    .find(|(&(pos, _), _)| pos == i)
                    .map_or_else(
                        || elem_iter.next().unwrap_or(TyArena::ERROR),
                        |(_, &ty)| ty,
                    )
            })
            .collect();

        let for_ty = self.ty_arena.alloc(Ty::Tuple(full));
        Some((TypeId::TUPLE, for_ty, elem_tys))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_hkt_named(
        &mut self,
        class: ClassId,
        kind: u8,
        type_name: QualifiedName,
        ast_args: SmallVec<[AstTypeExprId; 2]>,
        subst: &mut IndexMap<StringId, TyId>,
        module: &Option<QualifiedName>,
        span: Span,
    ) -> Option<(TypeId, TyId, SmallVec<[TyId; 2]>)> {
        // Resolve type name (with module fallback)
        let (type_id, qn) = self
            .convert()
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
                    self.env.resolve_str(self.env.class_registry().name(class)),
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
            .map(|a| self.convert().ast_type_to_ty(*a, subst))
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

        // Normalize builtin parameterized types to their canonical `Ty`
        // form so that the HKT `Apply` resolution in `apply_inner`
        // produces the correct variant (e.g. `Ty::Option(T)` instead of
        // `Ty::Named(OPTION, [T])`)
        let for_ty = match (type_id, full_args.as_slice()) {
            (TypeId::ARRAY, &[a, ..]) => self.ty_arena.alloc(Ty::Array(a)),
            (TypeId::OPTION, &[a, ..]) => self.ty_arena.alloc(Ty::Option(a)),
            (TypeId::RESULT, &[ok, err, ..]) => {
                self.ty_arena.alloc(Ty::Result(ok, err))
            }
            (TypeId::MAP, &[k, v, ..]) => self.ty_arena.alloc(Ty::Map(k, v)),
            _ => self.ty_arena.named(type_id, full_args),
        };

        Some((type_id, for_ty, elem_tys))
    }
}
