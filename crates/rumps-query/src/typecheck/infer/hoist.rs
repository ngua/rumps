//! Declaration hoisting for forward references.
//!
//! Implements Pass 1 of the two-pass type inference: traverse statements and
//! register function/module names with provisional types before any body
//! inference. This enables forward references and mutual recursion.

use std::collections::HashMap;

use smallvec::SmallVec;

use super::{ClassInstanceInput, InferCtx};
use crate::ast::{AstTypeExprId, BindingPattern, Stmt, StmtId, TypeParam};
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::Instance;
use crate::typecheck::ty::{BuiltinClassTag, Class, Scheme, Ty, TyVar};
use crate::Span;

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
                self.hoist_module(name, body, span);
            }
        });

        // Phase 2: Process imports (populates imported_types)
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
            }) => {
                self.hoist_fun(&name, &type_params, &params, ret.as_ref(), span)
            }

            Some(Stmt::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types: _,
                methods,
            }) => self.hoist_class_instance(ClassInstanceInput {
                class_name: &class_name,
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
        name: &str,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
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

        let type_param_subst: HashMap<_, _> = type_param_vars
            .iter()
            .map(|(tp, tv)| {
                let id = self.env.intern(&tp.name);
                (id, Ty::Var(*tv))
            })
            .collect();

        // Process type parameter constraints
        let mut scheme_constraints: SmallVec<[(TyVar, Class); 2]> =
            SmallVec::new();
        type_param_vars.iter().for_each(|(tp, tv)| {
            tp.constraints.iter().for_each(|c| {
                let class = self.ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((*tv, class));
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        // Return type: use annotation if present, else fresh var
        let ret_ty = match ret {
            Some(id) => self.ast_type_to_ty(*id, &type_param_subst),
            None => self.fresh(),
        };

        // Build function type
        let fn_ty = Ty::Fn(param_tys, Box::new(ret_ty));

        // Generalize over ALL free type variables in both the function type
        // and the constraints. This includes:
        // - Explicit type parameters (e.g., `T` in `FUN f[T](x: T) -> T`)
        // - Inferred type variables from unannotated params/returns (e.g., `FUN id(x) { x }`)
        // - Type variables that only appear in constraints (e.g., `T` in `FUN f[T, F: Fallible[T]](x: F)`)
        let outer_free = self.env.free_vars();
        let mut fn_free = fn_ty.free_vars();

        // Add free variables from constraints
        scheme_constraints.iter().for_each(|(tv, class)| {
            fn_free.insert(*tv);
            fn_free.extend(class.free_vars());
        });

        let vars: Vec<_> = fn_free
            .into_iter()
            .filter(|v| !outer_free.contains(v))
            .collect();
        let scheme = Scheme {
            vars,
            ty: fn_ty,
            constraints: scheme_constraints,
        };
        self.env.bind(name, scheme);
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
    fn hoist_module(&mut self, mod_path: &str, body: &[StmtId], span: Span) {
        // Register the module name first
        self.env.register_user_module(mod_path);

        // Save and set current module for unqualified type resolution
        let prev_module = self.current_module.replace(mod_path.to_string());

        // Phase 1: Process nested modules and type declarations
        body.iter().for_each(|&id| {
            let item_span = self.ast.stmt_span(id).unwrap_or(span);
            let item = self.ast.get_stmt(id).cloned();

            match item {
                Some(Stmt::Module { ref name, ref body }) => {
                    // Nested module; recurse with qualified path
                    let nested_path = format!("{}.{}", mod_path, name);
                    self.hoist_module(&nested_path, body, item_span);
                }

                // TYPE/UNION/NEWTYPE: register visibility for imports.
                // Type definitions are processed by registry; we only need
                // to record visibility so imports can check access.
                Some(Stmt::Type { ref name, vis, .. })
                | Some(Stmt::Union { ref name, vis, .. })
                | Some(Stmt::NewType { ref name, vis, .. }) => {
                    // Check for shadowing of builtin types
                    if self.named_type_to_ty(name) != Ty::Unknown {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "type `{}` shadows a builtin type",
                                name
                            ),
                            span: item_span,
                        });
                    // Check for shadowing from parent modules
                    } else if let Some(parent) =
                        mod_path.rsplit_once('.').map(|(p, _)| p)
                    {
                        // Temporarily set current_module to parent for lookup
                        let saved = self.current_module.replace(parent.to_string());
                        if self.resolve_type_name(name).is_some() {
                            self.error(TypeError::Custom {
                                msg: format!(
                                    "type `{}` already in scope from outer module",
                                    name
                                ),
                                span: item_span,
                            });
                        }
                        self.current_module = saved;
                    }
                    let qname = format!("{}.{}", mod_path, name);
                    self.env.register_user_module_type_vis(&qname, vis);
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
                        name,
                        type_params,
                        params,
                        ret.as_ref(),
                        item_span,
                    );

                    // Register as module member with provisional type
                    if let Some(scheme) = self.env.lookup(name).cloned() {
                        self.env.register_user_module_member(
                            mod_path, name, scheme, vis,
                        );
                    }
                }

                // Module LET bindings: hoist with provisional type.
                // Only simple bindings are valid; destructuring rejected in Pass 2.
                Some(Stmt::Let(
                    BindingPattern::Var(ref const_name),
                    ref ann,
                    _,
                    vis,
                )) => {
                    // Use annotation if present, else fresh type variable
                    let ty = match ann {
                        Some(id) => self.ast_type_to_ty(*id, &HashMap::new()),
                        None => self.fresh(),
                    };
                    let scheme = Scheme::mono(ty);
                    self.env.bind(const_name, scheme.clone());
                    self.env.register_user_module_member(
                        mod_path, const_name, scheme, vis,
                    );
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
                    let mod_id = self.env.intern(mod_path);
                    self.hoist_class_instance(ClassInstanceInput {
                        class_name,
                        class_args,
                        type_params,
                        for_type,
                        constraints,
                        methods,
                        assoc_types: (),
                        module: Some(mod_id),
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
        if let Some(class) = BuiltinClassTag::from_str(class_name) {
            // Build type parameter substitution from WHERE constraints
            let type_param_subst: HashMap<_, _> = if type_params.is_empty() {
                constraints
                    .iter()
                    .map(|(name, _)| {
                        let id = self.env.intern(name);
                        let tv = self.fresh_var();
                        (id, Ty::Var(tv))
                    })
                    .collect()
            } else {
                type_params
                    .iter()
                    .map(|tp| {
                        let id = self.env.intern(&tp.name);
                        let tv = self.fresh_var();
                        (id, Ty::Var(tv))
                    })
                    .collect()
            };

            // Resolve for_type
            let for_ty = self.ast_type_to_ty(for_type, &type_param_subst);

            // Fallback for module-scoped unqualified type names: if `for_ty` is
            // `Unknown` and we're inside a module, try the qualified name.
            let for_ty = match (&for_ty, module) {
                (Ty::Unknown, Some(mod_id)) => {
                    let raw_name = self.extract_type_name_from_ast(for_type);
                    if raw_name.contains('.') {
                        for_ty // Already qualified
                    } else {
                        let mod_path =
                            self.env.get_str(mod_id).map(String::from);
                        mod_path
                            .map(|mp| {
                                let qname = format!("{}.{}", mp, raw_name);
                                let qname_id = self.env.intern(&qname);
                                self.registry
                                    .lookup(qname_id)
                                    .map(|tid| Ty::Named(tid, Vec::new()))
                                    .unwrap_or(for_ty.clone())
                            })
                            .unwrap_or(for_ty)
                    }
                }
                _ => for_ty,
            };

            // Convert class args (needed for builtin check)
            let class_arg_tys: SmallVec<[Ty; 2]> = class_args
                .iter()
                .map(|id| self.ast_type_to_ty(*id, &type_param_subst))
                .collect();

            // Extract TypeId; for primitives, use primitive_type_id
            let type_id_opt = match &for_ty {
                Ty::Named(id, _) => Some(*id),
                _ => self.primitive_type_id(&for_ty),
            };

            if let Some(type_id) = type_id_opt {
                // Check if this is a forbidden builtin instance (same logic as stmt.rs)
                // Allow if any class arg is a user-defined type
                let is_forbidden_builtin = self.is_builtin_type(type_id)
                    && (class_arg_tys.is_empty()
                        || class_arg_tys
                            .iter()
                            .all(|ty| self.is_builtin_ty(ty)));

                if !is_forbidden_builtin {
                    // Process constraints
                    let mut scheme_constraints: SmallVec<[(TyVar, Class); 2]> =
                        SmallVec::new();
                    constraints.iter().for_each(
                        |(param_name, param_constraints)| {
                            let param_id = self.env.intern(param_name);
                            let ty = type_param_subst
                                .get(&param_id)
                                .cloned()
                                .unwrap_or(Ty::Unknown);
                            let tv = match ty {
                                Ty::Var(v) => v,
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
                    let type_name_for_fn = match (&for_ty, module) {
                        (Ty::Named(_, _), Some(mod_id)) => {
                            let raw_name =
                                self.extract_type_name_from_ast(for_type);
                            if raw_name.contains('.') {
                                raw_name
                            } else {
                                self.env
                                    .get_str(mod_id)
                                    .map(|mod_path| {
                                        format!("{}.{}", mod_path, raw_name)
                                    })
                                    .unwrap_or(raw_name)
                            }
                        }
                        _ => self.extract_type_name_from_ast(for_type),
                    };
                    let method_map: HashMap<_, _> = methods
                        .iter()
                        .map(|m| {
                            let method_id = self.env.intern(&m.name);
                            let fn_name =
                                crate::interpreter::instance::instance_fn_name(
                                    class,
                                    &type_name_for_fn,
                                    &m.name,
                                );
                            let fn_name_id = self.env.intern(&fn_name);
                            (method_id, fn_name_id)
                        })
                        .collect();

                    // Extract type params as TyVars
                    let type_var_params: SmallVec<[TyVar; 2]> =
                        if type_params.is_empty() {
                            constraints
                                .iter()
                                .filter_map(|(name, _)| {
                                    let id = self.env.intern(name);
                                    type_param_subst.get(&id).and_then(|ty| {
                                        match ty {
                                            Ty::Var(v) => Some(*v),
                                            _ => None,
                                        }
                                    })
                                })
                                .collect()
                        } else {
                            type_params
                                .iter()
                                .filter_map(|tp| {
                                    let id = self.env.intern(&tp.name);
                                    type_param_subst.get(&id).and_then(|ty| {
                                        match ty {
                                            Ty::Var(v) => Some(*v),
                                            _ => None,
                                        }
                                    })
                                })
                                .collect()
                        };

                    // Register instance (ignore duplicate errors; caught in Pass 2)
                    let inst = Instance {
                        class,
                        class_args: class_arg_tys,
                        type_params: type_var_params,
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
}
