//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::HashMap;

use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{
    ArrayElem, AstTypeExprId, BindingPattern, DbRef, Expr, ExprId,
    OutputFormat, OutputStmt, OutputTarget, Stmt, StmtId, SubscriptElem,
    TypeParam, UserConstraint,
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
            Some(Stmt::Fun {
                name,
                type_params,
                params,
                ret,
                body,
            }) => {
                self.fun(&name, &type_params, &params, ret.as_ref(), body, span)
            }

            Some(Stmt::Let(pattern, ann, rhs)) => {
                self.r#let(&pattern, ann.as_ref(), rhs, span)
            }

            Some(Stmt::Set(ref dbref, value)) => self.set(dbref, value, span),

            Some(Stmt::Kill(ref dbref)) => self.kill(dbref, span),

            Some(Stmt::Output(output)) => self.output(&output, span),

            Some(Stmt::Expr(expr)) => {
                self.expr(expr);
            }

            Some(Stmt::Type { .. }) => {
                // Type declarations are processed by the registry; nothing to
                // infer here. The types are registered before type checking.
            }

            Some(Stmt::Union { .. }) => {
                // Union declarations are processed by the registry; nothing to
                // infer here. The unions are registered before type checking.
            }

            Some(Stmt::Module { name, body }) => {
                self.user_module_with_path(&name, &body, span)
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
                Some(Stmt::Fun { ref name, .. }) => {
                    // Typecheck the function (binds it in current scope)
                    self.stmt(id);
                    // Register as module member
                    if let Some(scheme) = self.env.lookup(name).cloned() {
                        self.env.register_user_module_member(
                            mod_path, name, scheme,
                        );
                    }
                }

                Some(Stmt::Let(ref pat, ..)) => {
                    // Module constants must be simple bindings (not destructuring)
                    match pat {
                        BindingPattern::Var(ref const_name) => {
                            self.stmt(id);
                            // Register as module member
                            if let Some(scheme) =
                                self.env.lookup(const_name).cloned()
                            {
                                self.env.register_user_module_member(
                                    mod_path, const_name, scheme,
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
                Some(Stmt::Output(..)) => {
                    self.error(TypeError::Custom {
                        msg: "`OUTPUT` is not allowed inside a module"
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
                Some(Stmt::Type { .. }) => {
                    // Type declarations are processed by the registry with
                    // qualified names (e.g., `ModuleName.TypeName`); nothing
                    // to infer here.
                }
                Some(Stmt::Union { .. }) => {
                    // Union declarations are processed by the registry with
                    // qualified names (e.g., `ModuleName.UnionName`); nothing
                    // to infer here.
                }
                None => {}
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
            [(TyVar, UserConstraint, Option<TyVar>); 2],
        > = SmallVec::new();

        type_params.iter().for_each(|tp| {
            let tv = name_to_tv[tp.name.as_str()];
            let ty = Ty::Var(tv);

            tp.constraints.iter().for_each(|c| {
                // Resolve element type name for Iterable[T]
                let elem_tv = match c {
                    UserConstraint::Iterable(Some(el)) => {
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
                    UserConstraint::Numeric => {
                        Constraint::Numeric(ty.clone(), span)
                    }
                    UserConstraint::Stringable => {
                        Constraint::Stringable(ty.clone(), span)
                    }
                    UserConstraint::Jsonable => {
                        Constraint::Jsonable(ty.clone(), span)
                    }
                    UserConstraint::Subscriptable => {
                        Constraint::Subscriptable(ty.clone(), span)
                    }
                    UserConstraint::Storable => {
                        Constraint::Storable(ty.clone(), span)
                    }
                    UserConstraint::Iterable(_) => {
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

                // Try special case: array literal with `Array[UnionType]`
                let special = match (&ann_ty, self.ast.get_expr(rhs)) {
                    (Ty::Array(elem_ty), Some(Expr::Array(elems)))
                        if self.expand_union_members(elem_ty).is_some() =>
                    {
                        let result =
                            self.array_with_expected(elems, elem_ty, span);
                        self.record_type(rhs, result.clone());
                        self.bind_pattern(pattern, &result, span);
                        true
                    }
                    _ => false,
                };

                if special {
                    None // Already bound
                } else {
                    let rhs_ty = self.expr(rhs);
                    self.unify(rhs_ty.clone(), ann_ty.clone(), span);

                    // Extensible records: if rhs is an object and annotation
                    // is a struct, keep the full object type to preserve extra
                    // fields
                    let is_struct = matches!(&ann_ty, Ty::Named(id, _)
                        if matches!(self.registry.get_def(*id), Some(TypeDef::Struct { .. })));

                    Some(
                        if matches!(&rhs_ty, Ty::Object(_)) && is_struct {
                            rhs_ty
                        } else {
                            ann_ty
                        },
                    )
                }
            }
        };

        // Bind variables from the pattern (if not already done)
        if let Some(ty) = ty {
            self.bind_pattern(pattern, &ty, span);
        }
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

            BindingPattern::Array(pats, rest) => {
                let elem_ty = match ty {
                    Ty::Array(e) => e.as_ref().clone(),
                    Ty::Var(_) => {
                        let fresh = self.fresh();
                        self.unify(
                            ty.clone(),
                            Ty::Array(Box::new(fresh.clone())),
                            span,
                        );
                        fresh
                    }
                    Ty::Error => Ty::Error,
                    _ => {
                        self.error(TypeError::NotIndexable(ty.clone(), span));
                        Ty::Error
                    }
                };

                // Bind each fixed-position pattern
                pats.iter()
                    .for_each(|p| self.bind_pattern(p, &elem_ty, span));

                // Bind rest pattern if present
                if let Some(crate::ast::RestPattern::Bind(name)) = rest {
                    self.env.bind(name, Scheme::mono(ty.clone()));
                }
            }
        }
    }

    /// Infer types for a `SET` statement or expression.
    ///
    /// Type-checks subscript expressions and the value, adding appropriate
    /// constraints. Global writes must be inside a transaction block.
    pub(super) fn set(&mut self, dbref: &DbRef, value: ExprId, span: Span) {
        // Global writes require transaction context
        if matches!(dbref, DbRef::Global(..)) && !self.in_transaction {
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

    /// Infer types for a `KILL` statement or expression.
    ///
    /// Type-checks subscript expressions with `Subscriptable` constraints.
    /// Global kills must be inside a transaction block.
    pub(super) fn kill(&mut self, dbref: &DbRef, span: Span) {
        // Global writes require transaction context
        if matches!(dbref, DbRef::Global(..)) && !self.in_transaction {
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

    /// Infer types for an `OUTPUT` statement or expression.
    ///
    /// Type-checks the expression and adds constraints based on format and target:
    /// - `Stringable` for default format
    /// - `Jsonable` for JSON format
    /// - `FilePath | String` for file target path
    pub(super) fn output(&mut self, output: &OutputStmt, span: Span) {
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
