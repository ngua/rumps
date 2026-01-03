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
