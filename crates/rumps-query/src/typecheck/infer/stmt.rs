//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::{HashMap, HashSet};

use smallvec::SmallVec;

use super::{ClassContext, Constraint, InferCtx};
use crate::ast::{
    self, ArrayElem, AssocTypeDef, AstTypeExpr, AstTypeExprId, BindingPattern,
    DbRef, Expr, ExprId, Import, ImportItem, InstanceMethodDef, Literal,
    OutputFormat, OutputTarget, RefTarget, Stmt, StmtId, SubscriptElem, TxnId,
    TypeParam, UnOp, Visibility, WriteExpr,
};
use crate::typecheck::error::TypeError;
use crate::typecheck::instance::{self, Instance};
use crate::typecheck::ty::{Class, ClassKind, Scheme, Ty, TyVar};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Infer types for a statement.
    ///
    /// Most statements don't produce a type, but function definitions
    /// bind the function name with its inferred type scheme in the environment.
    pub(crate) fn stmt(&mut self, id: StmtId) {
        let span = self.ast.stmt_span(id).unwrap_or(Span::new(0, 0));
        let stmt = self.ast.get_stmt(id).cloned();

        match stmt {
            Some(Stmt::Import(ref import)) => {
                // Check import ordering before processing
                if !self.env.imports_allowed() {
                    self.error(TypeError::Custom {
                        msg: "imports must appear at the top of a scope"
                            .to_string(),
                        span,
                    });
                }
                self.import_stmt(import, span);
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
                self.fun(
                    &name,
                    &type_params,
                    &params,
                    ret.as_ref(),
                    body,
                    span,
                );
            }

            Some(Stmt::Let(pattern, ann, rhs, _)) => {
                self.env.mark_non_import();
                self.r#let(&pattern, ann.as_ref(), rhs, span);
            }

            Some(Stmt::Expr(expr)) => {
                self.env.mark_non_import();
                self.expr(expr);
            }

            Some(Stmt::Type { .. }) => {
                self.env.mark_non_import();
                // Type declarations are processed by the registry; nothing to
                // infer here. The types are registered before type checking.
            }

            Some(Stmt::Union { .. }) => {
                self.env.mark_non_import();
                // Union declarations are processed by the registry; nothing to
                // infer here. The unions are registered before type checking.
            }

            Some(Stmt::NewType { .. }) => {
                self.env.mark_non_import();
                // NewType declarations are processed by the registry; nothing
                // to infer here. The type aliases are registered before type
                // checking.
            }

            Some(Stmt::Module { name, body }) => {
                self.env.mark_non_import();
                self.user_module_with_path(&name, &body, span);
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
                self.class_instance(
                    &class_name,
                    &class_args,
                    &type_params,
                    for_type,
                    &constraints,
                    &assoc_types,
                    &methods,
                    span,
                );
            }

            None => {}
        }
    }

    /// Infer types for a top-level user-defined module.
    ///
    /// Delegates to `user_module` with an empty path prefix.
    fn user_module_with_path(
        &mut self,
        mod_name: &str,
        body: &[StmtId],
        span: Span,
    ) {
        self.user_module(mod_name, body, span)
    }

    /// Infer types for a user-defined module.
    ///
    /// Validates that only `FUN`, `LET`, and nested `MODULE` statements appear
    /// inside, typechecks each item, and registers the module's types so they
    /// can be accessed via `ModuleName.fn(...)` or `ModuleName.const`.
    ///
    /// The `mod_path` is the fully-qualified module path (e.g., `"Outer.Inner"`
    /// for a nested module).
    fn user_module(&mut self, mod_path: &str, body: &[StmtId], span: Span) {
        // Register the module name FIRST so self-references like
        // `Geometry.pi` from within `Geometry.area` resolve correctly.
        self.env.register_user_module(mod_path);

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
                    if let Some(scheme) = self.env.lookup(name).cloned() {
                        self.env.register_user_module_member(
                            mod_path, name, scheme, vis,
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
                                self.env.lookup(const_name).cloned()
                            {
                                self.env.register_user_module_member(
                                    mod_path, const_name, scheme, vis,
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
                    let nested_path = format!("{}.{}", mod_path, name);
                    self.user_module(&nested_path, body, item_span);
                }

                // Invalid statements inside a module (SET/KILL/WRITE are now
                // `Stmt::Expr` wrapping their expression forms)
                Some(Stmt::Expr(..)) => {
                    self.error(TypeError::Custom {
                        msg: "expression statements are not allowed inside a \
                              module"
                            .to_string(),
                        span: item_span,
                    });
                }
                Some(Stmt::Type { ref name, vis, .. }) => {
                    // Type declarations are processed by the registry with
                    // qualified names; register visibility for access checks.
                    let qname = format!("{}.{}", mod_path, name);
                    self.env.register_user_module_type_vis(&qname, vis);
                }
                Some(Stmt::Union { ref name, vis, .. }) => {
                    // Union declarations are processed by the registry with
                    // qualified names; register visibility for access checks.
                    let qname = format!("{}.{}", mod_path, name);
                    self.env.register_user_module_type_vis(&qname, vis);
                }
                Some(Stmt::NewType { ref name, vis, .. }) => {
                    // NewType declarations are processed by the registry with
                    // qualified names; register visibility for access checks.
                    let qname = format!("{}.{}", mod_path, name);
                    self.env.register_user_module_type_vis(&qname, vis);
                }
                Some(Stmt::Import(ref import)) => {
                    // Process import inside module
                    self.import_stmt(import, item_span);
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
                    self.class_instance(
                        class_name,
                        class_args,
                        type_params,
                        for_type,
                        constraints,
                        assoc_types,
                        methods,
                        item_span,
                    );
                }
                None => {}
            }
        });
    }

    /// Process an import statement.
    ///
    /// Validates module and member existence, checks visibility, and binds
    /// imported names in the current scope with their types.
    fn import_stmt(&mut self, import: &Import, span: Span) {
        let mod_path = import.path.join(".");
        let path_segs: Vec<&str> =
            import.path.iter().map(String::as_str).collect();

        // Check if module exists (builtin or user-defined)
        let is_builtin = path_segs
            .first()
            .is_some_and(|&name| self.runtime_env.is_builtin_module(name));
        let is_user = self.env.is_user_module(&mod_path);

        if !is_builtin && !is_user {
            self.error(TypeError::Custom {
                msg: format!("unknown module `{}`", mod_path),
                span,
            });
            // Continue to gather more errors
        }

        // Collect exclusions and check for wildcard
        let mut has_wildcard = false;
        let mut exclusions = HashSet::new();

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
                exclusions.insert(name.as_str());
            }
            ImportItem::Named { .. } => {}
        });

        // Process wildcard: import all public members
        if has_wildcard {
            // Get public members from builtin module
            if is_builtin {
                if let Some(m) = path_segs
                    .first()
                    .and_then(|&name| self.runtime_env.get_builtin_module(name))
                {
                    m.public_members()
                        .into_iter()
                        .filter(|(name, _)| !exclusions.contains(name.as_str()))
                        .for_each(|(name, scheme)| {
                            self.env.bind(&name, scheme);
                        });
                }
            }
            // Get public members from user module
            if is_user {
                self.env
                    .get_public_user_module_members(&mod_path)
                    .into_iter()
                    .filter(|(name, _)| !exclusions.contains(name.as_str()))
                    .for_each(|(name, scheme)| {
                        self.env.bind(&name, scheme);
                    });
            }
        }

        // Process named imports
        import.items.iter().for_each(|item| {
            if let ImportItem::Named { name, alias } = item {
                let bind_name = alias.as_deref().unwrap_or(name);
                let mut full_path = path_segs.clone();
                full_path.push(name);

                // Try builtin module first (always public)
                let builtin = self
                    .runtime_env
                    .get_module_fn_type(&full_path)
                    .cloned()
                    .or_else(|| {
                        self.runtime_env
                            .get_module_const_type(&full_path)
                            .cloned()
                            .map(Scheme::mono)
                    });

                // Then try user module (check visibility)
                let user = self.env.lookup_user_module_member(&full_path);

                match (builtin, user) {
                    (Some(s), _) => self.env.bind(bind_name, s),
                    (None, Some(m)) if m.vis == Visibility::Public => {
                        self.env.bind(bind_name, m.scheme.clone());
                    }
                    (None, Some(_)) => {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "member `{}` is private in module `{}`",
                                name, mod_path
                            ),
                            span,
                        });
                    }
                    (None, None) => {
                        self.error(TypeError::Custom {
                            msg: format!(
                                "member `{}` not found in module `{}`",
                                name, mod_path
                            ),
                            span,
                        });
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
        name: &str,
        type_params: &SmallVec<[TypeParam; 2]>,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) {
        // Capture outer env free vars BEFORE binding function (for generalization)
        let outer_free = self.env.free_vars();

        // First pass: create fresh type variables for all type parameters
        let name_to_tv: HashMap<&str, TyVar> = type_params
            .iter()
            .map(|tp| (tp.name.as_str(), self.fresh_var()))
            .collect();

        // Build type_param_subst (StringId -> Ty) for type resolution
        let type_param_subst: HashMap<_, _> = type_params
            .iter()
            .map(|tp| {
                let id = self.env.intern(&tp.name);
                let tv = name_to_tv[tp.name.as_str()];
                (id, Ty::Var(tv))
            })
            .collect();

        // Second pass: process constraints now that all type params are known
        // Convert ast::Class to ty::Class for storage in Scheme
        let mut scheme_constraints: SmallVec<[(TyVar, Class); 2]> =
            SmallVec::new();

        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[tp.name.as_str()];
            let ty = Ty::Var(tv);

            tp.constraints.iter().for_each(|c| {
                let class = self.ast_class_to_ty_class(c, &type_param_subst);
                scheme_constraints.push((tv, class.clone()));

                // Emit constraint for checking the function body
                self.constrain(Constraint::Class {
                    ty: ty.clone(),
                    class,
                    span,
                });
            });
        });

        // Infer parameter types (using type param substitution)
        let param_tys = self.param_tys_with_subst(params, &type_param_subst);

        // Declared return type annotation (if any)
        let declared_ret =
            ret.map(|id| self.ast_type_to_ty(*id, &type_param_subst));

        // Fresh var for provisional return (supports recursive calls)
        let provisional_ret = self.fresh();
        let provisional_fn =
            Ty::Fn(param_tys.clone(), Box::new(provisional_ret.clone()));
        self.env.bind(name, Scheme::mono(provisional_fn));

        self.env.push_scope();
        self.bind_params(params, &param_tys);

        // Infer body type
        let body_ty = self.expr(body);

        // Pop parameter scope
        self.env.pop_scope();

        // Determine actual return type: use annotation if present, else body type
        let actual_ret = match declared_ret {
            Some(ret_ty) => {
                self.unify(body_ty.clone(), ret_ty.clone(), span);
                ret_ty
            }
            None => body_ty,
        };

        // Link provisional return var with actual (for recursive call consistency)
        self.unify(provisional_ret, actual_ret.clone(), span);

        // Build final function type and generalize
        let fn_ty = Ty::Fn(param_tys, Box::new(actual_ret));
        let ty_vars = fn_ty.free_vars();
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

    /// Infer types for a `LET` statement.
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
                let ann_ty = self.ast_type_to_ty(*id, &HashMap::new());

                // Clone to avoid borrow issues with mutable self
                let rhs_expr = self.ast.get_expr(rhs).cloned();

                // Reject negative literals for Word type
                if let (Ty::Word, Some(Expr::Unary(UnOp::Neg, _))) =
                    (&ann_ty, &rhs_expr)
                {
                    self.error(TypeError::NegativeWord(span));
                    self.expr(rhs);
                    self.bind_pattern(pattern, &Ty::Word, span);
                    None
                // Try special case: array literal with `Array[UnionType]`
                } else if let (Ty::Array(elem_ty), Some(Expr::Array(elems))) =
                    (&ann_ty, &rhs_expr)
                {
                    if self.expand_union_members(elem_ty).is_some() {
                        let result =
                            self.array_with_expected(elems, elem_ty, span);
                        self.record_type(rhs, result.clone());
                        self.bind_pattern(pattern, &result, span);
                        None // Already bound
                    } else {
                        self.infer_default_let(&ann_ty, rhs, span)
                    }
                } else {
                    self.infer_default_let(&ann_ty, rhs, span)
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
                    self.env.bind(name, scheme);
                }
                _ => self.bind_pattern(pattern, &ty, span),
            }
        }
    }

    /// Default inference for `LET` with type annotation.
    fn infer_default_let(
        &mut self,
        ann_ty: &Ty,
        rhs: ExprId,
        span: Span,
    ) -> Option<Ty> {
        let rhs_ty = self.expr(rhs);
        self.unify(rhs_ty.clone(), ann_ty.clone(), span);

        // Extensible records: if rhs is an object and annotation
        // is an alias to object, keep the full object type to
        // preserve extra fields
        let is_obj_alias = matches!(ann_ty, Ty::Named(id, _)
        if self.registry.get_def(*id).is_some_and(|def| match def {
            TypeDef::Alias { target, .. } => self
                .ast
                .get_type_expr(*target)
                .is_some_and(|te| matches!(te, AstTypeExpr::Object(_))),
            _ => false,
        }));

        // Preserve concrete type for:
        // 1. Extensible records (object rhs with object alias annotation)
        // 2. Ref types (Local/Global rhs with Ref union annotation)
        let use_rhs_ty = (matches!(&rhs_ty, Ty::Object(_)) && is_obj_alias)
            || (matches!(&rhs_ty, Ty::Local | Ty::Global)
                && matches!(ann_ty, Ty::Named(id, _) if *id == TypeId::REF));

        Some(if use_rhs_ty { rhs_ty } else { ann_ty.clone() })
    }

    /// Bind variables from a binding pattern to types in the environment.
    ///
    /// Recursively descends into the pattern, extracting types from the
    /// value type and binding each variable with the appropriate type.
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &BindingPattern,
        ty: &Ty,
        span: Span,
    ) {
        match pattern {
            BindingPattern::Var(name) => {
                self.env.bind(name, Scheme::mono(ty.clone()));
            }

            BindingPattern::Wildcard => {
                // No binding
            }

            BindingPattern::Tuple(pats) => {
                let elem_tys = match ty {
                    Ty::Tuple(ts) => {
                        if ts.len() != pats.len() {
                            self.error(TypeError::ArityMismatch {
                                expected: pats.len(),
                                got: ts.len(),
                                span,
                            });
                            vec![Ty::Error; pats.len()]
                        } else {
                            ts.clone()
                        }
                    }
                    Ty::Var(_) => {
                        // Create fresh vars for each element and constrain
                        let fresh: Vec<_> =
                            (0..pats.len()).map(|_| self.fresh()).collect();
                        self.unify(ty.clone(), Ty::Tuple(fresh.clone()), span);
                        fresh
                    }
                    Ty::Error => vec![Ty::Error; pats.len()],
                    _ => {
                        self.error(TypeError::NotATuple(ty.clone(), span));
                        vec![Ty::Error; pats.len()]
                    }
                };
                pats.iter()
                    .zip(elem_tys.iter())
                    .for_each(|(p, t)| self.bind_pattern(p, t, span));
            }

            BindingPattern::Object(fields) => {
                fields.iter().for_each(|(name, sub)| {
                    let fty = self.field_type(ty, name, span);
                    self.bind_pattern(sub, &fty, span);
                });
            }

            BindingPattern::Array(_, _) => {
                // Array destructuring is only allowed in MATCH expressions
                self.error(TypeError::ArrayPatternInLet(span));
            }
        }
    }

    /// Validate a `SET` operation with a resolved `RefTarget`.
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
                self.expr_types
                    .get(e)
                    .is_none_or(|ty| matches!(ty, Ty::Global))
            }
        };
        if needs_txn && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }
    }

    /// Validate a `KILL` operation with a resolved `RefTarget`.
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
                self.expr_types
                    .get(e)
                    .is_none_or(|ty| matches!(ty, Ty::Global))
            }
        };
        if needs_txn && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }
    }

    /// Infer types for a `WRITE` statement or expression.
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
                    class: Class::Into(Ty::String),
                    span,
                });
            }
            OutputFormat::Json => {
                // Must be convertible to Json
                self.constrain(Constraint::Class {
                    ty: expr_ty,
                    class: Class::Into(Ty::Json),
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
                let union_ty = Ty::Union(vec![Ty::FilePath, Ty::String]);
                self.unify(path_ty, union_ty, span);
            }
        }
    }

    /// Infer types for a `CLASS ... FOR ...` instance declaration.
    ///
    /// Validates:
    /// 1. The class name is a valid `ClassKind`
    /// 2. The `for_type` is NOT a builtin type
    /// 3. All required methods are present
    /// 4. Method signatures match the class definition (arity)
    /// 5. Method bodies typecheck correctly
    ///
    /// Registers the instance in `InstanceRegistry` on success.
    #[allow(clippy::too_many_arguments)]
    fn class_instance(
        &mut self,
        class_name: &str,
        class_args: &SmallVec<[AstTypeExprId; 2]>,
        type_params: &SmallVec<[TypeParam; 2]>,
        for_type: AstTypeExprId,
        constraints: &SmallVec<[(String, SmallVec<[ast::Class; 2]>); 2]>,
        assoc_types: &SmallVec<[AssocTypeDef; 2]>,
        methods: &SmallVec<[InstanceMethodDef; 4]>,
        span: Span,
    ) {
        // 1. Resolve class name to ClassKind
        let class = ClassKind::from_str(class_name).unwrap_or_else(|| {
            self.error(TypeError::UnknownClass(class_name.to_string(), span));
            ClassKind::Display // Default to `Display` to avoid cascading errors
        });

        // 2. Build type parameter substitution map (BEFORE resolving for_type)
        //    If `type_params` is empty, extract type param names from the
        //    WHERE constraints (e.g., `L: Display, R: Display` gives `[L, R]`)
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

        // 3. Resolve for_type and get its TypeId
        let for_ty = self.ast_type_to_ty(for_type, &type_param_subst);
        let type_id = self.extract_type_id(&for_ty);

        // 4. Convert class args to Ty (needed for builtin check)
        let class_arg_tys: SmallVec<[Ty; 2]> = class_args
            .iter()
            .map(|id| self.ast_type_to_ty(*id, &type_param_subst))
            .collect();

        // 5. Check for forbidden builtin instance
        //
        // We allow implementing classes for builtin types IF the class has
        // type args that include user-defined types. For example:
        //   - `CLASS Display FOR Int` is forbidden (builtin has Display)
        //   - `CLASS Into[String] FOR Int` is forbidden (builtin has Into[String])
        //   - `CLASS Into[UserId] FOR Int` is ALLOWED (no builtin Into[UserId])
        //
        // The heuristic: if for_type is builtin AND all class args are builtin,
        // reject. If any class arg is a user type, we allow it.
        if let Some(tid) = type_id {
            if self.is_builtin_type(tid) {
                let all_args_builtin = class_arg_tys.is_empty()
                    || class_arg_tys.iter().all(|ty| self.is_builtin_ty(ty));
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

        // 6.5. Process associated type definitions and set class context
        //
        // This enables bare `:Index` references inside method bodies to resolve
        // to the concrete type defined in this instance.
        let assoc_type_map: HashMap<_, _> = assoc_types
            .iter()
            .map(|def| {
                let name_id = self.env.intern(&def.name);
                let ty = self.ast_type_to_ty(def.target, &type_param_subst);
                (name_id, ty)
            })
            .collect();

        // 6.6. Validate all required associated types are provided
        class.def().assoc_types.iter().for_each(|req| {
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
            assoc_types: assoc_type_map.clone(),
        });

        // 7. Collect provided method names
        let provided_methods: HashSet<&str> =
            methods.iter().map(|m| m.name.as_str()).collect();

        // 8. Check all required methods are present
        class.required_methods().iter().for_each(|req| {
            if !provided_methods.contains(req) {
                self.error(TypeError::MissingInstanceMethod {
                    class,
                    method: req.to_string(),
                    span,
                });
            }
        });

        // 9. Typecheck each method
        methods.iter().for_each(|m| {
            self.instance_method(
                class,
                &for_ty,
                &class_arg_tys,
                &type_param_subst,
                m,
                span,
            );
        });

        // 9.5. Clear class context after method processing
        self.class_context = None;

        // 10. Register instance (if we have a valid type_id)
        let type_name = self.extract_type_name_from_ast(for_type);
        if let Some(tid) = type_id {
            let method_map: HashMap<_, _> = methods
                .iter()
                .map(|m| {
                    let method_id = self.env.intern(&m.name);
                    let fn_name =
                        crate::interpreter::instance::instance_fn_name(
                            class, &type_name, &m.name,
                        );
                    let fn_name_id = self.env.intern(&fn_name);
                    (method_id, fn_name_id)
                })
                .collect();

            let type_var_params: SmallVec<[TyVar; 2]> = type_params
                .iter()
                .filter_map(|tp| {
                    let id = self.env.intern(&tp.name);
                    type_param_subst.get(&id).and_then(|ty| match ty {
                        Ty::Var(v) => Some(*v),
                        _ => None,
                    })
                })
                .collect();

            // Skip registration if already hoisted (avoid duplicate error)
            if self.instance_registry.lookup(class, tid).is_none() {
                // Convert AST associated types to instance associated types
                let inst_assoc_types: SmallVec<[instance::AssocTypeDef; 1]> =
                    assoc_types
                        .iter()
                        .map(|def| {
                            let name_id = self.env.intern(&def.name);
                            let ty = assoc_type_map
                                .get(&name_id)
                                .cloned()
                                .unwrap_or(Ty::Unknown);
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
                                name: name_id,
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
    #[allow(clippy::too_many_arguments)]
    fn instance_method(
        &mut self,
        class: ClassKind,
        for_ty: &Ty,
        class_arg_tys: &SmallVec<[Ty; 2]>,
        type_param_subst: &HashMap<crate::intern::StringId, Ty>,
        method: &InstanceMethodDef,
        inst_span: Span,
    ) {
        let m_span = method.span;

        // Get expected method signature from class
        let expected =
            class.method(&method.name, m_span, |s| self.env.intern(s));

        // Handle unknown method error
        let (expected_param_tys, expected_ret_ty) = expected
            .map(|spec| {
                let scheme = spec.scheme();
                // The first quantified var represents `Self` in class methods.
                // Subsequent vars represent class type parameters (e.g., `U` in
                // `Into[U]`). We substitute both `Self` and class arg types.
                let self_var = scheme.vars.first().copied();
                let class_arg_vars: Vec<_> =
                    scheme.vars.iter().skip(1).copied().collect();
                // Extract param and return types, substituting vars
                match &scheme.ty {
                    Ty::Fn(params, ret) => {
                        let subst = |ty: &Ty| {
                            let mut result =
                                self.subst_self_type(ty, self_var, for_ty);
                            // Substitute class arg type vars
                            class_arg_vars
                                .iter()
                                .zip(class_arg_tys.iter())
                                .for_each(|(var, arg_ty)| {
                                    result =
                                        self.subst_tyvar(&result, *var, arg_ty);
                                });
                            result
                        };
                        (
                            params.iter().map(subst).collect::<Vec<_>>(),
                            subst(ret),
                        )
                    }
                    _ => (vec![], Ty::Unknown),
                }
            })
            .unwrap_or_else(|e| {
                self.error(e);
                (vec![], Ty::Unknown)
            });

        // Check arity
        if method.params.len() != expected_param_tys.len() {
            self.error(TypeError::MethodSignatureMismatch {
                class,
                method: method.name.clone(),
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
            |(user_ty, exp_ty)| {
                self.unify(user_ty.clone(), exp_ty.clone(), m_span);
            },
        );

        self.bind_params(&method.params, &param_tys);

        // Infer body type
        let body_ty = self.expr(method.body);

        // Determine expected return type (user annotation or class signature)
        let ret_ty = method
            .ret
            .map(|ret_id| self.ast_type_to_ty(ret_id, type_param_subst))
            .unwrap_or_else(|| expected_ret_ty.clone());

        // Unify body with return type
        self.unify(body_ty.clone(), ret_ty.clone(), m_span);

        // Also unify with class's expected return type (catches wrong annotation)
        if !matches!(expected_ret_ty, Ty::Unknown) {
            self.unify(ret_ty, expected_ret_ty, inst_span);
        }

        self.env.pop_scope();
    }

    /// Extract a `TypeId` from a `Ty`, if it represents a named/aliased type.
    fn extract_type_id(&self, ty: &Ty) -> Option<TypeId> {
        match ty {
            Ty::Named(id, _) => Some(*id),
            Ty::Unknown | Ty::Error => None,
            // For primitive types, look up by name
            _ => self.primitive_type_id(ty),
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
                AstTypeExpr::Named(name) => Some(name.clone()),
                AstTypeExpr::App(name, _) => Some(name.clone()),
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
    pub(super) fn is_builtin_ty(&self, ty: &Ty) -> bool {
        match ty {
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
            Ty::Named(id, _) => self.is_builtin_type(*id),
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
        &self,
        ty: &Ty,
        self_var: Option<TyVar>,
        for_ty: &Ty,
    ) -> Ty {
        self_var
            .map_or_else(|| ty.clone(), |sv| self.subst_tyvar(ty, sv, for_ty))
    }

    /// Recursively substitute a type variable with a concrete type.
    fn subst_tyvar(&self, ty: &Ty, var: TyVar, replacement: &Ty) -> Ty {
        match ty {
            Ty::Var(v) if *v == var => replacement.clone(),
            Ty::Var(_) => ty.clone(),
            Ty::Fn(params, ret) => Ty::Fn(
                params
                    .iter()
                    .map(|p| self.subst_tyvar(p, var, replacement))
                    .collect(),
                Box::new(self.subst_tyvar(ret, var, replacement)),
            ),
            Ty::Array(elem) => {
                Ty::Array(Box::new(self.subst_tyvar(elem, var, replacement)))
            }
            Ty::Map(k, v) => Ty::Map(
                Box::new(self.subst_tyvar(k, var, replacement)),
                Box::new(self.subst_tyvar(v, var, replacement)),
            ),
            Ty::Tuple(elems) => Ty::Tuple(
                elems
                    .iter()
                    .map(|e| self.subst_tyvar(e, var, replacement))
                    .collect(),
            ),
            Ty::Option(inner) => {
                Ty::Option(Box::new(self.subst_tyvar(inner, var, replacement)))
            }
            Ty::Result(ok, err) => Ty::Result(
                Box::new(self.subst_tyvar(ok, var, replacement)),
                Box::new(self.subst_tyvar(err, var, replacement)),
            ),
            Ty::Named(id, args) => Ty::Named(
                *id,
                args.iter()
                    .map(|a| self.subst_tyvar(a, var, replacement))
                    .collect(),
            ),
            // Primitives and other non-parametric types pass through
            _ => ty.clone(),
        }
    }
}
