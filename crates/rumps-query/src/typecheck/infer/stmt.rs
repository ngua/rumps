//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::{HashMap, HashSet};

use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExpr, AstTypeExprId, BindingPattern, DbRef, Expr, ExprId,
    Import, ImportItem, OutputFormat, OutputTarget, ParamConstraint, Stmt,
    StmtId, SubscriptElem, TxnId, TypeParam, UnOp, Visibility, WriteStmt,
};
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty, TyVar};
use crate::value::TypeDef;
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

            Some(Stmt::Set(ref dbref, value, _)) => {
                self.env.mark_non_import();
                self.set_stmt(id, dbref, value, span);
            }

            Some(Stmt::Kill(ref dbref, _)) => {
                self.env.mark_non_import();
                self.kill_stmt(id, dbref, span);
            }

            Some(Stmt::Write(output)) => {
                self.env.mark_non_import();
                self.write(&output, span);
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

                // Invalid statements inside a module
                Some(Stmt::Set(..)) => {
                    self.error(TypeError::Custom {
                        msg: "`SET` is not allowed inside a module".to_string(),
                        span: item_span,
                    });
                }
                Some(Stmt::Kill(..)) => {
                    self.error(TypeError::Custom {
                        msg: "`KILL` is not allowed inside a module"
                            .to_string(),
                        span: item_span,
                    });
                }
                Some(Stmt::Write(..)) => {
                    self.error(TypeError::Custom {
                        msg: "`WRITE` is not allowed inside a module"
                            .to_string(),
                        span: item_span,
                    });
                }
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
        let mut scheme_constraints: SmallVec<
            [(TyVar, ParamConstraint, Option<TyVar>); 2],
        > = SmallVec::new();

        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[tp.name.as_str()];
            let ty = Ty::Var(tv);

            tp.constraints.iter().for_each(|c| {
                // Resolve element type name for Iterable[T]
                let elem_tv = match c {
                    ParamConstraint::Iterable(Some(el)) => {
                        let tv = name_to_tv.get(el.as_str()).copied();
                        if tv.is_none() {
                            self.error(TypeError::Custom {
                                msg: format!(
                                    "unknown type parameter `{el}` in \
                                     constraint `Iterable[{el}]`"
                                ),
                                span,
                            });
                        }
                        tv
                    }
                    _ => None,
                };
                scheme_constraints.push((tv, c.clone(), elem_tv));

                // Emit constraint for checking the function body
                let constraint = match c {
                    ParamConstraint::Numeric => {
                        Constraint::Numeric(ty.clone(), span)
                    }
                    ParamConstraint::Stringable => {
                        Constraint::Stringable(ty.clone(), span)
                    }
                    ParamConstraint::Jsonable => {
                        Constraint::Jsonable(ty.clone(), span)
                    }
                    ParamConstraint::Subscriptable => {
                        Constraint::Subscriptable(ty.clone(), span)
                    }
                    ParamConstraint::Storable => {
                        Constraint::Storable(ty.clone(), span)
                    }
                    ParamConstraint::Iterable(_) => {
                        // Use resolved elem type or fresh var
                        let elem = elem_tv
                            .map(Ty::Var)
                            .unwrap_or_else(|| self.fresh());
                        Constraint::Iterable {
                            coll: ty.clone(),
                            elem,
                            span,
                        }
                    }
                    ParamConstraint::Monoid => {
                        Constraint::Monoid(ty.clone(), span)
                    }
                    ParamConstraint::BitLike => {
                        Constraint::BitLike(ty.clone(), span)
                    }
                };
                self.constrain(constraint);
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
        let vars: Vec<_> = ty_vars.difference(&outer_free).copied().collect();
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

        // Bind variables from the pattern (if not already done)
        if let Some(ty) = ty {
            self.bind_pattern(pattern, &ty, span);
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

        Some(
            if matches!(&rhs_ty, Ty::Object(_)) && is_obj_alias {
                rhs_ty
            } else {
                ann_ty.clone()
            },
        )
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

    /// Validate a `SET` operation (shared by statement and expression forms).
    ///
    /// Type-checks subscript expressions and the value, adding appropriate
    /// constraints. Global writes must be inside a transaction block.
    fn set(&mut self, dbref: &DbRef, value: ExprId, span: Span) {
        // Global writes require transaction context
        if matches!(dbref, DbRef::Global(..)) && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }

        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);

        // Type-check value and add Storable constraint
        let val_ty = self.expr(value);
        self.constrain(Constraint::Storable(val_ty, span));
    }

    /// Infer types for a `SET` statement.
    ///
    /// Calls validation, then populates the `TxnId` field in the AST.
    pub(super) fn set_stmt(
        &mut self,
        id: StmtId,
        dbref: &DbRef,
        value: ExprId,
        span: Span,
    ) {
        self.set(dbref, value, span);
        self.ast
            .set_stmt(id, Stmt::Set(dbref.clone(), value, self.in_transaction));
    }

    /// Infer types for a `@SET` expression.
    ///
    /// Calls validation, then populates the `TxnId` field in the AST.
    pub(super) fn set_expr(
        &mut self,
        id: ExprId,
        dbref: &DbRef,
        value: ExprId,
        span: Span,
    ) {
        self.set(dbref, value, span);
        self.ast
            .set_expr(id, Expr::Set(dbref.clone(), value, self.in_transaction));
    }

    /// Validate a `KILL` operation (shared by statement and expression forms).
    ///
    /// Type-checks subscript expressions with `Subscriptable` constraints.
    /// Global kills must be inside a transaction block.
    fn kill(&mut self, dbref: &DbRef, span: Span) {
        // Global writes require transaction context
        if matches!(dbref, DbRef::Global(..)) && self.in_transaction.is_none() {
            self.error(TypeError::Custom {
                msg: "global writes require a transaction".to_string(),
                span,
            });
        }

        let subs = match dbref {
            DbRef::Local(_, s) | DbRef::Global(_, s) => s,
        };
        self.check_subscript_elems(subs, span);
    }

    /// Infer types for a `KILL` statement.
    ///
    /// Calls validation, then populates the `TxnId` field in the AST.
    pub(super) fn kill_stmt(&mut self, id: StmtId, dbref: &DbRef, span: Span) {
        self.kill(dbref, span);
        self.ast
            .set_stmt(id, Stmt::Kill(dbref.clone(), self.in_transaction));
    }

    /// Infer types for a `@KILL` expression.
    ///
    /// Calls validation, then populates the `TxnId` field in the AST.
    pub(super) fn kill_expr(&mut self, id: ExprId, dbref: &DbRef, span: Span) {
        self.kill(dbref, span);
        self.ast
            .set_expr(id, Expr::Kill(dbref.clone(), self.in_transaction));
    }

    /// Infer types for a `WRITE` statement or expression.
    ///
    /// Type-checks the expression and adds constraints based on format and target:
    /// - `Stringable` for default format
    /// - `Jsonable` for JSON format
    /// - `FilePath | String` for file target path
    pub(super) fn write(&mut self, output: &WriteStmt, span: Span) {
        let expr_ty = self.expr(output.expr);

        // Format constraint
        match output.format {
            OutputFormat::Default => {
                // All types are stringable
                self.constrain(Constraint::Stringable(expr_ty, span));
            }
            OutputFormat::Json => {
                // Must be JSON-convertible (not closures, etc.)
                self.constrain(Constraint::Jsonable(expr_ty, span));
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
}
