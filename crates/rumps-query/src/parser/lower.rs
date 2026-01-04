//! Lowering pass: CST to AST.
//!
//! Converts the boxed CST representation to the arena-allocated AST.
//! This is a straightforward recursive traversal with direct `&mut Ast` access;
//! no `Rc` or `RefCell` required.

use smallvec::SmallVec;

use super::cst;
use crate::ast::{
    self, ArrayElem, Ast, AstTypeExpr, AstTypeExprId, BindingPattern, DbRef,
    Expr, ExprId, JsonAccessKey, MatchArm, MatchPattern, MatchPatternId,
    ObjectEntry, OutputFormat, OutputStmt, OutputTarget, RestPattern, Stmt,
    StmtId, SubscriptElem, TransactionModifiers, TypeDefAst, TypePattern,
    VariantAst,
};
use crate::Result;

/// Convert a CST constraint to an AST constraint.
fn lower_constraint(c: cst::UserConstraint) -> ast::UserConstraint {
    match c {
        cst::UserConstraint::Numeric => ast::UserConstraint::Numeric,
        cst::UserConstraint::Stringable => ast::UserConstraint::Stringable,
        cst::UserConstraint::Jsonable => ast::UserConstraint::Jsonable,
        cst::UserConstraint::Subscriptable => {
            ast::UserConstraint::Subscriptable
        }
        cst::UserConstraint::Storable => ast::UserConstraint::Storable,
        cst::UserConstraint::Iterable(elem) => {
            ast::UserConstraint::Iterable(elem)
        }
        cst::UserConstraint::Monoid => ast::UserConstraint::Monoid,
        cst::UserConstraint::BitLike => ast::UserConstraint::BitLike,
    }
}

/// Convert a CST type parameter to an AST type parameter.
fn lower_type_param(tp: cst::TypeParam) -> ast::TypeParam {
    ast::TypeParam {
        name: tp.name,
        constraints: tp.constraints.into_iter().map(lower_constraint).collect(),
    }
}

/// Convert a list of CST type parameters to AST type parameters.
fn lower_type_params(
    tps: Vec<cst::TypeParam>,
) -> SmallVec<[ast::TypeParam; 2]> {
    tps.into_iter().map(lower_type_param).collect()
}

/// Lower a CST program (list of statements) to AST.
pub(crate) fn program(stmts: Vec<cst::Stmt>) -> Result<(Ast, Vec<StmtId>)> {
    let mut ast = Ast::new();
    let ids = stmts
        .into_iter()
        .map(|s| lower_stmt(&mut ast, s))
        .collect::<Result<Vec<_>>>()?;
    Ok((ast, ids))
}

/// Lower a CST statement to AST.
fn lower_stmt(ast: &mut Ast, stmt: cst::Stmt) -> Result<StmtId> {
    let span = stmt.span;
    let s = match stmt.kind {
        cst::StmtKind::Let(pat, ty, expr) => {
            let pat = lower_binding_pattern(pat);
            let ty_id = ty.map(|t| lower_type_expr(ast, t)).transpose()?;
            let expr_id = lower_expr(ast, expr)?;
            Stmt::Let(pat, ty_id, expr_id)
        }
        cst::StmtKind::Set(dbref, value) => {
            let dbref = lower_db_ref(ast, dbref)?;
            let value_id = lower_expr(ast, value)?;
            Stmt::Set(dbref, value_id, None)
        }
        cst::StmtKind::Kill(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Stmt::Kill(dbref, None)
        }
        cst::StmtKind::Output(output) => {
            let expr_id = lower_expr(ast, output.expr)?;
            let format = match output.format {
                cst::OutputFormat::Default => OutputFormat::Default,
                cst::OutputFormat::Json => OutputFormat::Json,
            };
            let target = match output.target {
                cst::OutputTarget::Stdout => OutputTarget::Stdout,
                cst::OutputTarget::Stderr => OutputTarget::Stderr,
                cst::OutputTarget::File(path_expr) => {
                    let path_id = lower_expr(ast, *path_expr)?;
                    OutputTarget::File(path_id)
                }
            };
            Stmt::Output(OutputStmt {
                expr: expr_id,
                format,
                target,
            })
        }
        cst::StmtKind::Expr(expr) => {
            let expr_id = lower_expr(ast, expr)?;
            Stmt::Expr(expr_id)
        }
        cst::StmtKind::Fun {
            name,
            type_params,
            params,
            ret,
            body,
        } => {
            let params_lowered = params
                .into_iter()
                .map(|(n, t)| {
                    t.map(|te| lower_type_expr(ast, te))
                        .transpose()
                        .map(|ty_id| (n, ty_id))
                })
                .collect::<Result<SmallVec<_>>>()?;
            let ret_id = ret.map(|t| lower_type_expr(ast, t)).transpose()?;
            let body_id = lower_expr(ast, body)?;
            Stmt::Fun {
                name,
                type_params: lower_type_params(type_params),
                params: params_lowered,
                ret: ret_id,
                body: body_id,
            }
        }
        cst::StmtKind::Type {
            name,
            type_params,
            def,
        } => {
            let def_lowered = lower_type_def(ast, def)?;
            Stmt::Type {
                name,
                type_params: lower_type_params(type_params),
                def: def_lowered,
            }
        }
        cst::StmtKind::NewType {
            name,
            type_params,
            target,
        } => {
            let target_id = lower_type_expr(ast, target)?;
            Stmt::NewType {
                name,
                type_params: lower_type_params(type_params),
                target: target_id,
            }
        }
        cst::StmtKind::Union {
            name,
            type_params,
            members,
        } => {
            let member_ids = members
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect::<Result<SmallVec<_>>>()?;
            Stmt::Union {
                name,
                type_params: lower_type_params(type_params),
                members: member_ids,
            }
        }
        cst::StmtKind::Module { name, body } => {
            let body_ids = body
                .into_iter()
                .map(|s| lower_stmt(ast, s))
                .collect::<Result<Vec<_>>>()?;
            Stmt::Module {
                name,
                body: body_ids,
            }
        }
    };
    ast.add_stmt(s, span)
}

/// Lower a CST expression to AST.
fn lower_expr(ast: &mut Ast, expr: cst::Expr) -> Result<ExprId> {
    let span = expr.span;
    let e = match expr.kind {
        cst::ExprKind::Literal(lit) => Expr::Literal(lit),
        cst::ExprKind::Interpolation(parts) => {
            lower_interpolation(ast, parts, span)?
        }
        cst::ExprKind::Var(name) => Expr::Var(name),
        cst::ExprKind::Get(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Expr::Get(dbref, None)
        }
        cst::ExprKind::Binary(lhs, op, rhs) => {
            let lhs_id = lower_expr(ast, *lhs)?;
            let rhs_id = lower_expr(ast, *rhs)?;
            Expr::Binary(lhs_id, op, rhs_id)
        }
        cst::ExprKind::Unary(op, operand) => {
            let operand_id = lower_expr(ast, *operand)?;
            Expr::Unary(op, operand_id)
        }
        cst::ExprKind::Call(callee, args) => {
            let callee_id = lower_expr(ast, *callee)?;
            let arg_ids = lower_exprs(ast, args)?;
            Expr::Call(callee_id, arg_ids)
        }
        cst::ExprKind::Object(entries) => {
            let lowered = entries
                .into_iter()
                .map(|e| lower_object_entry(ast, e))
                .collect::<Result<Vec<_>>>()?;
            Expr::Object(lowered)
        }
        cst::ExprKind::Array(elems) => {
            let lowered = elems
                .into_iter()
                .map(|e| lower_array_elem(ast, e))
                .collect::<Result<Vec<_>>>()?;
            Expr::Array(lowered)
        }
        cst::ExprKind::Tuple(elems) => {
            let elem_ids = elems
                .into_iter()
                .map(|e| lower_expr(ast, e))
                .collect::<Result<SmallVec<_>>>()?;
            Expr::Tuple(elem_ids)
        }
        cst::ExprKind::MapLit(entries) => {
            let entry_ids = entries
                .into_iter()
                .map(|(k, v)| {
                    let k_id = lower_expr(ast, k)?;
                    let v_id = lower_expr(ast, v)?;
                    Ok((k_id, v_id))
                })
                .collect::<Result<SmallVec<_>>>()?;
            Expr::MapLit(entry_ids)
        }
        cst::ExprKind::TupleIndex(base, idx) => {
            let base_id = lower_expr(ast, *base)?;
            Expr::TupleIndex(base_id, idx)
        }
        cst::ExprKind::Index(base, idx) => {
            let base_id = lower_expr(ast, *base)?;
            let idx_id = lower_expr(ast, *idx)?;
            Expr::Index(base_id, idx_id)
        }
        cst::ExprKind::OptionalIndex(base, idx) => {
            let base_id = lower_expr(ast, *base)?;
            let idx_id = lower_expr(ast, *idx)?;
            Expr::OptionalIndex(base_id, idx_id)
        }
        cst::ExprKind::Field(base, field) => {
            let base_id = lower_expr(ast, *base)?;
            Expr::Field(base_id, field)
        }
        cst::ExprKind::OptionalField(base, field) => {
            let base_id = lower_expr(ast, *base)?;
            Expr::OptionalField(base_id, field)
        }
        cst::ExprKind::Variant(ty, var, args) => {
            let arg_ids = lower_exprs(ast, args)?;
            Expr::Variant(ty, var, arg_ids)
        }
        // NOTE: No `Path` case; `Expr::Path` will be used for modules (not yet implemented).
        cst::ExprKind::Is(inner, pattern) => {
            let inner_id = lower_expr(ast, *inner)?;
            let lowered_pat = lower_type_pattern(ast, pattern)?;
            Expr::Is(inner_id, lowered_pat)
        }
        cst::ExprKind::As(inner, ty) => {
            let inner_id = lower_expr(ast, *inner)?;
            let ty_id = lower_type_expr(ast, ty)?;
            Expr::As(inner_id, ty_id)
        }
        cst::ExprKind::Read(inner, ty) => {
            let inner_id = lower_expr(ast, *inner)?;
            let ty_id = lower_type_expr(ast, ty)?;
            Expr::Read(inner_id, ty_id)
        }
        cst::ExprKind::Block(stmts, tail) => {
            let stmt_ids = stmts
                .into_iter()
                .map(|s| lower_stmt(ast, s))
                .collect::<Result<Vec<_>>>()?;
            let tail_id = tail.map(|e| lower_expr(ast, *e)).transpose()?;
            Expr::Block(stmt_ids, tail_id)
        }
        cst::ExprKind::If(cond, then_br, else_br) => {
            let cond_id = lower_expr(ast, *cond)?;
            let then_id = lower_expr(ast, *then_br)?;
            let else_id = else_br.map(|e| lower_expr(ast, *e)).transpose()?;
            Expr::If(cond_id, then_id, else_id)
        }
        cst::ExprKind::Closure {
            type_params,
            params,
            ret,
            body,
        } => {
            let params_lowered = params
                .into_iter()
                .map(|(n, t)| {
                    t.map(|te| lower_type_expr(ast, te))
                        .transpose()
                        .map(|ty_id| (n, ty_id))
                })
                .collect::<Result<SmallVec<_>>>()?;
            let ret_id = ret.map(|t| lower_type_expr(ast, t)).transpose()?;
            let body_id = lower_expr(ast, *body)?;
            Expr::Closure {
                type_params: lower_type_params(type_params),
                params: params_lowered,
                ret: ret_id,
                body: body_id,
            }
        }
        cst::ExprKind::Match(scrutinee, arms) => {
            let scrutinee_id = lower_expr(ast, *scrutinee)?;
            let arms_lowered = arms
                .into_iter()
                .map(|arm| lower_match_arm(ast, arm))
                .collect::<Result<Vec<_>>>()?;
            Expr::Match(scrutinee_id, arms_lowered)
        }
        cst::ExprKind::Unwrap(inner) => {
            let inner_id = lower_expr(ast, *inner)?;
            Expr::Unwrap(inner_id)
        }
        cst::ExprKind::Range(start, end, inclusive) => {
            let start_id = lower_expr(ast, *start)?;
            let end_id = lower_expr(ast, *end)?;
            Expr::Range(start_id, end_id, inclusive)
        }
        cst::ExprKind::Annotate(inner, ty) => {
            let inner_id = lower_expr(ast, *inner)?;
            let ty_id = lower_type_expr(ast, ty)?;
            Expr::Annotate(inner_id, ty_id)
        }
        cst::ExprKind::Json(fields) => {
            let field_ids = fields
                .into_iter()
                .map(|(k, v)| lower_expr(ast, v).map(|id| (k, id)))
                .collect::<Result<Vec<_>>>()?;
            Expr::Json(field_ids)
        }
        cst::ExprKind::JsonAccess(base, kind, key) => {
            let base_id = lower_expr(ast, *base)?;
            let key_lowered = match key {
                cst::JsonAccessKey::Field(name) => JsonAccessKey::Field(name),
                cst::JsonAccessKey::Expr(e) => {
                    let e_id = lower_expr(ast, *e)?;
                    JsonAccessKey::Expr(e_id)
                }
            };
            Expr::JsonAccess(base_id, kind, key_lowered)
        }
        cst::ExprKind::Regex(pattern) => Expr::Regex(pattern, None),
        cst::ExprKind::Matches(lhs, rhs) => {
            let lhs_id = lower_expr(ast, *lhs)?;
            let rhs_id = lower_expr(ast, *rhs)?;
            Expr::Matches(lhs_id, rhs_id)
        }
        cst::ExprKind::Catch(expr, handler) => {
            let expr_id = lower_expr(ast, *expr)?;
            let handler_id = lower_expr(ast, *handler)?;
            Expr::Catch(expr_id, handler_id)
        }
        cst::ExprKind::Data(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Expr::Data(dbref, None)
        }
        cst::ExprKind::Order(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Expr::Order(dbref, None)
        }
        cst::ExprKind::Query(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Expr::Query(dbref, None)
        }
        cst::ExprKind::Output(output) => {
            let expr_id = lower_expr(ast, output.expr)?;
            let format = match output.format {
                cst::OutputFormat::Default => OutputFormat::Default,
                cst::OutputFormat::Json => OutputFormat::Json,
            };
            let target = match output.target {
                cst::OutputTarget::Stdout => OutputTarget::Stdout,
                cst::OutputTarget::Stderr => OutputTarget::Stderr,
                cst::OutputTarget::File(path_expr) => {
                    let path_id = lower_expr(ast, *path_expr)?;
                    OutputTarget::File(path_id)
                }
            };
            Expr::Output(OutputStmt {
                expr: expr_id,
                format,
                target,
            })
        }
        cst::ExprKind::Set(dbref, value) => {
            let dbref = lower_db_ref(ast, dbref)?;
            let value_id = lower_expr(ast, *value)?;
            Expr::Set(dbref, value_id, None)
        }
        cst::ExprKind::Kill(dbref) => {
            let dbref = lower_db_ref(ast, dbref)?;
            Expr::Kill(dbref, None)
        }
        cst::ExprKind::Raise(inner) => {
            let id = lower_expr(ast, *inner)?;
            Expr::Raise(id)
        }
        cst::ExprKind::Forever {
            seed,
            state_param,
            cont_param,
            body,
        } => {
            let seed_id = lower_expr(ast, *seed)?;
            let state_ty =
                state_param.1.map(|t| lower_type_expr(ast, t)).transpose()?;
            let cont_ty =
                cont_param.1.map(|t| lower_type_expr(ast, t)).transpose()?;
            let body_id = lower_expr(ast, *body)?;
            Expr::Forever {
                seed: seed_id,
                state_param: (state_param.0, state_ty),
                cont_param: (cont_param.0, cont_ty),
                body: body_id,
            }
        }
        cst::ExprKind::Transaction(txn) => {
            let stmts = txn
                .stmts
                .into_iter()
                .map(|s| lower_stmt(ast, s))
                .collect::<Result<Vec<_>>>()?;
            let expr = txn.expr.map(|e| lower_expr(ast, *e)).transpose()?;
            let modifiers = lower_txn_modifiers(ast, txn.modifiers)?;
            Expr::Transaction(ast::TransactionExpr {
                id: None,
                stmts,
                expr,
                modifiers,
            })
        }
        cst::ExprKind::Error(msg) => {
            Err(crate::Error::parse(span, msg, vec![]))?
        }
    };
    ast.add_expr(e, span)
}

/// Lower a list of CST expressions to AST, returning a `SmallVec`.
fn lower_exprs(
    ast: &mut Ast,
    exprs: Vec<cst::Expr>,
) -> Result<SmallVec<[ExprId; 4]>> {
    exprs
        .into_iter()
        .map(|e| lower_expr(ast, e))
        .collect::<Result<SmallVec<_>>>()
}

/// Lower a CST type expression to AST.
fn lower_type_expr(ast: &mut Ast, ty: cst::TypeExpr) -> Result<AstTypeExprId> {
    let span = ty.span;
    let te = match ty.kind {
        cst::TypeExprKind::Named(name) => AstTypeExpr::Named(name),
        cst::TypeExprKind::App(name, params) => {
            let param_ids = params
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect::<Result<SmallVec<_>>>()?;
            AstTypeExpr::App(name, param_ids)
        }
        cst::TypeExprKind::Fn(params, ret) => {
            let param_ids = params
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect::<Result<SmallVec<_>>>()?;
            let ret_id = lower_type_expr(ast, *ret)?;
            AstTypeExpr::Fn(param_ids, ret_id)
        }
        cst::TypeExprKind::Tuple(elems) => {
            let elem_ids = elems
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect::<Result<SmallVec<_>>>()?;
            AstTypeExpr::Tuple(elem_ids)
        }
        cst::TypeExprKind::Union(members) => {
            let member_ids = members
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect::<Result<SmallVec<_>>>()?;
            AstTypeExpr::Union(member_ids)
        }
        cst::TypeExprKind::Object(fields) => {
            let lowered = fields
                .into_iter()
                .map(|(name, ty)| lower_type_expr(ast, ty).map(|id| (name, id)))
                .collect::<Result<SmallVec<_>>>()?;
            AstTypeExpr::Object(lowered)
        }
    };
    ast.add_type_expr(te, span)
}

/// Lower a CST type pattern to AST.
fn lower_type_pattern(
    ast: &mut Ast,
    pat: cst::TypePattern,
) -> Result<TypePattern> {
    Ok(match pat {
        cst::TypePattern::Type(ty) => {
            TypePattern::Type(lower_type_expr(ast, ty)?)
        }
        cst::TypePattern::Variant(ty, var) => TypePattern::Variant(ty, var),
        cst::TypePattern::VariantWildcard(ty, var) => {
            TypePattern::VariantWildcard(ty, var)
        }
        cst::TypePattern::VariantBind(ty, var, names) => {
            TypePattern::VariantBind(ty, var, names)
        }
        cst::TypePattern::Object(fields) => {
            let lowered = fields
                .into_iter()
                .map(|(name, ty)| lower_type_expr(ast, ty).map(|id| (name, id)))
                .collect::<Result<SmallVec<_>>>()?;
            TypePattern::Object(lowered)
        }
    })
}

/// Lower a CST binding pattern to AST.
fn lower_binding_pattern(pat: cst::BindingPattern) -> BindingPattern {
    match pat {
        cst::BindingPattern::Var(name) => BindingPattern::Var(name),
        cst::BindingPattern::Tuple(pats) => BindingPattern::Tuple(
            pats.into_iter().map(lower_binding_pattern).collect(),
        ),
        cst::BindingPattern::Object(fields) => BindingPattern::Object(
            fields
                .into_iter()
                .map(|(k, p)| (k, lower_binding_pattern(p)))
                .collect(),
        ),
        cst::BindingPattern::Array(pats, rest) => BindingPattern::Array(
            pats.into_iter().map(lower_binding_pattern).collect(),
            rest.map(lower_rest_pattern),
        ),
        cst::BindingPattern::Wildcard => BindingPattern::Wildcard,
    }
}

/// Lower a CST rest pattern to AST.
fn lower_rest_pattern(pat: cst::RestPattern) -> RestPattern {
    match pat {
        cst::RestPattern::Ignore => RestPattern::Ignore,
        cst::RestPattern::Bind(name) => RestPattern::Bind(name),
    }
}

/// Lower a CST type definition to AST.
fn lower_type_def(ast: &mut Ast, def: cst::TypeDefCst) -> Result<TypeDefAst> {
    match def {
        cst::TypeDefCst::Sum(variants) => {
            let lowered = variants
                .into_iter()
                .map(|v| lower_variant(ast, v))
                .collect::<Result<SmallVec<_>>>()?;
            Ok(TypeDefAst::Sum(lowered))
        }
    }
}

/// Lower a CST variant to AST.
fn lower_variant(ast: &mut Ast, v: cst::VariantCst) -> Result<VariantAst> {
    let payloads = v
        .payloads
        .into_iter()
        .map(|t| lower_type_expr(ast, t))
        .collect::<Result<SmallVec<_>>>()?;
    Ok(VariantAst {
        name: v.name,
        payloads,
    })
}

/// Lower a CST match arm to AST.
fn lower_match_arm(ast: &mut Ast, arm: cst::MatchArm) -> Result<MatchArm> {
    let pattern = lower_match_pattern(ast, arm.pattern)?;
    let guard = arm.guard.map(|e| lower_expr(ast, e)).transpose()?;
    let body = lower_expr(ast, arm.body)?;
    Ok(MatchArm {
        pattern,
        guard,
        body,
    })
}

/// Lower a CST array element to AST.
fn lower_array_elem(ast: &mut Ast, elem: cst::ArrayElem) -> Result<ArrayElem> {
    match elem {
        cst::ArrayElem::Elem(e) => lower_expr(ast, e).map(ArrayElem::Elem),
        cst::ArrayElem::Spread(e) => lower_expr(ast, e).map(ArrayElem::Spread),
    }
}

/// Lower a CST object entry to AST.
fn lower_object_entry(
    ast: &mut Ast,
    entry: cst::ObjectEntry,
) -> Result<ObjectEntry> {
    match entry {
        cst::ObjectEntry::Field(k, v) => {
            lower_expr(ast, v).map(|id| ObjectEntry::Field(k, id))
        }
        cst::ObjectEntry::Spread(e) => {
            lower_expr(ast, e).map(ObjectEntry::Spread)
        }
    }
}

/// Lower a CST subscript element to AST.
fn lower_subscript_elem(
    ast: &mut Ast,
    elem: cst::SubscriptElem,
) -> Result<SubscriptElem> {
    match elem {
        cst::SubscriptElem::Elem(e) => {
            lower_expr(ast, e).map(SubscriptElem::Elem)
        }
        cst::SubscriptElem::Spread(e) => {
            lower_expr(ast, e).map(SubscriptElem::Spread)
        }
    }
}

/// Lower a list of CST subscript elements to AST.
fn lower_subscript_elems(
    ast: &mut Ast,
    elems: Vec<cst::SubscriptElem>,
) -> Result<SmallVec<[SubscriptElem; 4]>> {
    elems
        .into_iter()
        .map(|e| lower_subscript_elem(ast, e))
        .collect()
}

/// Lower a CST database reference to AST.
fn lower_db_ref(ast: &mut Ast, dbref: cst::DbRef) -> Result<DbRef> {
    match dbref {
        cst::DbRef::Local(name, subs) => {
            let sub_ids = lower_subscript_elems(ast, subs)?;
            Ok(DbRef::Local(name, sub_ids))
        }
        cst::DbRef::Global(name, subs) => {
            let sub_ids = lower_subscript_elems(ast, subs)?;
            Ok(DbRef::Global(name, sub_ids))
        }
    }
}

/// Lower a CST match pattern to AST, allocating into the pattern arena.
fn lower_match_pattern(
    ast: &mut Ast,
    pat: cst::MatchPattern,
) -> Result<MatchPatternId> {
    let p = match pat {
        cst::MatchPattern::Wildcard => MatchPattern::Wildcard,
        cst::MatchPattern::Var(name) => MatchPattern::Var(name),
        cst::MatchPattern::Literal(lit) => MatchPattern::Literal(lit),
        cst::MatchPattern::Variant(ty, var, pats) => {
            let sub_ids = pats
                .into_iter()
                .map(|p| lower_match_pattern(ast, p))
                .collect::<Result<SmallVec<_>>>()?;
            MatchPattern::Variant(ty, var, sub_ids)
        }
        cst::MatchPattern::Object(fields) => {
            let field_ids = fields
                .into_iter()
                .map(|(k, p)| lower_match_pattern(ast, p).map(|id| (k, id)))
                .collect::<Result<SmallVec<_>>>()?;
            MatchPattern::Object(field_ids)
        }
        cst::MatchPattern::Tuple(pats) => {
            let elem_ids = pats
                .into_iter()
                .map(|p| lower_match_pattern(ast, p))
                .collect::<Result<SmallVec<_>>>()?;
            MatchPattern::Tuple(elem_ids)
        }
        cst::MatchPattern::Array(pats, rest) => {
            let elem_ids = pats
                .into_iter()
                .map(|p| lower_match_pattern(ast, p))
                .collect::<Result<SmallVec<_>>>()?;
            MatchPattern::Array(elem_ids, rest.map(lower_rest_pattern))
        }
        cst::MatchPattern::Is(name, ty) => {
            let ty_id = lower_type_expr(ast, ty)?;
            MatchPattern::Is(name, ty_id)
        }
    };
    ast.add_pattern(p)
}

/// Lower CST transaction modifiers to AST.
fn lower_txn_modifiers(
    ast: &mut Ast,
    m: cst::TransactionModifiers,
) -> Result<TransactionModifiers> {
    let conflict = m.conflict.map(|c| match c {
        cst::ConflictModifier::Abort => rumps_storage::ConflictStrategy::Abort,
        cst::ConflictModifier::Overwrite => {
            rumps_storage::ConflictStrategy::Overwrite
        }
    });
    let timeout = m.timeout.map(|e| lower_expr(ast, *e)).transpose()?;
    let isolation = m.isolation.map(|i| match i {
        cst::IsolationModifier::Snapshot => {
            rumps_storage::IsolationLevel::SnapshotIsolation
        }
    });
    Ok(TransactionModifiers {
        conflict,
        timeout,
        retries: m.retries,
        isolation,
    })
}

/// Lower interpolated string parts to an AST expression.
///
/// Takes the alternating literal/expression parts and parses expression strings
/// into AST nodes. Returns an `Expr::Interpolation` containing the parsed parts.
fn lower_interpolation(
    ast: &mut Ast,
    parts: Vec<String>,
    span: crate::Span,
) -> Result<Expr> {
    use crate::{Lexer, Parser};

    let ids: Result<SmallVec<[crate::ast::ExprId; 4]>> = parts
        .into_iter()
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 0 {
                // Even indices: literal text; create a String literal
                let lit = ast::Literal::String(part);
                ast.add_expr(Expr::Literal(lit), span)
            } else {
                // Odd indices: expression source code; parse and merge
                let tokens = Lexer::new(&part).lex().map_err(|e| {
                    crate::Error::parse(span, e.to_string(), vec![])
                })?;

                let parsed = Parser::parse_tokens(&tokens).map_err(|e| {
                    crate::Error::parse(span, e.to_string(), vec![])
                })?;

                // Should produce exactly one expression statement
                let expr = parsed
                    .stmts
                    .first()
                    .and_then(|stmt_id| parsed.ast.get_stmt(*stmt_id))
                    .and_then(|stmt| match stmt {
                        ast::Stmt::Expr(expr_id) => Some(*expr_id),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        let msg = if part.trim().is_empty() {
                            "interpolation requires an expression".into()
                        } else {
                            format!("interpolation requires an expression, got `{}`", part)
                        };
                        crate::Error::parse(span, msg, vec![])
                    })?;

                // Copy the expression from the parsed AST into our AST
                merge_expr(ast, &parsed.ast, expr, span)
            }
        })
        .collect();

    ids.map(Expr::Interpolation)
}

/// Merge a `DbRef` from source AST into target AST.
///
/// Recursively copies subscript expressions.
fn merge_dbref(
    target: &mut Ast,
    source: &Ast,
    dbref: &DbRef,
    span: crate::Span,
) -> Result<DbRef> {
    let mut merge_subs =
        |subs: &SmallVec<[SubscriptElem; 4]>| -> Result<SmallVec<_>> {
            subs.iter()
                .map(|elem| match elem {
                    SubscriptElem::Elem(e) => {
                        merge_expr(target, source, *e, span)
                            .map(SubscriptElem::Elem)
                    }
                    SubscriptElem::Spread(e) => {
                        merge_expr(target, source, *e, span)
                            .map(SubscriptElem::Spread)
                    }
                })
                .collect()
        };
    match dbref {
        DbRef::Local(name, subs) => {
            Ok(DbRef::Local(name.clone(), merge_subs(subs)?))
        }
        DbRef::Global(name, subs) => {
            Ok(DbRef::Global(name.clone(), merge_subs(subs)?))
        }
    }
}

/// Merge an `AstTypeExprId` from source AST into target AST.
fn merge_type_expr(
    target: &mut Ast,
    source: &Ast,
    id: AstTypeExprId,
    span: crate::Span,
) -> Result<AstTypeExprId> {
    let te = source
        .get_type_expr(id)
        .ok_or_else(|| {
            crate::Error::parse(span, "invalid type expr id", vec![])
        })?
        .clone();
    let new_te = match te {
        AstTypeExpr::Named(n) => AstTypeExpr::Named(n),
        AstTypeExpr::App(name, args) => {
            let new_args: Result<SmallVec<_>> = args
                .iter()
                .map(|&a| merge_type_expr(target, source, a, span))
                .collect();
            AstTypeExpr::App(name, new_args?)
        }
        AstTypeExpr::Fn(params, ret) => {
            let new_params: Result<SmallVec<_>> = params
                .iter()
                .map(|&p| merge_type_expr(target, source, p, span))
                .collect();
            let new_ret = merge_type_expr(target, source, ret, span)?;
            AstTypeExpr::Fn(new_params?, new_ret)
        }
        AstTypeExpr::Tuple(elems) => {
            let new_elems: Result<SmallVec<_>> = elems
                .iter()
                .map(|&e| merge_type_expr(target, source, e, span))
                .collect();
            AstTypeExpr::Tuple(new_elems?)
        }
        AstTypeExpr::Union(members) => {
            let new_members: Result<SmallVec<_>> = members
                .iter()
                .map(|&m| merge_type_expr(target, source, m, span))
                .collect();
            AstTypeExpr::Union(new_members?)
        }
        AstTypeExpr::Object(fields) => {
            let new_fields: Result<SmallVec<_>> = fields
                .into_iter()
                .map(|(name, ty_id)| {
                    merge_type_expr(target, source, ty_id, span)
                        .map(|new_id| (name, new_id))
                })
                .collect();
            AstTypeExpr::Object(new_fields?)
        }
    };
    target.add_type_expr(new_te, span)
}

/// Merge a `MatchPatternId` from source AST into target AST.
fn merge_pattern(
    target: &mut Ast,
    source: &Ast,
    id: MatchPatternId,
    span: crate::Span,
) -> Result<MatchPatternId> {
    let pat = source
        .get_pattern(id)
        .ok_or_else(|| crate::Error::parse(span, "invalid pattern id", vec![]))?
        .clone();
    let new_pat = match pat {
        MatchPattern::Wildcard => MatchPattern::Wildcard,
        MatchPattern::Var(name) => MatchPattern::Var(name),
        MatchPattern::Literal(lit) => MatchPattern::Literal(lit),
        MatchPattern::Variant(ty, var, pats) => {
            let new_pats: Result<SmallVec<_>> = pats
                .iter()
                .map(|&p| merge_pattern(target, source, p, span))
                .collect();
            MatchPattern::Variant(ty, var, new_pats?)
        }
        MatchPattern::Object(fields) => {
            let new_fields: Result<SmallVec<_>> = fields
                .into_iter()
                .map(|(name, pat_id)| {
                    merge_pattern(target, source, pat_id, span)
                        .map(|new_id| (name, new_id))
                })
                .collect();
            MatchPattern::Object(new_fields?)
        }
        MatchPattern::Tuple(pats) => {
            let new_pats: Result<SmallVec<_>> = pats
                .iter()
                .map(|&p| merge_pattern(target, source, p, span))
                .collect();
            MatchPattern::Tuple(new_pats?)
        }
        MatchPattern::Array(pats, rest) => {
            let new_pats: Result<SmallVec<_>> = pats
                .iter()
                .map(|&p| merge_pattern(target, source, p, span))
                .collect();
            MatchPattern::Array(new_pats?, rest)
        }
        MatchPattern::Is(name, ty_id) => {
            let new_ty = merge_type_expr(target, source, ty_id, span)?;
            MatchPattern::Is(name, new_ty)
        }
    };
    target.add_pattern(new_pat)
}

/// Merge an `OutputStmt` from source AST into target AST.
fn merge_output_stmt(
    target: &mut Ast,
    source: &Ast,
    stmt: &OutputStmt,
    span: crate::Span,
) -> Result<OutputStmt> {
    let new_expr = merge_expr(target, source, stmt.expr, span)?;
    let new_target = match stmt.target {
        OutputTarget::Stdout => OutputTarget::Stdout,
        OutputTarget::Stderr => OutputTarget::Stderr,
        OutputTarget::File(e) => {
            OutputTarget::File(merge_expr(target, source, e, span)?)
        }
    };
    Ok(OutputStmt {
        expr: new_expr,
        format: stmt.format,
        target: new_target,
    })
}

/// Merge a `TypePattern` from source AST into target AST.
fn merge_type_pattern(
    target: &mut Ast,
    source: &Ast,
    pat: &TypePattern,
    span: crate::Span,
) -> Result<TypePattern> {
    match pat {
        TypePattern::Type(ty_id) => {
            let new_ty = merge_type_expr(target, source, *ty_id, span)?;
            Ok(TypePattern::Type(new_ty))
        }
        TypePattern::Variant(ty, var) => {
            Ok(TypePattern::Variant(ty.clone(), var.clone()))
        }
        TypePattern::VariantWildcard(ty, var) => {
            Ok(TypePattern::VariantWildcard(ty.clone(), var.clone()))
        }
        TypePattern::VariantBind(ty, var, binds) => Ok(
            TypePattern::VariantBind(ty.clone(), var.clone(), binds.clone()),
        ),
        TypePattern::Object(fields) => {
            let new_fields: Result<SmallVec<_>> = fields
                .iter()
                .map(|(name, ty_id)| {
                    merge_type_expr(target, source, *ty_id, span)
                        .map(|new_id| (name.clone(), new_id))
                })
                .collect();
            Ok(TypePattern::Object(new_fields?))
        }
    }
}

/// Merge a statement from source AST into target AST.
fn merge_stmt(
    target: &mut Ast,
    source: &Ast,
    stmt_id: StmtId,
    span: crate::Span,
) -> Result<StmtId> {
    let stmt = source
        .get_stmt(stmt_id)
        .ok_or_else(|| crate::Error::parse(span, "invalid stmt id", vec![]))?
        .clone();
    let new_stmt = match stmt {
        Stmt::Let(pat, ty_ann, expr) => {
            let new_ty = ty_ann
                .map(|t| merge_type_expr(target, source, t, span))
                .transpose()?;
            let new_expr = merge_expr(target, source, expr, span)?;
            Stmt::Let(pat, new_ty, new_expr)
        }
        Stmt::Set(dbref, expr, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            let new_expr = merge_expr(target, source, expr, span)?;
            Stmt::Set(new_dbref, new_expr, txn)
        }
        Stmt::Kill(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Stmt::Kill(new_dbref, txn)
        }
        Stmt::Output(out) => {
            let new_out = merge_output_stmt(target, source, &out, span)?;
            Stmt::Output(new_out)
        }
        Stmt::Expr(e) => {
            let new_e = merge_expr(target, source, e, span)?;
            Stmt::Expr(new_e)
        }
        Stmt::Fun {
            name,
            type_params,
            params,
            ret,
            body,
        } => {
            let new_params: Result<SmallVec<_>> = params
                .into_iter()
                .map(|(n, ty_opt)| {
                    let new_ty = ty_opt
                        .map(|t| merge_type_expr(target, source, t, span))
                        .transpose()?;
                    Ok((n, new_ty))
                })
                .collect();
            let new_ret = ret
                .map(|t| merge_type_expr(target, source, t, span))
                .transpose()?;
            let new_body = merge_expr(target, source, body, span)?;
            Stmt::Fun {
                name,
                type_params,
                params: new_params?,
                ret: new_ret,
                body: new_body,
            }
        }
        Stmt::Type {
            name,
            type_params,
            def,
        } => {
            // TypeDefAst variants only contain AstTypeExprId
            let new_def = match def {
                TypeDefAst::Sum(variants) => {
                    let new_variants: Result<SmallVec<_>> = variants
                        .into_iter()
                        .map(|v| {
                            let new_payloads: Result<SmallVec<_>> = v
                                .payloads
                                .iter()
                                .map(|&p| {
                                    merge_type_expr(target, source, p, span)
                                })
                                .collect();
                            Ok(VariantAst {
                                name: v.name,
                                payloads: new_payloads?,
                            })
                        })
                        .collect();
                    TypeDefAst::Sum(new_variants?)
                }
            };
            Stmt::Type {
                name,
                type_params,
                def: new_def,
            }
        }
        Stmt::NewType {
            name,
            type_params,
            target: ty,
        } => {
            let new_ty = merge_type_expr(target, source, ty, span)?;
            Stmt::NewType {
                name,
                type_params,
                target: new_ty,
            }
        }
        Stmt::Union {
            name,
            type_params,
            members,
        } => {
            let new_members: Result<SmallVec<_>> = members
                .iter()
                .map(|&m| merge_type_expr(target, source, m, span))
                .collect();
            Stmt::Union {
                name,
                type_params,
                members: new_members?,
            }
        }
        Stmt::Module { name, body } => {
            let new_body: Result<Vec<_>> = body
                .iter()
                .map(|&s| merge_stmt(target, source, s, span))
                .collect();
            Stmt::Module {
                name,
                body: new_body?,
            }
        }
    };
    target.add_stmt(new_stmt, span)
}

/// Merge an expression from a parsed AST into the target AST.
///
/// Recursively copies the expression and all its sub-expressions, statements,
/// type expressions, and patterns.
fn merge_expr(
    target: &mut Ast,
    source: &Ast,
    expr_id: ExprId,
    span: crate::Span,
) -> Result<ExprId> {
    let expr = source
        .get_expr(expr_id)
        .ok_or_else(|| {
            crate::Error::parse(span, "invalid expression id", vec![])
        })?
        .clone();

    let new_expr = match expr {
        Expr::Literal(lit) => Expr::Literal(lit),
        Expr::Interpolation(parts) => {
            let new_parts: Result<SmallVec<_>> = parts
                .iter()
                .map(|&id| merge_expr(target, source, id, span))
                .collect();
            Expr::Interpolation(new_parts?)
        }
        Expr::Var(name) => Expr::Var(name),
        Expr::Get(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Expr::Get(new_dbref, txn)
        }
        Expr::Binary(lhs, op, rhs) => {
            let new_lhs = merge_expr(target, source, lhs, span)?;
            let new_rhs = merge_expr(target, source, rhs, span)?;
            Expr::Binary(new_lhs, op, new_rhs)
        }
        Expr::Unary(op, operand) => {
            let new_op = merge_expr(target, source, operand, span)?;
            Expr::Unary(op, new_op)
        }
        Expr::Call(callee, args) => {
            let new_callee = merge_expr(target, source, callee, span)?;
            let new_args: Result<SmallVec<_>> = args
                .iter()
                .map(|&id| merge_expr(target, source, id, span))
                .collect();
            Expr::Call(new_callee, new_args?)
        }
        Expr::Object(entries) => {
            let new_entries: Result<Vec<_>> = entries
                .into_iter()
                .map(|entry| match entry {
                    ObjectEntry::Field(k, v) => {
                        merge_expr(target, source, v, span)
                            .map(|new_v| ObjectEntry::Field(k, new_v))
                    }
                    ObjectEntry::Spread(e) => {
                        merge_expr(target, source, e, span)
                            .map(ObjectEntry::Spread)
                    }
                })
                .collect();
            Expr::Object(new_entries?)
        }
        Expr::Array(elems) => {
            let new_elems: Result<Vec<_>> = elems
                .into_iter()
                .map(|elem| match elem {
                    ArrayElem::Elem(e) => {
                        merge_expr(target, source, e, span).map(ArrayElem::Elem)
                    }
                    ArrayElem::Spread(e) => merge_expr(target, source, e, span)
                        .map(ArrayElem::Spread),
                })
                .collect();
            Expr::Array(new_elems?)
        }
        Expr::Tuple(elems) => {
            let new_elems: Result<SmallVec<_>> = elems
                .iter()
                .map(|&id| merge_expr(target, source, id, span))
                .collect();
            Expr::Tuple(new_elems?)
        }
        Expr::MapLit(entries) => {
            let new_entries: Result<SmallVec<_>> = entries
                .into_iter()
                .map(|(k, v)| {
                    let new_k = merge_expr(target, source, k, span)?;
                    let new_v = merge_expr(target, source, v, span)?;
                    Ok((new_k, new_v))
                })
                .collect();
            Expr::MapLit(new_entries?)
        }
        Expr::TupleIndex(base, idx) => {
            let new_base = merge_expr(target, source, base, span)?;
            Expr::TupleIndex(new_base, idx)
        }
        Expr::Index(base, idx) => {
            let new_base = merge_expr(target, source, base, span)?;
            let new_idx = merge_expr(target, source, idx, span)?;
            Expr::Index(new_base, new_idx)
        }
        Expr::OptionalIndex(base, idx) => {
            let new_base = merge_expr(target, source, base, span)?;
            let new_idx = merge_expr(target, source, idx, span)?;
            Expr::OptionalIndex(new_base, new_idx)
        }
        Expr::Field(base, field) => {
            let new_base = merge_expr(target, source, base, span)?;
            Expr::Field(new_base, field)
        }
        Expr::OptionalField(base, field) => {
            let new_base = merge_expr(target, source, base, span)?;
            Expr::OptionalField(new_base, field)
        }
        Expr::Variant(ty, var, args) => {
            let new_args: Result<SmallVec<_>> = args
                .iter()
                .map(|&id| merge_expr(target, source, id, span))
                .collect();
            Expr::Variant(ty, var, new_args?)
        }
        Expr::Path(segments) => Expr::Path(segments),
        Expr::Is(expr, pat) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_pat = merge_type_pattern(target, source, &pat, span)?;
            Expr::Is(new_expr, new_pat)
        }
        Expr::As(expr, ty) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_ty = merge_type_expr(target, source, ty, span)?;
            Expr::As(new_expr, new_ty)
        }
        Expr::Read(expr, ty) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_ty = merge_type_expr(target, source, ty, span)?;
            Expr::Read(new_expr, new_ty)
        }
        Expr::Block(stmts, tail) => {
            let new_stmts: Result<Vec<_>> = stmts
                .iter()
                .map(|&s| merge_stmt(target, source, s, span))
                .collect();
            let new_tail = tail
                .map(|e| merge_expr(target, source, e, span))
                .transpose()?;
            Expr::Block(new_stmts?, new_tail)
        }
        Expr::If(cond, then, els) => {
            let new_cond = merge_expr(target, source, cond, span)?;
            let new_then = merge_expr(target, source, then, span)?;
            let new_els = els
                .map(|e| merge_expr(target, source, e, span))
                .transpose()?;
            Expr::If(new_cond, new_then, new_els)
        }
        Expr::Match(scrut, arms) => {
            let new_scrut = merge_expr(target, source, scrut, span)?;
            let new_arms: Result<Vec<_>> = arms
                .into_iter()
                .map(|arm| {
                    let new_pat =
                        merge_pattern(target, source, arm.pattern, span)?;
                    let new_guard = arm
                        .guard
                        .map(|g| merge_expr(target, source, g, span))
                        .transpose()?;
                    let new_body = merge_expr(target, source, arm.body, span)?;
                    Ok(MatchArm {
                        pattern: new_pat,
                        guard: new_guard,
                        body: new_body,
                    })
                })
                .collect();
            Expr::Match(new_scrut, new_arms?)
        }
        Expr::Closure {
            type_params,
            params,
            ret,
            body,
        } => {
            let new_params: Result<SmallVec<_>> = params
                .into_iter()
                .map(|(n, ty_opt)| {
                    let new_ty = ty_opt
                        .map(|t| merge_type_expr(target, source, t, span))
                        .transpose()?;
                    Ok((n, new_ty))
                })
                .collect();
            let new_ret = ret
                .map(|t| merge_type_expr(target, source, t, span))
                .transpose()?;
            let new_body = merge_expr(target, source, body, span)?;
            Expr::Closure {
                type_params,
                params: new_params?,
                ret: new_ret,
                body: new_body,
            }
        }
        Expr::Unwrap(expr) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            Expr::Unwrap(new_expr)
        }
        Expr::Range(start, end, incl) => {
            let new_start = merge_expr(target, source, start, span)?;
            let new_end = merge_expr(target, source, end, span)?;
            Expr::Range(new_start, new_end, incl)
        }
        Expr::Annotate(expr, ty) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_ty = merge_type_expr(target, source, ty, span)?;
            Expr::Annotate(new_expr, new_ty)
        }
        Expr::Json(entries) => {
            let new_entries: Result<Vec<_>> = entries
                .into_iter()
                .map(|(k, v)| {
                    merge_expr(target, source, v, span).map(|new_v| (k, new_v))
                })
                .collect();
            Expr::Json(new_entries?)
        }
        Expr::JsonAccess(expr, kind, key) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_key = match key {
                JsonAccessKey::Field(f) => JsonAccessKey::Field(f),
                JsonAccessKey::Expr(e) => {
                    JsonAccessKey::Expr(merge_expr(target, source, e, span)?)
                }
            };
            Expr::JsonAccess(new_expr, kind, new_key)
        }
        Expr::Regex(pat, cache_idx) => Expr::Regex(pat, cache_idx),
        Expr::Matches(lhs, rhs) => {
            let new_lhs = merge_expr(target, source, lhs, span)?;
            let new_rhs = merge_expr(target, source, rhs, span)?;
            Expr::Matches(new_lhs, new_rhs)
        }
        Expr::Catch(expr, handler) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            let new_handler = merge_expr(target, source, handler, span)?;
            Expr::Catch(new_expr, new_handler)
        }
        Expr::Data(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Expr::Data(new_dbref, txn)
        }
        Expr::Order(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Expr::Order(new_dbref, txn)
        }
        Expr::Query(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Expr::Query(new_dbref, txn)
        }
        Expr::Output(out) => {
            let new_out = merge_output_stmt(target, source, &out, span)?;
            Expr::Output(new_out)
        }
        Expr::Set(dbref, expr, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            let new_expr = merge_expr(target, source, expr, span)?;
            Expr::Set(new_dbref, new_expr, txn)
        }
        Expr::Kill(dbref, txn) => {
            let new_dbref = merge_dbref(target, source, &dbref, span)?;
            Expr::Kill(new_dbref, txn)
        }
        Expr::Raise(expr) => {
            let new_expr = merge_expr(target, source, expr, span)?;
            Expr::Raise(new_expr)
        }
        Expr::Forever {
            seed,
            state_param,
            cont_param,
            body,
        } => {
            let new_seed = merge_expr(target, source, seed, span)?;
            let new_state_ty = state_param
                .1
                .map(|t| merge_type_expr(target, source, t, span))
                .transpose()?;
            let new_cont_ty = cont_param
                .1
                .map(|t| merge_type_expr(target, source, t, span))
                .transpose()?;
            let new_body = merge_expr(target, source, body, span)?;
            Expr::Forever {
                seed: new_seed,
                state_param: (state_param.0, new_state_ty),
                cont_param: (cont_param.0, new_cont_ty),
                body: new_body,
            }
        }
        Expr::Transaction(txn_expr) => {
            let new_stmts: Result<Vec<_>> = txn_expr
                .stmts
                .iter()
                .map(|&s| merge_stmt(target, source, s, span))
                .collect();
            let new_tail = txn_expr
                .expr
                .map(|e| merge_expr(target, source, e, span))
                .transpose()?;
            let new_timeout = txn_expr
                .modifiers
                .timeout
                .map(|e| merge_expr(target, source, e, span))
                .transpose()?;
            Expr::Transaction(ast::TransactionExpr {
                id: txn_expr.id,
                stmts: new_stmts?,
                expr: new_tail,
                modifiers: TransactionModifiers {
                    conflict: txn_expr.modifiers.conflict,
                    timeout: new_timeout,
                    retries: txn_expr.modifiers.retries,
                    isolation: txn_expr.modifiers.isolation,
                },
            })
        }
    };

    target.add_expr(new_expr, span)
}
