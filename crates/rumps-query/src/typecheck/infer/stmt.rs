//! Statement type inference.
//!
//! Contains methods for inferring types from statements: let bindings,
//! function definitions, assignments, etc.

use std::collections::HashMap;

use smallvec::SmallVec;

use super::{Constraint, InferCtx};
use crate::ast::{AstTypeExprId, BindingPattern, Expr, ExprId, Stmt, StmtId};
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty};
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
                params,
                ret,
                body,
            }) => self.fun(&name, &params, ret.as_ref(), body, span),

            Some(Stmt::Let(pattern, ann, rhs)) => {
                self.r#let(&pattern, ann.as_ref(), rhs, span)
            }

            Some(Stmt::Set(target, value)) => self.set(target, value, span),

            Some(Stmt::Kill(target)) => self.kill(target, span),

            Some(Stmt::Output(expr)) => self.output(expr, span),

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

            None => {}
        }
    }

    /// Infer type of a named function definition.
    ///
    /// Named functions support recursion: the function name is bound with a
    /// provisional type (fresh vars for params/return) before inferring the body.
    /// After inference, the type is generalized and the binding is updated.
    fn fun(
        &mut self,
        name: &str,
        params: &SmallVec<[(String, Option<AstTypeExprId>); 4]>,
        ret: Option<&AstTypeExprId>,
        body: ExprId,
        span: Span,
    ) {
        // Capture outer env free vars BEFORE binding function (for generalization)
        let outer_free = self.env.free_vars();

        let param_tys = self.param_tys(params);

        // Declared return type annotation (if any)
        let declared_ret =
            ret.map(|id| self.ast_type_to_ty(*id, &HashMap::new()));

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
        let scheme = Scheme { vars, ty: fn_ty };
        self.env.bind(name, scheme);
    }

    /// Infer types for a `LET` statement.
    ///
    /// Infers the RHS type, optionally unifies with an annotation, then
    /// binds variables from the pattern with appropriate types.
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
        let rhs_ty = self.expr(rhs);

        // If annotation present, parse and unify
        let ty = match ann {
            None => rhs_ty,
            Some(id) => {
                let ann_ty = self.ast_type_to_ty(*id, &HashMap::new());
                self.unify(rhs_ty.clone(), ann_ty.clone(), span);

                // Extensible records: if rhs is an object and annotation is a
                // struct, keep the full object type to preserve extra fields
                let is_struct = matches!(&ann_ty, Ty::Named(id, _)
                    if matches!(self.registry.get_def(*id), Some(TypeDef::Struct { .. })));

                if matches!(&rhs_ty, Ty::Object(_)) && is_struct {
                    rhs_ty
                } else {
                    ann_ty
                }
            }
        };

        // Bind variables from the pattern
        self.bind_pattern(pattern, &ty, span);
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

    /// Infer types for a `SET` statement.
    ///
    /// Type-checks subscript expressions and the value, adding appropriate
    /// constraints. Does not modify the environment (database write).
    fn set(&mut self, target: ExprId, value: ExprId, span: Span) {
        // Extract subscripts from target (Local or Global)
        let subs: SmallVec<[ExprId; 4]> = self
            .ast
            .get_expr(target)
            .and_then(|e| match e {
                Expr::Local(_, s) | Expr::Global(_, s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        // Type-check subscript expressions
        subs.iter().for_each(|sub_id| {
            let sub_ty = self.expr(*sub_id);
            self.constrain(Constraint::Subscript(sub_ty, span));
        });

        // Type-check value and add Storable constraint
        let val_ty = self.expr(value);
        self.constrain(Constraint::Storable(val_ty, span));
    }

    /// Infer types for a `KILL` statement.
    ///
    /// Type-checks subscript expressions with `Subscript` constraints.
    /// Does not modify the environment (database delete).
    fn kill(&mut self, target: ExprId, span: Span) {
        // Extract subscripts from target (Local or Global)
        let subs: SmallVec<[ExprId; 4]> = self
            .ast
            .get_expr(target)
            .and_then(|e| match e {
                Expr::Local(_, s) | Expr::Global(_, s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        // Type-check subscript expressions
        subs.iter().for_each(|sub_id| {
            let sub_ty = self.expr(*sub_id);
            self.constrain(Constraint::Subscript(sub_ty, span));
        });
    }

    /// Infer types for an `OUTPUT` statement.
    ///
    /// Type-checks the expression and adds a `Stringable` constraint.
    fn output(&mut self, expr: ExprId, span: Span) {
        let ty = self.expr(expr);
        self.constrain(Constraint::Stringable(ty, span));
    }
}
