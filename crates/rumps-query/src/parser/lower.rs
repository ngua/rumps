//! Lowering pass: CST to AST.
//!
//! Converts the boxed CST representation to the arena-allocated AST.
//! This is a straightforward recursive traversal with direct `&mut Ast` access;
//! no `Rc` or `RefCell` required.

use smallvec::SmallVec;

use super::cst;
use crate::ast::{
    Ast, AstTypeExpr, AstTypeExprId, BindingPattern, Expr, ExprId, RestPattern,
    Stmt, StmtId, TypeDefAst, VariantAst,
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
        cst::ExprKind::Object(fields) => {
            let field_ids = fields
                .into_iter()
                .map(|(k, v)| lower_expr(ast, v).map(|id| (k, id)))
                .collect::<Result<Vec<_>>>()?;
            Expr::Object(field_ids)
        }
        cst::ExprKind::Array(elems) => {
            let elem_ids = elems
                .into_iter()
                .map(|e| lower_expr(ast, e))
                .collect::<Result<Vec<_>>>()?;
            Expr::Array(elem_ids)
        }
        cst::ExprKind::Tuple(elems) => {
            let elem_ids = elems
                .into_iter()
                .map(|e| lower_expr(ast, e))
                .collect::<Result<SmallVec<_>>>()?;
            Expr::Tuple(elem_ids)
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
            Expr::Is(inner_id, pattern)
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
        cst::ExprKind::Closure { params, ret, body } => {
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
                params: params_lowered,
                ret: ret_id,
                body: body_id,
            }
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
    };
    ast.add_type_expr(te, span)
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
