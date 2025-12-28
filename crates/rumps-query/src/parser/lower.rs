//! Lowering pass: CST to AST.
//!
//! Converts the boxed CST representation to the arena-allocated AST.
//! This is a straightforward recursive traversal with direct `&mut Ast` access;
//! no `Rc` or `RefCell` required.

use smallvec::SmallVec;

use super::cst;
use crate::ast::{
    ArrayElem, Ast, AstTypeExpr, AstTypeExprId, BindingPattern, Expr, ExprId,
    JsonAccessKey, MatchArm, MatchPattern, MatchPatternId, ObjectEntry,
    RestPattern, Stmt, StmtId, TypeDefAst, TypePattern, VariantAst,
};
use crate::Result;

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
        cst::StmtKind::Set(target, value) => {
            let target_id = lower_expr(ast, target)?;
            let value_id = lower_expr(ast, value)?;
            Stmt::Set(target_id, value_id)
        }
        cst::StmtKind::Kill(target) => {
            let target_id = lower_expr(ast, target)?;
            Stmt::Kill(target_id)
        }
        cst::StmtKind::Output(expr) => {
            let expr_id = lower_expr(ast, expr)?;
            Stmt::Output(expr_id)
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
                type_params: SmallVec::from_vec(type_params),
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
                type_params: SmallVec::from_vec(type_params),
                def: def_lowered,
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
                type_params: SmallVec::from_vec(type_params),
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
        cst::ExprKind::Var(name) => Expr::Var(name),
        cst::ExprKind::Local(name, subs) => {
            let sub_ids = lower_exprs(ast, subs)?;
            Expr::Local(name, sub_ids)
        }
        cst::ExprKind::Global(name, subs) => {
            let sub_ids = lower_exprs(ast, subs)?;
            Expr::Global(name, sub_ids)
        }
        cst::ExprKind::Get(inner) => {
            let inner_id = lower_expr(ast, *inner)?;
            Expr::Get(inner_id)
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
                type_params: SmallVec::from_vec(type_params),
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
        cst::TypeDefCst::Struct(fields) => {
            let lowered = fields
                .into_iter()
                .map(|(name, ty)| lower_type_expr(ast, ty).map(|id| (name, id)))
                .collect::<Result<Vec<_>>>()?;
            Ok(TypeDefAst::Struct(lowered))
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
        cst::MatchPattern::Is(name, ty) => {
            let ty_id = lower_type_expr(ast, ty)?;
            MatchPattern::Is(name, ty_id)
        }
    };
    ast.add_pattern(p)
}
