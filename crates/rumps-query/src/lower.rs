//! Lowering pass: CST to AST.
//!
//! Converts the boxed CST representation to the arena-allocated AST.
//! This is a straightforward recursive traversal with direct `&mut Ast` access;
//! no `Rc` or `RefCell` required.

use smallvec::SmallVec;

use crate::ast::{Ast, AstTypeExpr, AstTypeExprId, Expr, ExprId, Stmt, StmtId};
use crate::cst::{
    CstExpr, CstExprKind, CstStmt, CstStmtKind, CstTypeExpr, CstTypeExprKind,
};

/// Lower a CST program (list of statements) to AST.
pub(crate) fn lower_program(stmts: Vec<CstStmt>) -> (Ast, Vec<StmtId>) {
    let mut ast = Ast::new();
    let ids = stmts.into_iter().map(|s| lower_stmt(&mut ast, s)).collect();
    (ast, ids)
}

/// Lower a CST statement to AST.
fn lower_stmt(ast: &mut Ast, stmt: CstStmt) -> StmtId {
    let span = stmt.span;
    let s = match stmt.kind {
        CstStmtKind::Let(name, ty, expr) => {
            let ty_id = ty.map(|t| lower_type_expr(ast, t));
            let expr_id = lower_expr(ast, expr);
            Stmt::Let(name, ty_id, expr_id)
        }
        CstStmtKind::Set(target, value) => {
            let target_id = lower_expr(ast, target);
            let value_id = lower_expr(ast, value);
            Stmt::Set(target_id, value_id)
        }
        CstStmtKind::Kill(target) => {
            let target_id = lower_expr(ast, target);
            Stmt::Kill(target_id)
        }
        CstStmtKind::Output(expr) => {
            let expr_id = lower_expr(ast, expr);
            Stmt::Output(expr_id)
        }
        CstStmtKind::Expr(expr) => {
            let expr_id = lower_expr(ast, expr);
            Stmt::Expr(expr_id)
        }
        CstStmtKind::Fun {
            name,
            params,
            ret,
            body,
        } => {
            let params_lowered = params
                .into_iter()
                .map(|(n, t)| (n, t.map(|te| lower_type_expr(ast, te))))
                .collect();
            let ret_id = ret.map(|t| lower_type_expr(ast, t));
            let body_id = lower_expr(ast, body);
            Stmt::Fun {
                name,
                params: params_lowered,
                ret: ret_id,
                body: body_id,
            }
        }
    };
    ast.add_stmt(s, span)
}

/// Lower a CST expression to AST.
fn lower_expr(ast: &mut Ast, expr: CstExpr) -> ExprId {
    let span = expr.span;
    let e = match expr.kind {
        CstExprKind::Literal(lit) => Expr::Literal(lit),
        CstExprKind::Var(name) => Expr::Var(name),
        CstExprKind::Local(name, subs) => {
            let sub_ids = lower_exprs(ast, subs);
            Expr::Local(name, sub_ids)
        }
        CstExprKind::Global(name, subs) => {
            let sub_ids = lower_exprs(ast, subs);
            Expr::Global(name, sub_ids)
        }
        CstExprKind::Get(inner) => {
            let inner_id = lower_expr(ast, *inner);
            Expr::Get(inner_id)
        }
        CstExprKind::Binary(lhs, op, rhs) => {
            let lhs_id = lower_expr(ast, *lhs);
            let rhs_id = lower_expr(ast, *rhs);
            Expr::Binary(lhs_id, op, rhs_id)
        }
        CstExprKind::Unary(op, operand) => {
            let operand_id = lower_expr(ast, *operand);
            Expr::Unary(op, operand_id)
        }
        CstExprKind::Call(callee, args) => {
            let callee_id = lower_expr(ast, *callee);
            let arg_ids = lower_exprs(ast, args);
            Expr::Call(callee_id, arg_ids)
        }
        CstExprKind::Object(fields) => {
            let field_ids = fields
                .into_iter()
                .map(|(k, v)| (k, lower_expr(ast, v)))
                .collect();
            Expr::Object(field_ids)
        }
        CstExprKind::Array(elems) => {
            let elem_ids =
                elems.into_iter().map(|e| lower_expr(ast, e)).collect();
            Expr::Array(elem_ids)
        }
        CstExprKind::Index(base, idx) => {
            let base_id = lower_expr(ast, *base);
            let idx_id = lower_expr(ast, *idx);
            Expr::Index(base_id, idx_id)
        }
        CstExprKind::Field(base, field) => {
            let base_id = lower_expr(ast, *base);
            Expr::Field(base_id, field)
        }
        CstExprKind::OptionalField(base, field) => {
            let base_id = lower_expr(ast, *base);
            Expr::OptionalField(base_id, field)
        }
        CstExprKind::Variant(ty, var, args) => {
            let arg_ids = lower_exprs(ast, args);
            Expr::Variant(ty, var, arg_ids)
        }
        CstExprKind::Is(inner, pattern) => {
            let inner_id = lower_expr(ast, *inner);
            Expr::Is(inner_id, pattern)
        }
        CstExprKind::As(inner, ty) => {
            let inner_id = lower_expr(ast, *inner);
            let ty_id = lower_type_expr(ast, ty);
            Expr::As(inner_id, ty_id)
        }
        CstExprKind::Read(inner, ty) => {
            let inner_id = lower_expr(ast, *inner);
            let ty_id = lower_type_expr(ast, ty);
            Expr::Read(inner_id, ty_id)
        }
        CstExprKind::Block(stmts, tail) => {
            let stmt_ids =
                stmts.into_iter().map(|s| lower_stmt(ast, s)).collect();
            let tail_id = tail.map(|e| lower_expr(ast, *e));
            Expr::Block(stmt_ids, tail_id)
        }
        CstExprKind::If(cond, then_br, else_br) => {
            let cond_id = lower_expr(ast, *cond);
            let then_id = lower_expr(ast, *then_br);
            let else_id = else_br.map(|e| lower_expr(ast, *e));
            Expr::If(cond_id, then_id, else_id)
        }
        CstExprKind::Closure { params, ret, body } => {
            let params_lowered = params
                .into_iter()
                .map(|(n, t)| (n, t.map(|te| lower_type_expr(ast, te))))
                .collect();
            let ret_id = ret.map(|t| lower_type_expr(ast, t));
            let body_id = lower_expr(ast, *body);
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
fn lower_exprs(ast: &mut Ast, exprs: Vec<CstExpr>) -> SmallVec<[ExprId; 4]> {
    exprs.into_iter().map(|e| lower_expr(ast, e)).collect()
}

/// Lower a CST type expression to AST.
fn lower_type_expr(ast: &mut Ast, ty: CstTypeExpr) -> AstTypeExprId {
    let span = ty.span;
    let te = match ty.kind {
        CstTypeExprKind::Named(name) => AstTypeExpr::Named(name),
        CstTypeExprKind::App(name, params) => {
            let param_ids = params
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect();
            AstTypeExpr::App(name, param_ids)
        }
        CstTypeExprKind::Fn(params, ret) => {
            let param_ids = params
                .into_iter()
                .map(|t| lower_type_expr(ast, t))
                .collect();
            let ret_id = lower_type_expr(ast, *ret);
            AstTypeExpr::Fn(param_ids, ret_id)
        }
    };
    ast.add_type_expr(te, span)
}
