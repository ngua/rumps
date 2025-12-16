//! AST definitions for the RUMPS query language.
//!
//! Uses arena allocation with indices instead of `Box` for cache-friendliness
//! and to avoid deep pointer chains. Spans are stored in parallel vectors
//! for cache efficiency; the interpreter rarely needs spans during execution.

#![allow(dead_code)]

use smallvec::SmallVec;

use crate::Span;

/// Index into the expression arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct ExprId(u32);

/// Index into the statement arena.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub(crate) struct StmtId(u32);

impl ExprId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

impl StmtId {
    /// The raw index value.
    pub(crate) const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// The AST arena; owns all expressions and statements.
///
/// Spans are stored in parallel vectors rather than inline for cache
/// efficiency; they're only accessed for error reporting.
#[derive(Clone, Debug, Default)]
pub(crate) struct Ast {
    exprs: Vec<Expr>,
    expr_spans: Vec<Span>,
    stmts: Vec<Stmt>,
    stmt_spans: Vec<Span>,
}

impl Ast {
    /// Create an empty AST.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Add an expression to the arena.
    pub(crate) fn add_expr(&mut self, e: Expr, span: Span) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(e);
        self.expr_spans.push(span);
        id
    }

    /// Add a statement to the arena.
    pub(crate) fn add_stmt(&mut self, s: Stmt, span: Span) -> StmtId {
        let id = StmtId(self.stmts.len() as u32);
        self.stmts.push(s);
        self.stmt_spans.push(span);
        id
    }

    /// Get an expression by ID.
    pub(crate) fn get_expr(&self, id: ExprId) -> Option<&Expr> {
        self.exprs.get(id.idx())
    }

    /// Get a statement by ID.
    pub(crate) fn get_stmt(&self, id: StmtId) -> Option<&Stmt> {
        self.stmts.get(id.idx())
    }

    /// Get the span of an expression.
    pub(crate) fn expr_span(&self, id: ExprId) -> Option<Span> {
        self.expr_spans.get(id.idx()).copied()
    }

    /// Get the span of a statement.
    pub(crate) fn stmt_span(&self, id: StmtId) -> Option<Span> {
        self.stmt_spans.get(id.idx()).copied()
    }

    /// Number of expressions in the arena.
    pub(crate) fn expr_count(&self) -> usize {
        self.exprs.len()
    }

    /// Number of statements in the arena.
    pub(crate) fn stmt_count(&self) -> usize {
        self.stmts.len()
    }
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    // Arithmetic
    Add,      // `+`
    Sub,      // `-`
    Mul,      // `*`
    Div,      // `/`
    FloorDiv, // `//`
    Mod,      // `%`

    // Comparison
    Eq, // `==`
    Ne, // `!=`
    Lt, // `<`
    Gt, // `>`
    Le, // `<=`
    Ge, // `>=`

    // Logical
    And, // `AND` or `&&`
    Or,  // `OR` or `||`

    // String
    Concat, // `++`
}

/// Unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnOp {
    Neg, // `-`
    Not, // `NOT` or `!`
}

/// A literal value in the AST.
///
/// This is the compile-time representation; runtime values (with arena
/// allocation and string interning) are defined separately in the `value`
/// module.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Literal {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
}

/// An expression node.
///
/// All recursive references use `ExprId` indices into the `Ast` arena.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Expr {
    /// A literal value.
    Literal(Literal),

    /// A local variable reference.
    Local(String),

    /// A global variable reference with subscripts.
    ///
    /// `^PATIENT(123, "NAME")` becomes `Global("PATIENT", [123, "NAME"])`.
    /// This is an implicit GET; reading a global alone evaluates to its value.
    Global(String, SmallVec<[ExprId; 4]>),

    /// A binary operation.
    Binary(ExprId, BinOp, ExprId),

    /// A unary operation.
    Unary(UnOp, ExprId),

    /// A function call.
    ///
    /// Most functions have 0-4 arguments, so `SmallVec` avoids heap allocation.
    Call(String, SmallVec<[ExprId; 4]>),

    /// An object/record literal: `{ key: value, ... }`.
    Object(Vec<(String, ExprId)>),

    /// An array literal: `[expr, ...]`.
    Array(Vec<ExprId>),

    /// Index access: `expr[index]`.
    Index(ExprId, ExprId),

    /// Field access: `expr.field`.
    Field(ExprId, String),

    /// A block expression: `{ stmt...; expr }`.
    ///
    /// Executes statements for side effects, then evaluates to the trailing
    /// expression. If no trailing expression, evaluates to `Option.None`.
    Block(Vec<StmtId>, Option<ExprId>),

    /// Conditional expression: `IF cond { then } ELSE { else }`.
    ///
    /// Evaluates to the value of the taken branch. If no else branch and
    /// condition is false, evaluates to `Option.None`.
    If(ExprId, ExprId, Option<ExprId>),
}

/// A statement node.
///
/// All recursive references use `ExprId`/`StmtId` indices into the `Ast` arena.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stmt {
    /// Lexical binding (sync, not subscriptable): `LET x = expr`.
    Let(String, ExprId),

    /// Local B-tree assignment: `SET x = expr` or `SET x(subs...) = expr`.
    Set(String, SmallVec<[ExprId; 4]>, ExprId),

    /// Global assignment: `SET ^NAME(subs...) = expr`.
    SetGlobal(String, SmallVec<[ExprId; 4]>, ExprId),

    /// Delete a variable or subtree: `KILL x` or `KILL ^NAME(subs...)`.
    Kill(String, SmallVec<[ExprId; 4]>),

    /// Delete a global: `KILL ^NAME(subs...)`.
    KillGlobal(String, SmallVec<[ExprId; 4]>),

    /// Output a value: `OUTPUT expr`.
    Output(ExprId),

    /// An expression used as a statement (for side effects).
    ///
    /// This is the canonical way to use `Expr::If` and `Expr::Block` as statements.
    Expr(ExprId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arena_basic() {
        let mut ast = Ast::new();

        let lit =
            ast.add_expr(Expr::Literal(Literal::Int(42)), Span::new(0, 2));
        let var = ast.add_expr(Expr::Local("x".into()), Span::new(4, 5));

        assert_eq!(ast.expr_count(), 2);
        assert_eq!(ast.get_expr(lit), Some(&Expr::Literal(Literal::Int(42))));
        assert_eq!(ast.get_expr(var), Some(&Expr::Local("x".into())));
        assert_eq!(ast.expr_span(lit), Some(Span::new(0, 2)));
        assert_eq!(ast.expr_span(var), Some(Span::new(4, 5)));
    }

    #[test]
    fn arena_binary_expr() {
        let mut ast = Ast::new();

        // Build: 1 + 2
        let lhs = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(0, 1));
        let rhs = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(4, 5));
        let add =
            ast.add_expr(Expr::Binary(lhs, BinOp::Add, rhs), Span::new(0, 5));

        assert_eq!(ast.expr_count(), 3);
        assert_eq!(
            ast.get_expr(add),
            Some(&Expr::Binary(lhs, BinOp::Add, rhs))
        );
    }

    #[test]
    fn arena_statements() {
        let mut ast = Ast::new();

        // Build: LET x = 10
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(10)), Span::new(8, 10));
        let stmt = ast.add_stmt(Stmt::Let("x".into(), val), Span::new(0, 10));

        assert_eq!(ast.stmt_count(), 1);
        assert_eq!(ast.get_stmt(stmt), Some(&Stmt::Let("x".into(), val)));
        assert_eq!(ast.stmt_span(stmt), Some(Span::new(0, 10)));
    }

    #[test]
    fn arena_global_with_subscripts() {
        let mut ast = Ast::new();

        // Build: ^PATIENT(123, "NAME")
        let sub1 =
            ast.add_expr(Expr::Literal(Literal::Int(123)), Span::new(9, 12));
        let sub2 = ast.add_expr(
            Expr::Literal(Literal::String("NAME".into())),
            Span::new(14, 20),
        );
        let global = ast.add_expr(
            Expr::Global("PATIENT".into(), smallvec::smallvec![sub1, sub2]),
            Span::new(0, 21),
        );

        assert_eq!(ast.expr_count(), 3);
        match ast.get_expr(global) {
            Some(Expr::Global(name, subs)) => {
                assert_eq!(name, "PATIENT");
                assert_eq!(subs.len(), 2);
            }
            _ => panic!("expected Global"),
        }
    }

    #[test]
    fn arena_if_expr() {
        let mut ast = Ast::new();

        // Build: IF x > 0 { 1 } ELSE { 0 }
        let x = ast.add_expr(Expr::Local("x".into()), Span::new(3, 4));
        let zero =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(7, 8));
        let cond =
            ast.add_expr(Expr::Binary(x, BinOp::Gt, zero), Span::new(3, 8));

        let one =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(12, 13));
        let then_blk =
            ast.add_expr(Expr::Block(vec![], Some(one)), Span::new(10, 15));

        let zero2 =
            ast.add_expr(Expr::Literal(Literal::Int(0)), Span::new(23, 24));
        let else_blk =
            ast.add_expr(Expr::Block(vec![], Some(zero2)), Span::new(21, 26));

        let if_expr = ast.add_expr(
            Expr::If(cond, then_blk, Some(else_blk)),
            Span::new(0, 26),
        );

        assert_eq!(ast.expr_count(), 8);
        match ast.get_expr(if_expr) {
            Some(Expr::If(c, then_br, else_br)) => {
                assert_eq!(*c, cond);
                assert_eq!(*then_br, then_blk);
                assert_eq!(*else_br, Some(else_blk));
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn arena_nested_binary() {
        let mut ast = Ast::new();

        // Build: (1 + 2) * 3
        let one = ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(1, 2));
        let two = ast.add_expr(Expr::Literal(Literal::Int(2)), Span::new(5, 6));
        let add =
            ast.add_expr(Expr::Binary(one, BinOp::Add, two), Span::new(1, 6));

        let three =
            ast.add_expr(Expr::Literal(Literal::Int(3)), Span::new(10, 11));
        let mul = ast
            .add_expr(Expr::Binary(add, BinOp::Mul, three), Span::new(0, 11));

        assert_eq!(ast.expr_count(), 5);

        // Verify structure
        match ast.get_expr(mul) {
            Some(Expr::Binary(lhs, BinOp::Mul, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Binary(one, BinOp::Add, two))
                );
                assert_eq!(
                    ast.get_expr(*rhs),
                    Some(&Expr::Literal(Literal::Int(3)))
                );
            }
            _ => panic!("expected Binary Mul"),
        }
    }

    #[test]
    fn arena_set_with_subscripts() {
        let mut ast = Ast::new();

        // Build: SET x(1, "ABC") = 30
        let sub1 =
            ast.add_expr(Expr::Literal(Literal::Int(1)), Span::new(6, 7));
        let sub2 = ast.add_expr(
            Expr::Literal(Literal::String("ABC".into())),
            Span::new(9, 14),
        );
        let val =
            ast.add_expr(Expr::Literal(Literal::Int(30)), Span::new(18, 20));

        let stmt = ast.add_stmt(
            Stmt::Set("x".into(), smallvec::smallvec![sub1, sub2], val),
            Span::new(0, 20),
        );

        match ast.get_stmt(stmt) {
            Some(Stmt::Set(name, subs, v)) => {
                assert_eq!(name, "x");
                assert_eq!(subs.len(), 2);
                assert_eq!(*v, val);
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn arena_out_of_bounds() {
        let ast = Ast::new();
        assert_eq!(ast.get_expr(ExprId(999)), None);
        assert_eq!(ast.get_stmt(StmtId(999)), None);
        assert_eq!(ast.expr_span(ExprId(999)), None);
        assert_eq!(ast.stmt_span(StmtId(999)), None);
    }

    #[test]
    fn literal_variants() {
        assert_eq!(Literal::Bool(true), Literal::Bool(true));
        assert_eq!(Literal::Int(42), Literal::Int(42));
        assert_eq!(Literal::Float(3.14), Literal::Float(3.14));
        assert_eq!(
            Literal::String("hello".into()),
            Literal::String("hello".into())
        );
    }
}
