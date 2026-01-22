//! Declaration hoisting for forward references.
//!
//! Implements Pass 1 of the two-pass type inference: traverse statements and
//! register function/module names with provisional types before any body
//! inference. This enables forward references and mutual recursion.

use std::collections::HashMap;

use smallvec::SmallVec;

use super::InferCtx;
use crate::ast::{
    self, AstTypeExprId, BindingPattern, InstanceMethodDef, Stmt, StmtId,
    TypeParam, Visibility,
};
use crate::typecheck::instance::Instance;
use crate::typecheck::ty::{Class, ClassKind, Scheme, Ty, TyVar};
use crate::Span;

impl InferCtx<'_> {
    /// Pass 1: Register all function/module declarations with provisional types.
    ///
    /// This enables forward references: functions can call other functions
    /// defined later in the same scope, and modules can be referenced before
    /// their definition.
    pub(crate) fn hoist_declarations(&mut self, stmts: &[StmtId]) {
        stmts.iter().for_each(|&id| self.hoist_stmt(id));
    }

    /// Hoist a single statement's declarations.
    ///
    /// Only processes `FUN` and `MODULE` statements; other statements are
    /// skipped (they don't introduce hoistable bindings).
    fn hoist_stmt(&mut self, id: StmtId) {
        let span = self.ast.stmt_span(id).unwrap_or(Span::new(0, 0));
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

            Some(Stmt::Module { name, body }) => {
                self.hoist_module(&name, &body, span)
            }

            Some(Stmt::ClassInstance {
                class_name,
                class_args,
                type_params,
                for_type,
                constraints,
                assoc_types: _,
                methods,
            }) => self.hoist_class_instance(
                &class_name,
                &class_args,
                &type_params,
                for_type,
                &constraints,
                &methods,
                span,
            ),

            // Other statements don't introduce hoistable bindings
            _ => {}
        }
    }

    /// Hoist a function declaration with a provisional type.
    ///
    /// Creates fresh type variables for type parameters and binds the function
    /// name with a monomorphic function type. The actual generalization happens
    /// in Pass 2 when the function body is inferred.
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
                (tp.name.as_str(), tv)
            })
            .collect();

        let type_param_subst: HashMap<_, _> = type_param_vars
            .iter()
            .map(|(name, tv)| {
                let id = self.env.intern(name);
                (id, Ty::Var(*tv))
            })
            .collect();

        // Infer parameter types (using type param substitution)
        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        // Return type: use annotation if present, else fresh var
        let ret_ty = match ret {
            Some(id) => self.ast_type_to_ty(*id, &type_param_subst),
            None => self.fresh(),
        };

        // Build function type
        let fn_ty = Ty::Fn(param_tys, Box::new(ret_ty));

        // Generalize over type parameter variables for polymorphic functions.
        // NOTE: Type parameter constraints (e.g. `T: Numeric`) are NOT processed
        // here; the `constraints` field is left empty. Constraint handling is
        // deferred to Pass 2 when `stmt()` processes the full function definition
        // and emits constraint-checking constraints during body inference.
        let vars: Vec<_> = type_param_vars.iter().map(|(_, tv)| *tv).collect();
        let scheme = Scheme {
            vars,
            ty: fn_ty,
            constraints: smallvec::SmallVec::new(),
        };
        self.env.bind(name, scheme);
    }

    /// Hoist a module declaration and its members.
    ///
    /// Registers the module name and hoists all function members with
    /// provisional types. Nested modules are processed recursively.
    fn hoist_module(&mut self, mod_path: &str, body: &[StmtId], span: Span) {
        // Register the module name first
        self.env.register_user_module(mod_path);

        // Hoist module members
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

                Some(Stmt::Module { ref name, ref body }) => {
                    // Nested module; recurse with qualified path
                    let nested_path = format!("{}.{}", mod_path, name);
                    self.hoist_module(&nested_path, body, item_span);
                }

                // TYPE/UNION/NEWTYPE are processed by registry; skip
                // Other statements are invalid in modules (caught in Pass 2)
                _ => {}
            }
        });
    }

    /// Hoist a class instance declaration.
    ///
    /// Registers the instance in `instance_registry` so that class method
    /// calls can find user instances even when the CLASS statement appears
    /// after the call site (forward reference).
    #[allow(clippy::too_many_arguments)]
    fn hoist_class_instance(
        &mut self,
        class_name: &str,
        class_args: &SmallVec<[AstTypeExprId; 2]>,
        type_params: &SmallVec<[TypeParam; 2]>,
        for_type: AstTypeExprId,
        constraints: &SmallVec<[(String, SmallVec<[ast::Class; 2]>); 2]>,
        methods: &SmallVec<[InstanceMethodDef; 4]>,
        span: Span,
    ) {
        // Parse class name; silently skip if invalid (error in Pass 2)
        let Some(class) = ClassKind::from_str(class_name) else {
            return;
        };

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

        // Convert class args (needed for builtin check)
        let class_arg_tys: SmallVec<[Ty; 2]> = class_args
            .iter()
            .map(|id| self.ast_type_to_ty(*id, &type_param_subst))
            .collect();

        // Extract TypeId; for primitives, use primitive_type_id
        let type_id = match &for_ty {
            Ty::Named(id, _) => *id,
            _ => match self.primitive_type_id(&for_ty) {
                Some(id) => id,
                None => return, // Skip if we can't get a TypeId
            },
        };

        // Check if this is a forbidden builtin instance (same logic as stmt.rs)
        // Allow if any class arg is a user-defined type
        if self.is_builtin_type(type_id) {
            let all_args_builtin = class_arg_tys.is_empty()
                || class_arg_tys.iter().all(|ty| self.is_builtin_ty(ty));
            if all_args_builtin {
                return; // Skip; error will be emitted in Pass 2
            }
        }

        // Process constraints
        let mut scheme_constraints: SmallVec<[(TyVar, Class); 2]> =
            SmallVec::new();
        constraints
            .iter()
            .for_each(|(param_name, param_constraints)| {
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
                        self.ast_class_to_ty_class(c, &type_param_subst),
                    ));
                });
            });

        // Build method map (empty for hoisting; filled in Pass 2)
        let method_map: HashMap<_, _> = methods
            .iter()
            .map(|m| {
                let method_id = self.env.intern(&m.name);
                // Generate function name for consistency
                let type_name = self.extract_type_name_from_ast(for_type);
                let fn_name = crate::interpreter::instance::instance_fn_name(
                    class, &type_name, &m.name,
                );
                let fn_name_id = self.env.intern(&fn_name);
                (method_id, fn_name_id)
            })
            .collect();

        // Extract type params as TyVars
        let type_var_params: SmallVec<[TyVar; 2]> = if type_params.is_empty() {
            constraints
                .iter()
                .filter_map(|(name, _)| {
                    let id = self.env.intern(name);
                    type_param_subst.get(&id).and_then(|ty| match ty {
                        Ty::Var(v) => Some(*v),
                        _ => None,
                    })
                })
                .collect()
        } else {
            type_params
                .iter()
                .filter_map(|tp| {
                    let id = self.env.intern(&tp.name);
                    type_param_subst.get(&id).and_then(|ty| match ty {
                        Ty::Var(v) => Some(*v),
                        _ => None,
                    })
                })
                .collect()
        };

        // Register instance (ignore duplicate errors; caught in Pass 2)
        // TODO: Phase 4 will pass `module` parameter to this function; top-level
        // instances will remain `None`, module-scoped instances will be `Some(path)`.
        let inst = Instance {
            class,
            class_args: class_arg_tys,
            type_params: type_var_params,
            constraints: scheme_constraints,
            methods: method_map,
            assoc_types: SmallVec::new(),
            module: None,
            span,
        };
        if let Err(e) = self.instance_registry.register(type_id, inst) {
            self.error(e);
        }
    }
}
