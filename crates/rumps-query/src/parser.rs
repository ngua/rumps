//! Parser for the RUMPS query language.
//!
//! Transforms a token stream into an AST using chumsky. Uses `RefCell` to build
//! the arena-allocated AST during parsing.

#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use chumsky::prelude::{choice, end, just, recursive, select, Simple};
use chumsky::Parser as _;
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use crate::{
    Ast, BinOp, Error, Expr, ExprId, Lexer, Literal, Result, Span, Spanned,
    Stmt, StmtId, Token, UnOp,
};

/// Shared mutable AST arena for use during parsing.
type AstCell = Rc<RefCell<Ast>>;

/// Parser error type for token-based parsing.
type ParseErr = Simple<Token, Span>;

/// Parser output paired with its span.
type SpannedExpr = (ExprId, Span);
type SpannedStmt = (StmtId, Span);

/// The result of parsing: the AST arena and the top-level statements.
#[derive(Debug)]
pub(crate) struct ParseResult {
    pub ast: Ast,
    pub stmts: Vec<StmtId>,
}

/// Parses source code into an AST.
pub(crate) struct Parser;

impl Parser {
    /// Parse source code into an AST.
    ///
    /// Returns the AST arena and a list of top-level statement IDs.
    pub(crate) fn parse(src: &str) -> Result<ParseResult> {
        let tokens = Lexer::new(src).lex()?;
        Self::parse_tokens(&tokens)
    }

    /// Parse a token stream into an AST.
    pub(crate) fn parse_tokens(tokens: &[Spanned]) -> Result<ParseResult> {
        let ast = Rc::new(RefCell::new(Ast::new()));
        let parser = Self::program(Rc::clone(&ast));

        // Find EOF span for chumsky's end-of-input handling
        let eof_span = tokens
            .iter()
            .find_map(|s| matches!(s.tok, Token::Eof).then_some(s.span))
            .unwrap_or(Span::new(0, 0));

        // Filter out EOF token; chumsky handles end-of-input separately
        let stream = chumsky::Stream::from_iter(
            eof_span,
            tokens
                .iter()
                .filter(|s| !matches!(s.tok, Token::Eof))
                .map(|s| (s.tok.clone(), s.span)),
        );

        parser
            .parse(stream)
            .map_err(|errs| {
                NonEmpty::collect(errs.into_iter().map(Into::into))
                    .map(Error::multiple)
                    .unwrap_or_else(|| {
                        Error::runtime_no_span("unknown parse error")
                    })
            })
            .map(|stmts| {
                // Extract the AST; try_unwrap if we're the only owner, otherwise take inner
                let inner = Rc::try_unwrap(ast)
                    .map(RefCell::into_inner)
                    .unwrap_or_else(|rc| rc.borrow().clone());
                ParseResult { ast: inner, stmts }
            })
    }

    /// Program: zero or more statements separated by newlines, ending with EOF.
    fn program(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, Vec<StmtId>, Error = ParseErr> {
        Self::opt_newlines()
            .ignore_then(
                Self::stmt(Rc::clone(&ast))
                    .map(|(id, _)| id)
                    .separated_by(Self::newlines())
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(end())
    }

    /// One or more newlines (skipping indentation tokens for now).
    fn newlines() -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        Self::newline_or_indent().repeated().at_least(1).ignored()
    }

    /// Optional newlines.
    fn opt_newlines(
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        Self::newline_or_indent().repeated().ignored()
    }

    /// A single newline or indentation token.
    fn newline_or_indent(
    ) -> impl chumsky::Parser<Token, Token, Error = ParseErr> + Clone {
        choice((
            just(Token::Newline),
            just(Token::Indent),
            just(Token::Dedent),
        ))
    }

    /// A single statement.
    fn stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        recursive(|stmt| {
            let let_stmt = Self::let_stmt(Rc::clone(&ast));
            let set_stmt = Self::set_stmt(Rc::clone(&ast));
            let kill_stmt = Self::kill_stmt(Rc::clone(&ast));
            let output_stmt = Self::output_stmt(Rc::clone(&ast));
            let if_stmt = Self::if_stmt(Rc::clone(&ast), stmt);
            let expr_stmt = Self::expr_stmt(Rc::clone(&ast));

            choice((
                let_stmt,
                set_stmt,
                kill_stmt,
                output_stmt,
                if_stmt,
                expr_stmt,
            ))
        })
    }

    /// `LET name = expr`
    fn let_stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        just(Token::Let)
            .ignore_then(Self::ident())
            .then_ignore(just(Token::Assign))
            .then(Self::expr(Rc::clone(&ast)))
            .map_with_span(move |(name, (val_id, _)), span| {
                let id =
                    ast.borrow_mut().add_stmt(Stmt::Let(name, val_id), span);
                (id, span)
            })
    }

    /// `SET name = expr` or `SET name(subs...) = expr`
    /// `SET ^global = expr` or `SET ^global(subs...) = expr`
    fn set_stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast4 = Rc::clone(&ast);
        let ast5 = Rc::clone(&ast);

        let local_set = just(Token::Set)
            .ignore_then(Self::ident())
            .then(Self::subscripts(Self::expr(Rc::clone(&ast))).or_not())
            .then_ignore(just(Token::Assign))
            .then(Self::expr(Rc::clone(&ast2)))
            .map_with_span(move |((name, subs), (val_id, _)), span| {
                let subs = subs.unwrap_or_default();
                let id = ast3
                    .borrow_mut()
                    .add_stmt(Stmt::Set(name, subs, val_id), span);
                (id, span)
            });

        let global_set = just(Token::Set)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(Self::expr(Rc::clone(&ast4))).or_not())
            .then_ignore(just(Token::Assign))
            .then(Self::expr(Rc::clone(&ast5)))
            .map_with_span(move |((name, subs), (val_id, _)), span| {
                let subs = subs.unwrap_or_default();
                let id = ast5
                    .borrow_mut()
                    .add_stmt(Stmt::SetGlobal(name, subs, val_id), span);
                (id, span)
            });

        global_set.or(local_set)
    }

    /// `KILL name` or `KILL name(subs...)`
    /// `KILL ^global` or `KILL ^global(subs...)`
    fn kill_stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast4 = Rc::clone(&ast);

        let local_kill = just(Token::Kill)
            .ignore_then(Self::ident())
            .then(Self::subscripts(Self::expr(Rc::clone(&ast))).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let id =
                    ast2.borrow_mut().add_stmt(Stmt::Kill(name, subs), span);
                (id, span)
            });

        let global_kill = just(Token::Kill)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(Self::expr(Rc::clone(&ast3))).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let id = ast4
                    .borrow_mut()
                    .add_stmt(Stmt::KillGlobal(name, subs), span);
                (id, span)
            });

        global_kill.or(local_kill)
    }

    /// `OUTPUT expr`
    fn output_stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        just(Token::Output)
            .ignore_then(Self::expr(Rc::clone(&ast)))
            .map_with_span(move |(expr_id, _), span| {
                let id = ast.borrow_mut().add_stmt(Stmt::Output(expr_id), span);
                (id, span)
            })
    }

    /// `IF cond { block } [ELSE { block }]`
    ///
    /// Parses an IF expression and wraps it in `Stmt::Expr`. The branches are
    /// block expressions (`Expr::Block`).
    fn if_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> + Clone,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let block = Self::block(stmt);

        just(Token::If)
            .ignore_then(Self::expr(Rc::clone(&ast)))
            .then(block.clone())
            .then(
                just(Token::Else)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(block)
                    .or_not(),
            )
            .map_with_span(
                move |((cond, (then_stmts, then_span)), else_block), span| {
                    let mut ast_mut = ast.borrow_mut();

                    // Convert then-block to Expr::Block
                    let then_expr = Self::stmts_to_block(
                        &mut ast_mut,
                        then_stmts,
                        then_span,
                    );

                    // Convert else-block to Expr::Block if present
                    let else_expr = else_block.map(|(stmts, blk_span)| {
                        Self::stmts_to_block(&mut ast_mut, stmts, blk_span)
                    });

                    // Create IF expression
                    let if_expr = ast_mut
                        .add_expr(Expr::If(cond.0, then_expr, else_expr), span);

                    // Wrap in Stmt::Expr
                    let id = ast_mut.add_stmt(Stmt::Expr(if_expr), span);
                    (id, span)
                },
            )
    }

    /// Convert a list of statements to a block expression.
    ///
    /// If the last statement is `Stmt::Expr(e)`, extracts `e` as the trailing
    /// expression (block's value). Otherwise, the block has no trailing expr.
    fn stmts_to_block(
        ast: &mut Ast,
        mut stmts: Vec<StmtId>,
        span: Span,
    ) -> ExprId {
        // Check if last statement is Stmt::Expr; if so, use it as tail
        let tail = stmts.last().and_then(|&last_id| {
            ast.get_stmt(last_id).and_then(|s| match s {
                Stmt::Expr(e) => Some(*e),
                _ => None,
            })
        });

        // If we found a tail, remove the last statement
        let tail = tail.inspect(|_| {
            stmts.pop();
        });

        ast.add_expr(Expr::Block(stmts, tail), span)
    }

    /// `{ stmts... }` block, returns statements and the block's span.
    fn block(
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> + Clone,
    ) -> impl chumsky::Parser<Token, (Vec<StmtId>, Span), Error = ParseErr> + Clone
    {
        Self::opt_newlines()
            .ignore_then(just(Token::LBrace))
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                stmt.map(|(id, _)| id)
                    .separated_by(Self::newlines())
                    .allow_leading()
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|stmts, span| (stmts, span))
    }

    /// Expression used as statement.
    fn expr_stmt(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        Self::expr(Rc::clone(&ast)).map_with_span(move |(expr_id, _), span| {
            let id = ast.borrow_mut().add_stmt(Stmt::Expr(expr_id), span);
            (id, span)
        })
    }

    /// Top-level expression parser with full precedence.
    ///
    /// Uses `recursive` at the top level to handle cyclic references in the
    /// grammar (e.g., parenthesized expressions, array elements, etc.).
    fn expr(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        recursive(|expr| Self::expr_inner(ast, expr))
    }

    /// Inner expression parser that takes the recursive reference.
    fn expr_inner(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        Self::or_expr(ast, expr)
    }

    /// Logical OR: `expr || expr` or `expr OR expr`
    fn or_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((just(Token::PipePipe), just(Token::Or))).to(BinOp::Or);

        Self::and_expr(Rc::clone(&ast), expr.clone())
            .then(op.then(Self::and_expr(Rc::clone(&ast), expr)).repeated())
            .map_with_span(move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            })
    }

    /// Logical AND: `expr && expr` or `expr AND expr`
    fn and_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((just(Token::AmpAmp), just(Token::And))).to(BinOp::And);

        Self::cmp_expr(Rc::clone(&ast), expr.clone())
            .then(op.then(Self::cmp_expr(Rc::clone(&ast), expr)).repeated())
            .map_with_span(move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            })
    }

    /// Comparison: `<`, `>`, `<=`, `>=`, `==`, `!=`
    fn cmp_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((
            just(Token::Eq).to(BinOp::Eq),
            just(Token::Ne).to(BinOp::Ne),
            just(Token::Le).to(BinOp::Le),
            just(Token::Ge).to(BinOp::Ge),
            just(Token::Lt).to(BinOp::Lt),
            just(Token::Gt).to(BinOp::Gt),
        ));

        Self::add_expr(Rc::clone(&ast), expr.clone())
            .then(op.then(Self::add_expr(Rc::clone(&ast), expr)).repeated())
            .map_with_span(move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            })
    }

    /// Additive: `+`, `-`, `++`
    fn add_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((
            just(Token::Plus).to(BinOp::Add),
            just(Token::Minus).to(BinOp::Sub),
            just(Token::Concat).to(BinOp::Concat),
        ));

        Self::mul_expr(Rc::clone(&ast), expr.clone())
            .then(op.then(Self::mul_expr(Rc::clone(&ast), expr)).repeated())
            .map_with_span(move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            })
    }

    /// Multiplicative: `*`, `/`, `//`, `%`
    fn mul_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((
            just(Token::Mul).to(BinOp::Mul),
            just(Token::FloorDiv).to(BinOp::FloorDiv),
            just(Token::Div).to(BinOp::Div),
            just(Token::Modulo).to(BinOp::Mod),
        ));

        Self::unary_expr(Rc::clone(&ast), expr.clone())
            .then(op.then(Self::unary_expr(Rc::clone(&ast), expr)).repeated())
            .map_with_span(move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            })
    }

    /// Folds a sequence of binary operations left-to-right.
    fn fold_binary(
        ast: &AstCell,
        first: SpannedExpr,
        rest: Vec<(BinOp, SpannedExpr)>,
        _outer_span: Span,
    ) -> SpannedExpr {
        rest.into_iter().fold(first, |lhs, (op, rhs)| {
            let span = lhs.1.merge(rhs.1);
            let id = ast
                .borrow_mut()
                .add_expr(Expr::Binary(lhs.0, op, rhs.0), span);
            (id, span)
        })
    }

    /// Unary: `NOT`, `!`, `-`
    fn unary_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((
            just(Token::Not).to(UnOp::Not),
            just(Token::Bang).to(UnOp::Not),
            just(Token::Minus).to(UnOp::Neg),
        ));

        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);

        // Unary is right-associative, so we use recursion
        recursive(move |unary| {
            let ast_inner = Rc::clone(&ast);
            let ast_get = Rc::clone(&ast3);
            let with_op = op.clone().then(unary.clone()).map_with_span(
                move |(op, (inner, _)): (UnOp, SpannedExpr), span| {
                    let id = ast_inner
                        .borrow_mut()
                        .add_expr(Expr::Unary(op, inner), span);
                    (id, span)
                },
            );

            // GET target: reads from a B-tree variable (local or global)
            let get_expr = just(Token::Get)
                .ignore_then(Self::gettable(Rc::clone(&ast_get), expr.clone()))
                .map_with_span(move |(inner, _), span| {
                    let id =
                        ast_get.borrow_mut().add_expr(Expr::Get(inner), span);
                    (id, span)
                });

            choice((with_op, get_expr))
                .or(Self::postfix_expr(Rc::clone(&ast2), expr.clone()))
        })
    }

    /// Target for `GET`: a local or global B-tree variable.
    fn gettable(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let ast2 = Rc::clone(&ast);

        // Global: `^NAME` or `^NAME(subs...)`
        let global = Self::global_name()
            .then(Self::subscripts(expr.clone()).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let id =
                    ast.borrow_mut().add_expr(Expr::Global(name, subs), span);
                (id, span)
            });

        // Local: `name` or `name(subs...)`
        let local = Self::ident()
            .then(Self::subscripts(expr).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let id =
                    ast2.borrow_mut().add_expr(Expr::Local(name, subs), span);
                (id, span)
            });

        choice((global, local))
    }

    /// Postfix: field access `.field`, index `[expr]`, call `(args...)`
    fn postfix_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let ast2 = Rc::clone(&ast);

        // Field access: `.field`
        let field = just(Token::Dot)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::Field);

        // Index: `[expr]`
        let index = just(Token::LBracket)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|(idx, _), span| PostfixOp::Index(idx, span));

        // Call: `(args...)`
        let call = just(Token::LParen)
            .ignore_then(
                expr.clone()
                    .map(|(id, _)| id)
                    .separated_by(just(Token::Comma))
                    .allow_trailing(),
            )
            .then_ignore(just(Token::RParen))
            .map_with_span(|args, span| {
                PostfixOp::Call(SmallVec::from_vec(args), span)
            });

        let postfix_op = choice((field, index, call));

        Self::primary_expr(Rc::clone(&ast), expr)
            .then(postfix_op.repeated())
            .map_with_span(|x, span| (x, span))
            .try_map(move |((base, ops), span), _| {
                Self::fold_postfix(&ast2, base, ops).ok_or_else(|| {
                    Simple::custom(span, "function call requires identifier")
                })
            })
    }

    /// Folds postfix operations left-to-right.
    ///
    /// Returns `None` if a call is applied to a non-identifier (parse error).
    fn fold_postfix(
        ast: &AstCell,
        base: SpannedExpr,
        ops: Vec<PostfixOp>,
    ) -> Option<SpannedExpr> {
        ops.into_iter().try_fold(base, |acc, op| {
            let span = Span::new(acc.1.start, op.end().end);
            match op {
                PostfixOp::Field(name, _) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::Field(acc.0, name), span);
                    Some((id, span))
                }
                PostfixOp::Index(idx, _) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::Index(acc.0, idx), span);
                    Some((id, span))
                }
                PostfixOp::Call(args, _) => {
                    // Convert the base expression to a function call if it's a Var
                    let base_expr = ast.borrow().get_expr(acc.0).cloned();
                    base_expr.and_then(|e| match e {
                        Expr::Var(name) => {
                            let id = ast
                                .borrow_mut()
                                .add_expr(Expr::Call(name, args), span);
                            Some((id, span))
                        }
                        _ => None, // Not a simple identifier; parse error
                    })
                }
            }
        })
    }

    /// Primary: literals, identifiers, globals, parenthesized, arrays, objects.
    fn primary_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast5 = Rc::clone(&ast);
        let ast6 = Rc::clone(&ast);

        // Literals
        let int_lit = select! { Token::Int(n) => Literal::Int(n) };
        let float_lit =
            select! { Token::Float(OrderedFloat(n)) => Literal::Float(n) };
        let str_lit = select! { Token::String(s) => Literal::String(s) };
        let bool_lit = choice((
            just(Token::True).to(Literal::Bool(true)),
            just(Token::False).to(Literal::Bool(false)),
        ));

        let literal = choice((int_lit, float_lit, str_lit, bool_lit))
            .map_with_span(move |lit, span| {
                let id = ast.borrow_mut().add_expr(Expr::Literal(lit), span);
                (id, span)
            });

        // Lexical variable (LET bindings)
        let var = Self::ident().map_with_span(move |name, span| {
            let id = ast2.borrow_mut().add_expr(Expr::Var(name), span);
            (id, span)
        });

        // Global with optional subscripts: `^NAME` or `^NAME(subs...)`
        let global = Self::global_name()
            .then(Self::subscripts(expr.clone()).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let id =
                    ast3.borrow_mut().add_expr(Expr::Global(name, subs), span);
                (id, span)
            });

        // Parenthesized expression
        let paren = just(Token::LParen)
            .ignore_then(expr.clone())
            .then_ignore(just(Token::RParen));

        // Array literal: `[expr, ...]`
        let array = just(Token::LBracket)
            .ignore_then(
                expr.clone()
                    .map(|(id, _)| id)
                    .separated_by(just(Token::Comma))
                    .allow_trailing(),
            )
            .then_ignore(just(Token::RBracket))
            .map_with_span(move |elems, span| {
                let id = ast5.borrow_mut().add_expr(Expr::Array(elems), span);
                (id, span)
            });

        // Object literal: `{ key: value, ... }`
        let obj_field = Self::ident()
            .then_ignore(just(Token::Colon))
            .then(expr.map(|(id, _)| id));

        let object = just(Token::LBrace)
            .ignore_then(
                obj_field.separated_by(just(Token::Comma)).allow_trailing(),
            )
            .then_ignore(just(Token::RBrace))
            .map_with_span(move |fields, span| {
                let id = ast6.borrow_mut().add_expr(Expr::Object(fields), span);
                (id, span)
            });

        // Order matters: try global before var (both can start with ident pattern)
        choice((literal, global, var, paren, array, object))
    }

    /// Parse an identifier token.
    fn ident() -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone
    {
        select! { Token::Ident(s) => s }
    }

    /// Parse a global variable name (without the `^` prefix, which is in the token).
    fn global_name(
    ) -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone {
        select! { Token::Global(s) => s }
    }

    /// Parse subscripts: `(expr, expr, ...)`
    fn subscripts(
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SmallVec<[ExprId; 4]>, Error = ParseErr> + Clone
    {
        just(Token::LParen)
            .ignore_then(
                expr.map(|(id, _)| id)
                    .separated_by(just(Token::Comma))
                    .at_least(1)
                    .allow_trailing(),
            )
            .then_ignore(just(Token::RParen))
            .map(SmallVec::from_vec)
    }
}

/// Helper enum for postfix operations during folding; carries end span.
enum PostfixOp {
    Field(String, Span),
    Index(ExprId, Span),
    Call(SmallVec<[ExprId; 4]>, Span),
}

impl PostfixOp {
    fn end(&self) -> Span {
        match self {
            Self::Field(_, s) | Self::Index(_, s) | Self::Call(_, s) => *s,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use smallvec::smallvec;

    use super::*;

    fn parse_ok(src: &str) -> ParseResult {
        Parser::parse(src).expect("should parse")
    }

    fn parse_expr_ok(src: &str) -> (Ast, ExprId) {
        let result = parse_ok(src);
        // Expression statement wraps the expression
        let stmt_id = result.stmts[0];
        let expr_id = result
            .ast
            .get_stmt(stmt_id)
            .and_then(|s| match s {
                Stmt::Expr(id) => Some(*id),
                _ => None,
            })
            .expect("expected expression statement");
        (result.ast, expr_id)
    }

    #[test]
    fn parse_integer() {
        let (ast, id) = parse_expr_ok("42");
        assert_eq!(ast.get_expr(id), Some(&Expr::Literal(Literal::Int(42))));
    }

    #[test]
    fn parse_float() {
        let (ast, id) = parse_expr_ok("3.14");
        assert_eq!(
            ast.get_expr(id),
            Some(&Expr::Literal(Literal::Float(3.14)))
        );
    }

    #[test]
    fn parse_string() {
        let (ast, id) = parse_expr_ok("\"hello\"");
        assert_eq!(
            ast.get_expr(id),
            Some(&Expr::Literal(Literal::String("hello".into())))
        );
    }

    #[test]
    fn parse_bool() {
        let (ast, id) = parse_expr_ok("TRUE");
        assert_eq!(ast.get_expr(id), Some(&Expr::Literal(Literal::Bool(true))));

        let (ast, id) = parse_expr_ok("FALSE");
        assert_eq!(
            ast.get_expr(id),
            Some(&Expr::Literal(Literal::Bool(false)))
        );
    }

    #[test]
    fn parse_var() {
        let (ast, id) = parse_expr_ok("foo");
        assert_eq!(ast.get_expr(id), Some(&Expr::Var("foo".into())));
    }

    #[test]
    fn parse_global() {
        let (ast, id) = parse_expr_ok("^PATIENT");
        assert_eq!(
            ast.get_expr(id),
            Some(&Expr::Global("PATIENT".into(), smallvec![]))
        );
    }

    #[test]
    fn parse_global_with_subscripts() {
        let (ast, id) = parse_expr_ok("^PATIENT(123, \"NAME\")");
        match ast.get_expr(id) {
            Some(Expr::Global(name, subs)) => {
                assert_eq!(name, "PATIENT");
                assert_eq!(subs.len(), 2);
            }
            _ => panic!("expected Global"),
        }
    }

    #[test]
    fn parse_get_global() {
        let (ast, id) = parse_expr_ok("GET ^PATIENT(123)");
        match ast.get_expr(id) {
            Some(Expr::Get(inner)) => match ast.get_expr(*inner) {
                Some(Expr::Global(name, subs)) => {
                    assert_eq!(name, "PATIENT");
                    assert_eq!(subs.len(), 1);
                }
                _ => panic!("expected Global inside Get"),
            },
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn parse_get_local() {
        let (ast, id) = parse_expr_ok("GET cache(\"key\")");
        match ast.get_expr(id) {
            Some(Expr::Get(inner)) => match ast.get_expr(*inner) {
                Some(Expr::Local(name, subs)) => {
                    assert_eq!(name, "cache");
                    assert_eq!(subs.len(), 1);
                }
                _ => panic!("expected Local inside Get"),
            },
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn parse_get_local_no_subscripts() {
        let (ast, id) = parse_expr_ok("GET myvar");
        match ast.get_expr(id) {
            Some(Expr::Get(inner)) => match ast.get_expr(*inner) {
                Some(Expr::Local(name, subs)) => {
                    assert_eq!(name, "myvar");
                    assert!(subs.is_empty());
                }
                _ => panic!("expected Local inside Get"),
            },
            _ => panic!("expected Get"),
        }
    }

    #[test]
    fn parse_binary_add() {
        let (ast, id) = parse_expr_ok("1 + 2");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Add, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Literal(Literal::Int(1)))
                );
                assert_eq!(
                    ast.get_expr(*rhs),
                    Some(&Expr::Literal(Literal::Int(2)))
                );
            }
            _ => panic!("expected Binary Add"),
        }
    }

    #[test]
    fn parse_precedence() {
        // 1 + 2 * 3 should be 1 + (2 * 3)
        let (ast, id) = parse_expr_ok("1 + 2 * 3");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Add, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Literal(Literal::Int(1)))
                );
                match ast.get_expr(*rhs) {
                    Some(Expr::Binary(_, BinOp::Mul, _)) => {}
                    _ => panic!("expected Mul on rhs"),
                }
            }
            _ => panic!("expected Binary Add"),
        }
    }

    #[test]
    fn parse_comparison() {
        let (ast, id) = parse_expr_ok("x > 10");
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Gt, _)) => {}
            _ => panic!("expected Binary Gt"),
        }
    }

    #[test]
    fn parse_logical() {
        let (ast, id) = parse_expr_ok("a AND b OR c");
        // Should be (a AND b) OR c
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Or, _)) => {}
            _ => panic!("expected Binary Or at top"),
        }
    }

    #[test]
    fn parse_unary_neg() {
        let (ast, id) = parse_expr_ok("-5");
        match ast.get_expr(id) {
            Some(Expr::Unary(UnOp::Neg, inner)) => {
                assert_eq!(
                    ast.get_expr(*inner),
                    Some(&Expr::Literal(Literal::Int(5)))
                );
            }
            _ => panic!("expected Unary Neg"),
        }
    }

    #[test]
    fn parse_unary_not() {
        let (ast, id) = parse_expr_ok("NOT x");
        match ast.get_expr(id) {
            Some(Expr::Unary(UnOp::Not, _)) => {}
            _ => panic!("expected Unary Not"),
        }

        let (ast, id) = parse_expr_ok("!x");
        match ast.get_expr(id) {
            Some(Expr::Unary(UnOp::Not, _)) => {}
            _ => panic!("expected Unary Not"),
        }
    }

    #[test]
    fn parse_parenthesized() {
        let (ast, id) = parse_expr_ok("(1 + 2) * 3");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Mul, _)) => {
                match ast.get_expr(*lhs) {
                    Some(Expr::Binary(_, BinOp::Add, _)) => {}
                    _ => panic!("expected Add on lhs"),
                }
            }
            _ => panic!("expected Binary Mul"),
        }
    }

    #[test]
    fn parse_array() {
        let (ast, id) = parse_expr_ok("[1, 2, 3]");
        match ast.get_expr(id) {
            Some(Expr::Array(elems)) => {
                assert_eq!(elems.len(), 3);
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn parse_object() {
        let (ast, id) = parse_expr_ok("{ x: 1, y: 2 }");
        match ast.get_expr(id) {
            Some(Expr::Object(fields)) => {
                assert_eq!(fields.len(), 2);
                assert_eq!(fields[0].0, "x");
                assert_eq!(fields[1].0, "y");
            }
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn parse_field_access() {
        let (ast, id) = parse_expr_ok("obj.field");
        match ast.get_expr(id) {
            Some(Expr::Field(_, name)) => {
                assert_eq!(name, "field");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn parse_index_access() {
        let (ast, id) = parse_expr_ok("arr[0]");
        match ast.get_expr(id) {
            Some(Expr::Index(_, _)) => {}
            _ => panic!("expected Index"),
        }
    }

    #[test]
    fn parse_function_call() {
        let (ast, id) = parse_expr_ok("foo(1, 2)");
        match ast.get_expr(id) {
            Some(Expr::Call(name, args)) => {
                assert_eq!(name, "foo");
                assert_eq!(args.len(), 2);
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn parse_let_stmt() {
        let result = parse_ok("LET x = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Let(name, _) => assert_eq!(name, "x"),
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn parse_set_stmt() {
        let result = parse_ok("SET x = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Set(name, subs, _) => {
                assert_eq!(name, "x");
                assert!(subs.is_empty());
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_set_with_subscripts() {
        let result = parse_ok("SET x(1, 2) = 30");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Set(name, subs, _) => {
                assert_eq!(name, "x");
                assert_eq!(subs.len(), 2);
            }
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_set_global() {
        let result = parse_ok("SET ^PATIENT(123) = \"Bob\"");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::SetGlobal(name, subs, _) => {
                assert_eq!(name, "PATIENT");
                assert_eq!(subs.len(), 1);
            }
            _ => panic!("expected SetGlobal"),
        }
    }

    #[test]
    fn parse_kill_stmt() {
        let result = parse_ok("KILL x");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Kill(name, subs) => {
                assert_eq!(name, "x");
                assert!(subs.is_empty());
            }
            _ => panic!("expected Kill"),
        }
    }

    #[test]
    fn parse_kill_global() {
        let result = parse_ok("KILL ^DATA(123)");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::KillGlobal(name, subs) => {
                assert_eq!(name, "DATA");
                assert_eq!(subs.len(), 1);
            }
            _ => panic!("expected KillGlobal"),
        }
    }

    #[test]
    fn parse_output_stmt() {
        let result = parse_ok("OUTPUT 42");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Output(_) => {}
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn parse_if_stmt() {
        let result = parse_ok("IF x > 0 { OUTPUT x }");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Expr(expr_id) = stmt else {
            panic!("expected Stmt::Expr");
        };
        let expr = result.ast.get_expr(*expr_id).unwrap();
        match expr {
            Expr::If(_, then_blk, else_blk) => {
                // then_blk is a block expression
                let Expr::Block(stmts, _) =
                    result.ast.get_expr(*then_blk).unwrap()
                else {
                    panic!("expected Block");
                };
                assert_eq!(stmts.len(), 1);
                assert!(else_blk.is_none());
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn parse_if_else_stmt() {
        let result =
            parse_ok("IF x > 0 { OUTPUT \"pos\" } ELSE { OUTPUT \"neg\" }");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Expr(expr_id) = stmt else {
            panic!("expected Stmt::Expr");
        };
        let expr = result.ast.get_expr(*expr_id).unwrap();
        match expr {
            Expr::If(_, then_blk, else_blk) => {
                let Expr::Block(then_stmts, _) =
                    result.ast.get_expr(*then_blk).unwrap()
                else {
                    panic!("expected Block");
                };
                assert_eq!(then_stmts.len(), 1);
                let else_id = else_blk.expect("expected else branch");
                let Expr::Block(else_stmts, _) =
                    result.ast.get_expr(else_id).unwrap()
                else {
                    panic!("expected Block");
                };
                assert_eq!(else_stmts.len(), 1);
            }
            _ => panic!("expected If"),
        }
    }

    #[test]
    fn parse_multiple_stmts() {
        let result = parse_ok("LET x = 10\nLET y = 20\nOUTPUT x + y");
        assert_eq!(result.stmts.len(), 3);
    }

    #[test]
    fn parse_complex_example() {
        let src = r#"
LET x = 10
LET y = 20
LET sum = x + y
OUTPUT sum

SET z = 100
SET z(1, "ABC") = 30

IF sum > 25 {
  OUTPUT "Large sum"
} ELSE {
  OUTPUT "Small sum"
}
"#;
        let result = parse_ok(src);
        assert!(result.stmts.len() >= 7);
    }
}
