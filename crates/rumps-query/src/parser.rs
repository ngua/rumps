//! Parser for the RUMPS query language.
//!
//! Transforms a token stream into an AST using chumsky. Uses `RefCell` to build
//! the arena-allocated AST during parsing.
//!
//! # Whitespace Handling
//!
//! The lexer emits `Newline`, `Indent`, and `Dedent` tokens to track source
//! structure. Rather than filtering these in the lexer, the parser handles them
//! explicitly via `opt_newlines()`. This preserves all tokens in the stream for:
//!
//! - **Formatters**: A future formatter needs the original whitespace structure
//! - **Source maps**: Accurate span information for error messages
//! - **Round-tripping**: Parse then re-emit without losing formatting
//!
//! The parser allows optional newlines (and indent/dedent) in these contexts:
//!
//! - **Binary operators**: Before and after operators for expression continuation
//!   (`1\n    + 2` parses as `1 + 2`)
//! - **Delimited constructs**: Inside `[]`, `()`, and `{}` for multi-line arrays,
//!   function calls, objects, and blocks
//!
//! Newlines remain significant as statement separators at the top level and
//! inside block expressions.

#![allow(dead_code)]
// NOTE: This is because `ParseErr = Simple<Token, Span>`, which can be quite
// large. Boxing it would infect the entire parser. This is only for errors,
// which are not the happy path, so I'm not too concerned about size here.
// It's also a warning for 136 bytes, which is not _that_ large and anyway
// `Box`ing would add allocation overhead
#![allow(clippy::result_large_err)]

use std::cell::RefCell;
use std::rc::Rc;

use chumsky::prelude::{choice, end, just, recursive, select, Simple};
use chumsky::Parser as _;
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::{
    Ast, BinOp, Error, Expr, ExprId, Lexer, Literal, Result, Span, Spanned,
    Stmt, StmtId, Token, TypePattern, UnOp,
};

/// Shared mutable AST arena for use during parsing.
type AstCell = Rc<RefCell<Ast>>;

/// Parser error type for token-based parsing.
type ParseErr = Simple<Token, Span>;

/// Parser output paired with its span.
type SpannedExpr = (ExprId, Span);
type SpannedStmt = (StmtId, Span);
type SpannedTypeExpr = (AstTypeExprId, Span);

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
            let let_stmt = Self::let_stmt(Rc::clone(&ast), stmt.clone());
            let set_stmt = Self::set_stmt(Rc::clone(&ast), stmt.clone());
            let kill_stmt = Self::kill_stmt(Rc::clone(&ast), stmt.clone());
            let output_stmt = Self::output_stmt(Rc::clone(&ast), stmt.clone());
            let fun_stmt = Self::fun_stmt(Rc::clone(&ast), stmt.clone());
            // IF is now parsed as an expression in primary_expr; expr_stmt handles it
            let expr_stmt = Self::expr_stmt(Rc::clone(&ast), stmt);

            choice((
                let_stmt,
                set_stmt,
                kill_stmt,
                output_stmt,
                fun_stmt,
                expr_stmt,
            ))
        })
    }

    /// `LET name = expr` or `LET name: Type = expr`
    fn let_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        // Optional type annotation: `: Type`
        let type_ann = just(Token::Colon)
            .ignore_then(Self::type_expr(Rc::clone(&ast)))
            .or_not();

        just(Token::Let)
            .ignore_then(Self::ident())
            .then(type_ann)
            .then_ignore(just(Token::Assign))
            .then(Self::expr(Rc::clone(&ast), stmt))
            .map_with_span(move |((name, ty_ann), (val_id, _)), span| {
                let ty_id = ty_ann.map(|(id, _)| id);
                let id = ast
                    .borrow_mut()
                    .add_stmt(Stmt::Let(name, ty_id, val_id), span);
                (id, span)
            })
    }

    /// `SET name = expr` or `SET name(subs...) = expr`
    /// `SET ^global = expr` or `SET ^global(subs...) = expr`
    fn set_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast4 = Rc::clone(&ast);
        let ast5 = Rc::clone(&ast);

        let expr1 = Self::expr(Rc::clone(&ast), stmt.clone());
        let expr2 = Self::expr(Rc::clone(&ast2), stmt.clone());
        let expr3 = Self::expr(Rc::clone(&ast4), stmt.clone());
        let expr4 = Self::expr(Rc::clone(&ast5), stmt);

        let local_set = just(Token::Set)
            .ignore_then(Self::ident())
            .then(Self::subscripts(expr1).or_not())
            .then_ignore(just(Token::Assign))
            .then(expr2)
            .map_with_span(move |((name, subs), (val_id, _)), span| {
                let subs = subs.unwrap_or_default();
                let mut ast_ref = ast3.borrow_mut();
                let target = ast_ref.add_expr(Expr::Local(name, subs), span);
                let id = ast_ref.add_stmt(Stmt::Set(target, val_id), span);
                (id, span)
            });

        let global_set = just(Token::Set)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(expr3).or_not())
            .then_ignore(just(Token::Assign))
            .then(expr4)
            .map_with_span(move |((name, subs), (val_id, _)), span| {
                let subs = subs.unwrap_or_default();
                let mut ast_ref = ast5.borrow_mut();
                let target = ast_ref.add_expr(Expr::Global(name, subs), span);
                let id = ast_ref.add_stmt(Stmt::Set(target, val_id), span);
                (id, span)
            });

        global_set.or(local_set)
    }

    /// `KILL name` or `KILL name(subs...)`
    /// `KILL ^global` or `KILL ^global(subs...)`
    fn kill_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast4 = Rc::clone(&ast);

        let expr1 = Self::expr(Rc::clone(&ast), stmt.clone());
        let expr2 = Self::expr(Rc::clone(&ast3), stmt);

        let local_kill = just(Token::Kill)
            .ignore_then(Self::ident())
            .then(Self::subscripts(expr1).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let mut ast_ref = ast2.borrow_mut();
                let target = ast_ref.add_expr(Expr::Local(name, subs), span);
                let id = ast_ref.add_stmt(Stmt::Kill(target), span);
                (id, span)
            });

        let global_kill = just(Token::Kill)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(expr2).or_not())
            .map_with_span(move |(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let mut ast_ref = ast4.borrow_mut();
                let target = ast_ref.add_expr(Expr::Global(name, subs), span);
                let id = ast_ref.add_stmt(Stmt::Kill(target), span);
                (id, span)
            });

        global_kill.or(local_kill)
    }

    /// `OUTPUT expr`
    fn output_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        just(Token::Output)
            .ignore_then(Self::expr(Rc::clone(&ast), stmt))
            .map_with_span(move |(expr_id, _), span| {
                let id = ast.borrow_mut().add_stmt(Stmt::Output(expr_id), span);
                (id, span)
            })
    }

    /// `FUN name (params) { body }` or `FUN name (params) -> Type { body }`
    fn fun_stmt(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        let ast2 = Rc::clone(&ast);

        // Parameter: `name` or `name: Type`
        let param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr(Rc::clone(&ast)))
                    .map(|(id, _)| id)
                    .or_not(),
            )
            .map(|(name, ty)| (name, ty));

        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());

        // Parameter list: `(params...)`
        let params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(param.separated_by(param_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Optional return type: `-> Type`
        let ret_ty = Self::opt_newlines()
            .ignore_then(just(Token::Arrow))
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::type_expr(Rc::clone(&ast2)))
            .map(|(id, _)| id)
            .or_not();

        // Body block
        let body = Self::block(stmt);

        just(Token::Fun)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(params)
            .then(ret_ty)
            .then(body)
            .map_with_span(
                move |(((name, params_vec), ret), (stmts, blk_span)), span| {
                    let params = SmallVec::from_vec(params_vec);
                    let mut ast_ref = ast2.borrow_mut();
                    let body =
                        Self::stmts_to_block(&mut ast_ref, stmts, blk_span);
                    let id = ast_ref.add_stmt(
                        Stmt::Fun {
                            name,
                            params,
                            ret,
                            body,
                        },
                        span,
                    );
                    (id, span)
                },
            )
    }

    /// Convert a list of statements to a block expression.
    ///
    /// If the last statement is `Stmt::Expr(e)`, extracts `e` as the trailing
    /// expression (block's value). Otherwise, the block has no trailing expr.
    fn stmts_to_block(ast: &mut Ast, stmts: Vec<StmtId>, span: Span) -> ExprId {
        // Check if last statement is Stmt::Expr; if so, use it as tail
        let tail = stmts.last().and_then(|&id| {
            ast.get_stmt(id).and_then(|s| match s {
                Stmt::Expr(e) => Some(*e),
                _ => None,
            })
        });

        // If last was Expr, exclude it from statements and use as tail
        if let Some(e) = tail {
            let n = stmts.len().saturating_sub(1);
            let block_stmts = stmts.into_iter().take(n).collect();
            ast.add_expr(Expr::Block(block_stmts, Some(e)), span)
        } else {
            ast.add_expr(Expr::Block(stmts, None), span)
        }
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
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr> {
        Self::expr(Rc::clone(&ast), stmt).map_with_span(
            move |(expr_id, _), span| {
                let id = ast.borrow_mut().add_stmt(Stmt::Expr(expr_id), span);
                (id, span)
            },
        )
    }

    /// Top-level expression parser with full precedence.
    ///
    /// Builds the entire precedence chain inside the `recursive` closure,
    /// so `stmt` is only passed to `primary_expr` where it's actually needed.
    fn expr(
        ast: AstCell,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        recursive(move |expr| {
            // Build precedence chain from highest to lowest
            let primary =
                Self::primary_expr(Rc::clone(&ast), expr.clone(), stmt.clone());
            let postfix =
                Self::postfix_expr(Rc::clone(&ast), expr.clone(), primary);
            let unary = Self::unary_expr(Rc::clone(&ast), expr, postfix);
            let pow = Self::pow_expr(Rc::clone(&ast), unary);
            let mul = Self::mul_expr(Rc::clone(&ast), pow);
            let add = Self::add_expr(Rc::clone(&ast), mul);
            let cmp = Self::cmp_expr(Rc::clone(&ast), add);
            let is = Self::is_expr(Rc::clone(&ast), cmp);
            let as_cast = Self::as_expr(Rc::clone(&ast), is);
            let read = Self::read_expr(Rc::clone(&ast), as_cast);
            let and = Self::and_expr(Rc::clone(&ast), read);
            let or = Self::or_expr(Rc::clone(&ast), and);
            Self::coalesce_expr(Rc::clone(&ast), or)
        })
    }

    /// Coalesce: `expr ?? expr` (lowest precedence)
    fn coalesce_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = just(Token::QuestionQuestion).to(BinOp::Coalesce);
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
    }

    /// Logical OR: `expr || expr` or `expr OR expr`
    fn or_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((just(Token::PipePipe), just(Token::Or))).to(BinOp::Or);
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
    }

    /// Logical AND: `expr && expr` or `expr AND expr`
    fn and_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((just(Token::AmpAmp), just(Token::And))).to(BinOp::And);
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
    }

    /// Comparison: `<`, `>`, `<=`, `>=`, `==`, `!=`
    fn cmp_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
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
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
    }

    /// Type check: `expr is Pattern`
    ///
    /// Pattern can be:
    /// - Simple type: `is Int`, `is String`
    /// - Variant (zero-arity): `is Option.None`
    /// - Variant with wildcard: `is Option.Some(_)`
    /// - Variant with binding: `is Option.Some(val)`
    fn is_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        // Parse a type pattern after `is`
        let type_pattern = Self::type_pattern();

        let is_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Is))
            .then_ignore(Self::opt_newlines())
            .ignore_then(type_pattern);

        operand.clone().then(is_rhs.or_not()).map_with_span(
            move |(expr, pattern), span| match pattern {
                Some(pat) => {
                    let id =
                        ast.borrow_mut().add_expr(Expr::Is(expr.0, pat), span);
                    (id, span)
                }
                None => expr,
            },
        )
    }

    /// Parse a type pattern for the `is` operator.
    fn type_pattern(
    ) -> impl chumsky::Parser<Token, TypePattern, Error = ParseErr> + Clone
    {
        // Wildcard: `_` (underscore is parsed as an identifier)
        let wildcard = select! { Token::Ident(s) if s == "_" => () };

        // Binding name (any identifier except `_`)
        let binding = select! { Token::Ident(s) if s != "_" => s };

        // Pattern arguments: `(name)`, `(name1, name2)`, or `(_)`
        let pattern_args = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(choice((
                // Wildcard: `(_)`
                wildcard.to(PatternArgs::Wildcard),
                // Bindings: `(name)` or `(name1, name2, ...)`
                binding
                    .separated_by(
                        just(Token::Comma).then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1)
                    .map(|names| {
                        PatternArgs::Bindings(SmallVec::from_vec(names))
                    }),
            )))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Type.Variant pattern (with optional args)
        let variant_pattern = Self::ident()
            .then_ignore(just(Token::Dot))
            .then(Self::ident())
            .then(pattern_args.or_not())
            .map(|((ty, var), args)| match args {
                None => TypePattern::Variant(ty, var),
                Some(PatternArgs::Wildcard) => {
                    TypePattern::VariantWildcard(ty, var)
                }
                Some(PatternArgs::Bindings(names)) => {
                    TypePattern::VariantBind(ty, var, names)
                }
            });

        // Simple type pattern: `Int`, `String`, etc.
        let simple_type = Self::ident().map(TypePattern::Type);

        // Try variant first, then simple type
        variant_pattern.or(simple_type)
    }

    /// Type cast: `expr as Type`
    fn as_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let as_rhs = Self::opt_newlines()
            .ignore_then(just(Token::As))
            .then_ignore(Self::opt_newlines())
            .ignore_then(Self::type_expr(Rc::clone(&ast)));

        operand.clone().then(as_rhs.or_not()).map_with_span(
            move |(expr, ty), span| match ty {
                Some((ty_id, _)) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::As(expr.0, ty_id), span);
                    (id, span)
                }
                None => expr,
            },
        )
    }

    /// Fallible conversion: `expr read Type`
    fn read_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let read_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Read))
            .then_ignore(Self::opt_newlines())
            .ignore_then(Self::type_expr(Rc::clone(&ast)));

        operand.clone().then(read_rhs.or_not()).map_with_span(
            move |(expr, ty), span| match ty {
                Some((ty_id, _)) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::Read(expr.0, ty_id), span);
                    (id, span)
                }
                None => expr,
            },
        )
    }

    /// Additive: `+`, `-`, `++`
    fn add_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op = choice((
            just(Token::Plus).to(BinOp::Add),
            just(Token::Minus).to(BinOp::Sub),
            just(Token::Concat).to(BinOp::Concat),
        ));
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
    }

    /// Power: `**` (right-associative)
    fn pow_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let op_rhs = Self::opt_newlines()
            .ignore_then(just(Token::StarStar))
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary_right(&ast, first, rest, span)
            },
        )
    }

    /// Folds a sequence of power operations right-to-left.
    fn fold_binary_right(
        ast: &AstCell,
        first: SpannedExpr,
        rest: Vec<(Token, SpannedExpr)>,
        _outer_span: Span,
    ) -> SpannedExpr {
        rest.into_iter()
            .rfold(None, |acc: Option<SpannedExpr>, (_tok, expr)| match acc {
                None => Some(expr),
                Some(rhs) => {
                    let span = expr.1.merge(rhs.1);
                    let id = ast.borrow_mut().add_expr(
                        Expr::Binary(expr.0, BinOp::Pow, rhs.0),
                        span,
                    );
                    Some((id, span))
                }
            })
            .map_or(first, |rhs| {
                let span = first.1.merge(rhs.1);
                let id = ast
                    .borrow_mut()
                    .add_expr(Expr::Binary(first.0, BinOp::Pow, rhs.0), span);
                (id, span)
            })
    }

    /// Multiplicative: `*`, `/`, `//`, `%`
    fn mul_expr(
        ast: AstCell,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
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
        // Allow newlines before/after operator for continuation
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            move |(first, rest), span| {
                Self::fold_binary(&ast, first, rest, span)
            },
        )
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

    /// Unary: `NOT`, `!`, `-`, `GET`
    fn unary_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
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

        // Unary is right-associative, so we use recursion
        recursive(move |unary| {
            let ast_inner = Rc::clone(&ast);
            let ast_get = Rc::clone(&ast2);
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

            choice((with_op, get_expr)).or(operand.clone())
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
        operand: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let ast2 = Rc::clone(&ast);

        // Field access: `.field`
        let field = just(Token::Dot)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::Field);

        // Optional field access: `?.field`
        let opt_field = just(Token::QuestionDot)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::OptionalField);

        // Index: `[expr]` - allow newlines inside
        let index = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|(idx, _), span| PostfixOp::Index(idx, span));

        // Call: `(args...)` - allow newlines around arguments
        let call_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let call = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                expr.map(|(id, _)| id)
                    .separated_by(call_sep)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(|args, span| {
                PostfixOp::Call(SmallVec::from_vec(args), span)
            });

        let postfix_op = choice((field, opt_field, index, call));

        operand
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
                PostfixOp::OptionalField(name, _) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::OptionalField(acc.0, name), span);
                    Some((id, span))
                }
                PostfixOp::Index(idx, _) => {
                    let id = ast
                        .borrow_mut()
                        .add_expr(Expr::Index(acc.0, idx), span);
                    Some((id, span))
                }
                PostfixOp::Call(args, _) => {
                    // Convert base to function call or variant constructor
                    let base_expr = ast.borrow().get_expr(acc.0).cloned();
                    base_expr.and_then(|e| match e {
                        // Simple call: `func(args)`
                        Expr::Var(name) => {
                            let id = ast
                                .borrow_mut()
                                .add_expr(Expr::Call(name, args), span);
                            Some((id, span))
                        }
                        // Variant constructor: `Type.Variant(args)`
                        Expr::Field(inner, var_name) => {
                            let inner_expr =
                                ast.borrow().get_expr(inner).cloned();
                            inner_expr.and_then(|ie| match ie {
                                Expr::Var(ty_name) => {
                                    let id = ast.borrow_mut().add_expr(
                                        Expr::Variant(ty_name, var_name, args),
                                        span,
                                    );
                                    Some((id, span))
                                }
                                _ => None,
                            })
                        }
                        _ => None,
                    })
                }
            }
        })
    }

    /// Primary: literals, identifiers, globals, parenthesized, arrays, objects, if.
    fn primary_expr(
        ast: AstCell,
        expr: impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr>
            + Clone
            + 'static,
        stmt: impl chumsky::Parser<Token, SpannedStmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, SpannedExpr, Error = ParseErr> + Clone
    {
        let ast2 = Rc::clone(&ast);
        let ast3 = Rc::clone(&ast);
        let ast5 = Rc::clone(&ast);
        let ast6 = Rc::clone(&ast);
        let ast7 = Rc::clone(&ast);
        let ast_closure = Rc::clone(&ast);
        let ast_closure2 = Rc::clone(&ast);
        let ast_closure3 = Rc::clone(&ast);

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

        // Parenthesized expression - allow newlines inside
        let paren = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Array literal: `[expr, ...]`
        // Allow newlines around elements for multi-line arrays
        let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let array = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                expr.clone()
                    .map(|(id, _)| id)
                    .separated_by(arr_sep)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(move |elems, span| {
                let id = ast5.borrow_mut().add_expr(Expr::Array(elems), span);
                (id, span)
            });

        // Object literal: `{ key: value, ... }`
        // Must be tried BEFORE block_expr since both start with `{`
        let obj_field = Self::ident()
            .then_ignore(just(Token::Colon))
            .then(expr.clone().map(|(id, _)| id));

        // Allow newlines around fields for multi-line objects
        let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let object = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(obj_field.separated_by(obj_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(move |fields, span| {
                let id = ast6.borrow_mut().add_expr(Expr::Object(fields), span);
                (id, span)
            });

        // Block expression: `{ stmts... [trailing_expr] }`
        // Used standalone and by IF, TRANSACTION, etc.
        let ast8 = Rc::clone(&ast7);
        let block_parser = Self::block(stmt);
        let block_expr = block_parser.clone().map_with_span(
            move |(stmts, blk_span), span| {
                let id = Self::stmts_to_block(
                    &mut ast7.borrow_mut(),
                    stmts,
                    blk_span,
                );
                (id, span)
            },
        );

        // IF expression: `IF cond block [ELSE block]`
        let if_expr = just(Token::If)
            .ignore_then(expr.clone().map(|(id, _)| id))
            .then(block_parser.clone())
            .then(
                just(Token::Else)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(block_parser.clone())
                    .or_not(),
            )
            .map_with_span(
                move |((cond, (then_stmts, then_span)), else_block), span| {
                    let mut ast_mut = ast8.borrow_mut();

                    let then_expr = Self::stmts_to_block(
                        &mut ast_mut,
                        then_stmts,
                        then_span,
                    );

                    let else_expr = else_block.map(|(stmts, blk_span)| {
                        Self::stmts_to_block(&mut ast_mut, stmts, blk_span)
                    });

                    let id = ast_mut
                        .add_expr(Expr::If(cond, then_expr, else_expr), span);
                    (id, span)
                },
            );

        // Closure expressions
        // Single untyped param: `x => expr`
        let closure_single = Self::ident()
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr.clone())
            .map_with_span(move |(name, (body, _)), span| {
                let params = smallvec::smallvec![(name, None)];
                let id = ast_closure.borrow_mut().add_expr(
                    Expr::Closure {
                        params,
                        ret: None,
                        body,
                    },
                    span,
                );
                (id, span)
            });

        // Param: `name` or `name: Type`
        let closure_param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr(Rc::clone(&ast_closure2)))
                    .map(|(id, _)| id)
                    .or_not(),
            )
            .map(|(name, ty)| (name, ty));

        // Multi-param closure: `(params) => expr` or `(params) -> Type => expr`
        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let closure_multi = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(closure_param.separated_by(param_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .then_ignore(Self::opt_newlines())
            .then(
                just(Token::Arrow)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr(Rc::clone(&ast_closure3)))
                    .map(|(id, _)| id)
                    .or_not(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(move |((params_vec, ret), (body, _)), span| {
                let params = SmallVec::from_vec(params_vec);
                let id = ast_closure2
                    .borrow_mut()
                    .add_expr(Expr::Closure { params, ret, body }, span);
                (id, span)
            });

        // Order matters:
        // - closure_single before var (both start with ident, but closure has `=>`)
        // - closure_multi before paren (both start with `(`, but closure has `=>`)
        // - object before block_expr (both start with `{`, object requires `ident:`)
        // - global before var (both can start with ident pattern)
        choice((
            literal,
            closure_single,
            closure_multi,
            global,
            var,
            paren,
            array,
            object,
            block_expr,
            if_expr,
        ))
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

    /// Parse a type expression: `Int`, `Option[Int]`, `(Int, Int) -> Int`, etc.
    ///
    /// Grammar:
    /// - `type_expr := fn_type`
    /// - `fn_type := atom_or_params '->' fn_type | atom_type`
    /// - `atom_or_params := '(' type_list ')' | '(' ')' | atom_type`
    /// - `atom_type := ident ('[' type_list ']')?`
    /// - `type_list := type_expr (',' type_expr)*`
    ///
    /// `->` is right-associative: `Int -> Int -> Int` = `Int -> (Int -> Int)`
    fn type_expr(
        ast: AstCell,
    ) -> impl chumsky::Parser<Token, SpannedTypeExpr, Error = ParseErr> + Clone
    {
        recursive(|ty| {
            // Type parameters for generic types: `[T]` or `[T, E]`
            let type_params = ty
                .clone()
                .separated_by(just(Token::Comma))
                .at_least(1)
                .delimited_by(just(Token::LBracket), just(Token::RBracket))
                .map(SmallVec::<[SpannedTypeExpr; 2]>::from_vec);

            // Atom: named type optionally with type params
            let ast_atom = Rc::clone(&ast);
            let atom = Self::ident().then(type_params.or_not()).map_with_span(
                move |(name, params), span| {
                    let te = match params {
                        None => AstTypeExpr::Named(name),
                        Some(ps) => AstTypeExpr::App(
                            name,
                            ps.into_iter().map(|(id, _)| id).collect(),
                        ),
                    };
                    let id = ast_atom.borrow_mut().add_type_expr(te, span);
                    TypeAtomOrParams::Single(id, span)
                },
            );

            // Parenthesized: `()`, `(T)`, or `(T, U, ...)`
            let sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let paren = just(Token::LParen)
                .ignore_then(Self::opt_newlines())
                .ignore_then(ty.clone().separated_by(sep).allow_trailing())
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen))
                .map_with_span(|types, span| {
                    let ids: SmallVec<[AstTypeExprId; 4]> =
                        types.into_iter().map(|(id, _)| id).collect();
                    TypeAtomOrParams::Params(ids, span)
                });

            // atom_or_params: either an atom or parenthesized params
            let atom_or_params = paren.or(atom);

            // Function type with `->` (right-associative via recursion)
            let ast_fn = Rc::clone(&ast);
            atom_or_params
                .then(
                    Self::opt_newlines()
                        .ignore_then(just(Token::Arrow))
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(ty)
                        .or_not(),
                )
                .try_map(move |(left, arrow_ret), span| {
                    Self::build_fn_type(&ast_fn, left, arrow_ret, span)
                })
        })
    }

    /// Build a function type or standalone type from parsed components.
    fn build_fn_type(
        ast: &AstCell,
        left: TypeAtomOrParams,
        arrow_ret: Option<SpannedTypeExpr>,
        span: Span,
    ) -> std::result::Result<SpannedTypeExpr, ParseErr> {
        match (left, arrow_ret) {
            // `T -> R` or `(T) -> R`: single param function
            (TypeAtomOrParams::Single(param, _), Some((ret, _))) => {
                let te = AstTypeExpr::Fn(smallvec::smallvec![param], ret);
                let id = ast.borrow_mut().add_type_expr(te, span);
                Ok((id, span))
            }
            // `(T, U, ...) -> R` or `() -> R`: multi/nullary param function
            (TypeAtomOrParams::Params(params, _), Some((ret, _))) => {
                let te = AstTypeExpr::Fn(params, ret);
                let id = ast.borrow_mut().add_type_expr(te, span);
                Ok((id, span))
            }
            // `T`: standalone type (no arrow)
            (TypeAtomOrParams::Single(ty, ty_span), None) => Ok((ty, ty_span)),
            // `(T)`: parenthesized single type (no arrow)
            (TypeAtomOrParams::Params(mut params, p_span), None)
                if params.len() == 1 =>
            {
                // Unwrap single-element parens; `(Int)` = `Int`
                params.pop().map_or_else(
                    || {
                        Err(Simple::custom(
                            span,
                            "internal: expected single type",
                        ))
                    },
                    |ty| Ok((ty, p_span)),
                )
            }
            // `()` or `(T, U)` without `->`: error
            (TypeAtomOrParams::Params(params, _), None) => {
                let msg = if params.is_empty() {
                    "empty parentheses require `->` for nullary function type"
                } else {
                    "multiple types in parentheses require `->` for function type"
                };
                Err(Simple::custom(span, msg))
            }
        }
    }
}

/// Helper for parsing function type syntax.
///
/// Distinguishes between a single type atom and parenthesized parameter lists.
#[derive(Clone)]
enum TypeAtomOrParams {
    /// A single named or parameterized type: `Int`, `Option[T]`
    Single(AstTypeExprId, Span),
    /// Parenthesized list of types: `()`, `(T)`, `(T, U)`
    Params(SmallVec<[AstTypeExprId; 4]>, Span),
}

/// Helper enum for pattern arguments in `is` patterns.
#[derive(Clone)]
enum PatternArgs {
    Wildcard,
    Bindings(SmallVec<[String; 2]>),
}

/// Helper enum for postfix operations during folding; carries end span.
enum PostfixOp {
    Field(String, Span),
    OptionalField(String, Span),
    Index(ExprId, Span),
    Call(SmallVec<[ExprId; 4]>, Span),
}

impl PostfixOp {
    fn end(&self) -> Span {
        match self {
            Self::Field(_, s)
            | Self::OptionalField(_, s)
            | Self::Index(_, s)
            | Self::Call(_, s) => *s,
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
    fn parse_coalesce() {
        let (ast, id) = parse_expr_ok("a ?? b");
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Coalesce, _)) => {}
            _ => panic!("expected Binary Coalesce"),
        }
    }

    #[test]
    fn parse_coalesce_chain() {
        // a ?? b ?? c should be (a ?? b) ?? c (left-associative)
        let (ast, id) = parse_expr_ok("a ?? b ?? c");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Coalesce, _)) => {
                match ast.get_expr(*lhs) {
                    Some(Expr::Binary(_, BinOp::Coalesce, _)) => {}
                    _ => panic!("expected nested Coalesce on lhs"),
                }
            }
            _ => panic!("expected Binary Coalesce"),
        }
    }

    #[test]
    fn parse_coalesce_precedence() {
        // a + b ?? c should be (a + b) ?? c (?? is lower precedence than +)
        let (ast, id) = parse_expr_ok("a + b ?? c");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Coalesce, _)) => {
                match ast.get_expr(*lhs) {
                    Some(Expr::Binary(_, BinOp::Add, _)) => {}
                    _ => panic!("expected Add on lhs"),
                }
            }
            _ => panic!("expected Binary Coalesce at top"),
        }
    }

    #[test]
    fn parse_let_stmt() {
        let result = parse_ok("LET x = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Let(name, _, _) => assert_eq!(name, "x"),
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn parse_let_with_type_annotation() {
        let result = parse_ok("LET x: Int = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(name, ty_ann, _) = stmt else {
            panic!("expected Let");
        };
        assert_eq!(name, "x");
        assert!(ty_ann.is_some());
        let ty_expr = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        match ty_expr {
            AstTypeExpr::Named(n) => assert_eq!(n, "Int"),
            _ => panic!("expected Named type"),
        }
    }

    #[test]
    fn parse_let_with_parameterized_type() {
        let result = parse_ok("LET x: Option[Int] = Option.None");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(name, ty_ann, _) = stmt else {
            panic!("expected Let");
        };
        assert_eq!(name, "x");
        assert!(ty_ann.is_some());
        let ty_expr = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        match ty_expr {
            AstTypeExpr::App(n, params) => {
                assert_eq!(n, "Option");
                assert_eq!(params.len(), 1);
            }
            _ => panic!("expected App type"),
        }
    }

    #[test]
    fn parse_let_with_nested_type() {
        let result = parse_ok("LET x: Array[Option[Int]] = []");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(_, ty_ann, _) = stmt else {
            panic!("expected Let");
        };
        let ty_expr = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        // Array[Option[Int]]
        let AstTypeExpr::App(outer, outer_params) = ty_expr else {
            panic!("expected App type");
        };
        assert_eq!(outer, "Array");
        assert_eq!(outer_params.len(), 1);
        // Option[Int]
        let inner = result.ast.get_type_expr(outer_params[0]).unwrap();
        let AstTypeExpr::App(mid, mid_params) = inner else {
            panic!("expected nested App type");
        };
        assert_eq!(mid, "Option");
        assert_eq!(mid_params.len(), 1);
        // Int
        let innermost = result.ast.get_type_expr(mid_params[0]).unwrap();
        let AstTypeExpr::Named(name) = innermost else {
            panic!("expected Named type");
        };
        assert_eq!(name, "Int");
    }

    #[test]
    fn parse_function_type_simple() {
        // `Int -> Int`
        let result = parse_ok("LET f: Int -> Int = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, ret) = ty else {
            panic!("expected Fn type, got {:?}", ty);
        };
        assert_eq!(params.len(), 1);
        // Check param is Int
        let AstTypeExpr::Named(p) =
            result.ast.get_type_expr(params[0]).unwrap()
        else {
            panic!("expected Named param");
        };
        assert_eq!(p, "Int");
        // Check return is Int
        let AstTypeExpr::Named(r) = result.ast.get_type_expr(*ret).unwrap()
        else {
            panic!("expected Named return");
        };
        assert_eq!(r, "Int");
    }

    #[test]
    fn parse_function_type_multi_param() {
        // `(Int, Int) -> Int`
        let result = parse_ok("LET f: (Int, Int) -> Int = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, _) = ty else {
            panic!("expected Fn type");
        };
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn parse_function_type_nullary() {
        // `() -> String`
        let result = parse_ok("LET f: () -> String = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, ret) = ty else {
            panic!("expected Fn type");
        };
        assert!(params.is_empty());
        let AstTypeExpr::Named(r) = result.ast.get_type_expr(*ret).unwrap()
        else {
            panic!("expected Named return");
        };
        assert_eq!(r, "String");
    }

    #[test]
    fn parse_function_type_right_assoc() {
        // `Int -> Int -> Int` = `Int -> (Int -> Int)`
        let result = parse_ok("LET f: Int -> Int -> Int = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, ret) = ty else {
            panic!("expected outer Fn type");
        };
        assert_eq!(params.len(), 1);
        // Return type should also be Fn
        let AstTypeExpr::Fn(inner_params, _) =
            result.ast.get_type_expr(*ret).unwrap()
        else {
            panic!("expected inner Fn type");
        };
        assert_eq!(inner_params.len(), 1);
    }

    #[test]
    fn parse_function_type_higher_order() {
        // `((Int) -> Int, Int) -> Int` - function that takes a function
        let result = parse_ok("LET f: ((Int) -> Int, Int) -> Int = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, _) = ty else {
            panic!("expected Fn type");
        };
        assert_eq!(params.len(), 2);
        // First param should be Fn
        let AstTypeExpr::Fn(inner_params, _) =
            result.ast.get_type_expr(params[0]).unwrap()
        else {
            panic!("expected first param to be Fn type");
        };
        assert_eq!(inner_params.len(), 1);
    }

    #[test]
    fn parse_function_type_returns_function() {
        // `(Int) -> (Int) -> Int` - function returning a function
        let result = parse_ok("LET f: (Int) -> (Int) -> Int = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Fn(params, ret) = ty else {
            panic!("expected outer Fn type");
        };
        assert_eq!(params.len(), 1);
        // Return type should be Fn
        let AstTypeExpr::Fn(inner_params, _) =
            result.ast.get_type_expr(*ret).unwrap()
        else {
            panic!("expected return to be Fn type");
        };
        assert_eq!(inner_params.len(), 1);
    }

    #[test]
    fn parse_parenthesized_single_type() {
        // `(Int)` should be the same as `Int` when not followed by `->`
        let result = parse_ok("LET x: (Int) = 0");
        let Stmt::Let(_, ty_ann, _) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let ty = result.ast.get_type_expr(ty_ann.unwrap()).unwrap();
        let AstTypeExpr::Named(name) = ty else {
            panic!("expected Named type, got {:?}", ty);
        };
        assert_eq!(name, "Int");
    }

    #[test]
    fn parse_set_stmt() {
        let result = parse_ok("SET x = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Set(target, _) => match result.ast.get_expr(*target) {
                Some(Expr::Local(name, subs)) => {
                    assert_eq!(name, "x");
                    assert!(subs.is_empty());
                }
                _ => panic!("expected Local"),
            },
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_set_with_subscripts() {
        let result = parse_ok("SET x(1, 2) = 30");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Set(target, _) => match result.ast.get_expr(*target) {
                Some(Expr::Local(name, subs)) => {
                    assert_eq!(name, "x");
                    assert_eq!(subs.len(), 2);
                }
                _ => panic!("expected Local"),
            },
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_set_global() {
        let result = parse_ok("SET ^PATIENT(123) = \"Bob\"");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Set(target, _) => match result.ast.get_expr(*target) {
                Some(Expr::Global(name, subs)) => {
                    assert_eq!(name, "PATIENT");
                    assert_eq!(subs.len(), 1);
                }
                _ => panic!("expected Global"),
            },
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_kill_stmt() {
        let result = parse_ok("KILL x");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Kill(target) => match result.ast.get_expr(*target) {
                Some(Expr::Local(name, subs)) => {
                    assert_eq!(name, "x");
                    assert!(subs.is_empty());
                }
                _ => panic!("expected Local"),
            },
            _ => panic!("expected Kill"),
        }
    }

    #[test]
    fn parse_kill_global() {
        let result = parse_ok("KILL ^DATA(123)");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Kill(target) => match result.ast.get_expr(*target) {
                Some(Expr::Global(name, subs)) => {
                    assert_eq!(name, "DATA");
                    assert_eq!(subs.len(), 1);
                }
                _ => panic!("expected Global"),
            },
            _ => panic!("expected Kill"),
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

    #[test]
    fn parse_if_expr_in_let() {
        // IF expression used in LET binding (the original issue)
        let result = parse_ok("LET x = IF FALSE { 42 }");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(name, _, val_id) = stmt else {
            panic!("expected Let");
        };
        assert_eq!(name, "x");
        let Expr::If(_, then_blk, else_blk) =
            result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected If expression as value");
        };
        // then block should contain `42`
        let Expr::Block(_, Some(tail)) =
            result.ast.get_expr(*then_blk).unwrap()
        else {
            panic!("expected Block with tail");
        };
        assert_eq!(
            result.ast.get_expr(*tail),
            Some(&Expr::Literal(Literal::Int(42)))
        );
        assert!(else_blk.is_none());
    }

    #[test]
    fn parse_if_else_expr_in_let() {
        let result = parse_ok("LET x = IF TRUE { 1 } ELSE { 2 }");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(name, _, val_id) = stmt else {
            panic!("expected Let");
        };
        assert_eq!(name, "x");
        let Expr::If(_, _, else_blk) = result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected If expression as value");
        };
        assert!(else_blk.is_some());
    }

    #[test]
    fn parse_block_expr_standalone() {
        // Standalone block expression
        let (ast, id) = parse_expr_ok("{ 1 + 2 }");
        let Expr::Block(stmts, tail) = ast.get_expr(id).unwrap() else {
            panic!("expected Block");
        };
        assert!(stmts.is_empty());
        assert!(tail.is_some());
    }

    #[test]
    fn parse_block_expr_in_let() {
        let result = parse_ok("LET x = { LET y = 1\ny }");
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        let Stmt::Let(name, _, val_id) = stmt else {
            panic!("expected Let");
        };
        assert_eq!(name, "x");
        let Expr::Block(stmts, tail) = result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected Block");
        };
        assert_eq!(stmts.len(), 1); // LET y = 1
        assert!(tail.is_some()); // y
    }

    #[test]
    fn parse_indented_continuation() {
        // 1 + 2 across two lines
        let result = parse_ok("LET x = 1\n    + 2\nOUTPUT x");
        assert_eq!(result.stmts.len(), 2); // LET and OUTPUT

        // Verify the LET contains a binary Add
        let Stmt::Let(_, _, val_id) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let Expr::Binary(_, BinOp::Add, _) =
            result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected Binary Add");
        };
    }

    #[test]
    fn parse_multi_line_continuation() {
        let result = parse_ok("LET x = 1\n    + 2\n    + 3\nOUTPUT x");
        assert_eq!(result.stmts.len(), 2);

        // Should be ((1 + 2) + 3)
        let Stmt::Let(_, _, val_id) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        let Expr::Binary(lhs, BinOp::Add, _) =
            result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected outer Add");
        };
        let Expr::Binary(_, BinOp::Add, _) = result.ast.get_expr(*lhs).unwrap()
        else {
            panic!("expected inner Add");
        };
    }

    #[test]
    fn parse_continuation_with_precedence() {
        // 1 + 2 * 3 should be 1 + (2 * 3)
        let result = parse_ok("LET x = 1\n    + 2\n    * 3");
        let Stmt::Let(_, _, val_id) =
            result.ast.get_stmt(result.stmts[0]).unwrap()
        else {
            panic!("expected Let");
        };
        // Top should be Add
        let Expr::Binary(_, BinOp::Add, rhs) =
            result.ast.get_expr(*val_id).unwrap()
        else {
            panic!("expected Add at top");
        };
        // RHS should be Mul
        let Expr::Binary(_, BinOp::Mul, _) = result.ast.get_expr(*rhs).unwrap()
        else {
            panic!("expected Mul on rhs");
        };
    }

    #[test]
    fn parse_variant_with_args() {
        let (ast, id) = parse_expr_ok("Option.Some(42)");
        match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, args)) => {
                assert_eq!(ty, "Option");
                assert_eq!(var, "Some");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Variant"),
        }
    }

    #[test]
    fn parse_variant_result_ok() {
        let (ast, id) = parse_expr_ok("Result.Ok(1)");
        match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, args)) => {
                assert_eq!(ty, "Result");
                assert_eq!(var, "Ok");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Variant"),
        }
    }

    #[test]
    fn parse_variant_result_err() {
        let (ast, id) = parse_expr_ok("Result.Err(\"oops\")");
        match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, args)) => {
                assert_eq!(ty, "Result");
                assert_eq!(var, "Err");
                assert_eq!(args.len(), 1);
            }
            _ => panic!("expected Variant"),
        }
    }

    #[test]
    fn parse_variant_zero_arity_as_field() {
        // Zero-arity variants parse as Field; interpreter handles them
        let (ast, id) = parse_expr_ok("Option.None");
        match ast.get_expr(id) {
            Some(Expr::Field(_, field)) => {
                assert_eq!(field, "None");
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn parse_variant_nested() {
        // Option.Some(Result.Ok(1))
        let (ast, id) = parse_expr_ok("Option.Some(Result.Ok(1))");
        match ast.get_expr(id) {
            Some(Expr::Variant(ty, var, args)) => {
                assert_eq!(ty, "Option");
                assert_eq!(var, "Some");
                assert_eq!(args.len(), 1);
                // Inner should also be Variant
                match ast.get_expr(args[0]) {
                    Some(Expr::Variant(ty2, var2, _)) => {
                        assert_eq!(ty2, "Result");
                        assert_eq!(var2, "Ok");
                    }
                    _ => panic!("expected inner Variant"),
                }
            }
            _ => panic!("expected outer Variant"),
        }
    }
}

#[cfg(test)]
mod array_parse_debug {
    use super::*;

    #[test]
    fn parse_test23_array() {
        // Exact content from test 23
        let src = r#"LET matrix3d = [
  [[1, 2], [3, 4]],
  [[5, 6], [7, 8]]
]"#;
        let result = Parser::parse(src);
        assert!(result.is_ok(), "Should parse: {:?}", result.err());
    }

    #[test]
    fn parse_simple_array_newlines() {
        let src = r#"LET arr = [
    1,
    2
]"#;
        let result = Parser::parse(src);
        assert!(result.is_ok(), "Should parse: {:?}", result.err());
    }
}

#[cfg(test)]
mod optional_chaining_tests {
    use super::*;

    fn parse_expr_ok(src: &str) -> (Ast, ExprId) {
        let result = Parser::parse(src).expect("should parse");
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
    fn parse_optional_field_access() {
        let (ast, id) = parse_expr_ok("obj?.field");
        match ast.get_expr(id) {
            Some(Expr::OptionalField(_, name)) => {
                assert_eq!(name, "field");
            }
            _ => panic!("expected OptionalField"),
        }
    }

    #[test]
    fn parse_optional_chaining_chain() {
        // a?.b?.c
        let (ast, id) = parse_expr_ok("a?.b?.c");
        match ast.get_expr(id) {
            Some(Expr::OptionalField(inner, name)) => {
                assert_eq!(name, "c");
                match ast.get_expr(*inner) {
                    Some(Expr::OptionalField(_, name2)) => {
                        assert_eq!(name2, "b");
                    }
                    _ => panic!("expected inner OptionalField"),
                }
            }
            _ => panic!("expected OptionalField"),
        }
    }

    #[test]
    fn parse_optional_then_regular() {
        // a?.b.c
        let (ast, id) = parse_expr_ok("a?.b.c");
        match ast.get_expr(id) {
            Some(Expr::Field(inner, name)) => {
                assert_eq!(name, "c");
                match ast.get_expr(*inner) {
                    Some(Expr::OptionalField(_, name2)) => {
                        assert_eq!(name2, "b");
                    }
                    _ => panic!("expected OptionalField"),
                }
            }
            _ => panic!("expected Field"),
        }
    }

    #[test]
    fn parse_regular_then_optional() {
        // a.b?.c
        let (ast, id) = parse_expr_ok("a.b?.c");
        match ast.get_expr(id) {
            Some(Expr::OptionalField(inner, name)) => {
                assert_eq!(name, "c");
                match ast.get_expr(*inner) {
                    Some(Expr::Field(_, name2)) => {
                        assert_eq!(name2, "b");
                    }
                    _ => panic!("expected Field"),
                }
            }
            _ => panic!("expected OptionalField"),
        }
    }

    #[test]
    fn parse_optional_with_coalesce() {
        // a?.b ?? "default"
        let (ast, id) = parse_expr_ok("a?.b ?? \"default\"");
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Coalesce, _)) => {
                match ast.get_expr(*lhs) {
                    Some(Expr::OptionalField(_, name)) => {
                        assert_eq!(name, "b");
                    }
                    _ => panic!("expected OptionalField on lhs"),
                }
            }
            _ => panic!("expected Binary Coalesce"),
        }
    }

    // ---- Closure tests ----

    #[test]
    fn parse_closure_single_param() {
        // x => x * 2
        let (ast, id) = parse_expr_ok("x => x * 2");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, body }) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].0, "x");
                assert!(params[0].1.is_none());
                assert!(ret.is_none());
                assert!(matches!(ast.get_expr(*body), Some(Expr::Binary(..))));
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_multi_param() {
        // (a, b) => a + b
        let (ast, id) = parse_expr_ok("(a, b) => a + b");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, body }) => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "a");
                assert_eq!(params[1].0, "b");
                assert!(ret.is_none());
                assert!(matches!(ast.get_expr(*body), Some(Expr::Binary(..))));
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_typed_param() {
        // (x: Int) => x * 2
        let (ast, id) = parse_expr_ok("(x: Int) => x * 2");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, .. }) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].0, "x");
                assert!(params[0].1.is_some());
                assert!(ret.is_none());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_with_return_type() {
        // (x: Int) -> Int => x * x
        let (ast, id) = parse_expr_ok("(x: Int) -> Int => x * x");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, .. }) => {
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].0, "x");
                assert!(params[0].1.is_some());
                assert!(ret.is_some());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_nullary() {
        // () => 42
        let (ast, id) = parse_expr_ok("() => 42");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, body }) => {
                assert!(params.is_empty());
                assert!(ret.is_none());
                assert!(matches!(
                    ast.get_expr(*body),
                    Some(Expr::Literal(Literal::Int(42)))
                ));
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_in_let() {
        // LET double = x => x * 2
        let result = Parser::parse("LET double = x => x * 2").unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Let(name, _, val_id) => {
                assert_eq!(name, "double");
                assert!(matches!(
                    result.ast.get_expr(*val_id),
                    Some(Expr::Closure { .. })
                ));
            }
            _ => panic!("expected Let"),
        }
    }

    // ---- FUN statement tests ----

    #[test]
    fn parse_fun_untyped() {
        // FUN add (a, b) { a + b }
        let result = Parser::parse("FUN add (a, b) { a + b }").unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Fun {
                name,
                params,
                ret,
                body,
            } => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "a");
                assert!(params[0].1.is_none());
                assert_eq!(params[1].0, "b");
                assert!(params[1].1.is_none());
                assert!(ret.is_none());
                assert!(matches!(
                    result.ast.get_expr(*body),
                    Some(Expr::Block(_, Some(_)))
                ));
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_fun_typed_params() {
        // FUN add (a: Int, b: Int) { a + b }
        let result =
            Parser::parse("FUN add (a: Int, b: Int) { a + b }").unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Fun {
                name, params, ret, ..
            } => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "a");
                assert!(params[0].1.is_some());
                assert_eq!(params[1].0, "b");
                assert!(params[1].1.is_some());
                assert!(ret.is_none());
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_fun_with_return_type() {
        // FUN square (x: Int) -> Int { x * x }
        let result =
            Parser::parse("FUN square (x: Int) -> Int { x * x }").unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Fun {
                name, params, ret, ..
            } => {
                assert_eq!(name, "square");
                assert_eq!(params.len(), 1);
                assert_eq!(params[0].0, "x");
                assert!(params[0].1.is_some());
                assert!(ret.is_some());
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_fun_nullary() {
        // FUN greet () { "Hello" }
        let result = Parser::parse("FUN greet () { \"Hello\" }").unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Fun {
                name, params, ret, ..
            } => {
                assert_eq!(name, "greet");
                assert!(params.is_empty());
                assert!(ret.is_none());
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_fun_higher_order_param() {
        // FUN apply (f: (Int) -> Int, x: Int) -> Int { f(x) }
        let result = Parser::parse(
            "FUN apply (f: (Int) -> Int, x: Int) -> Int { f(x) }",
        )
        .unwrap();
        let stmt = result.ast.get_stmt(result.stmts[0]).unwrap();
        match stmt {
            Stmt::Fun {
                name, params, ret, ..
            } => {
                assert_eq!(name, "apply");
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "f");
                assert!(params[0].1.is_some()); // f has function type
                assert_eq!(params[1].0, "x");
                assert!(params[1].1.is_some());
                assert!(ret.is_some());
            }
            _ => panic!("expected Fun"),
        }
    }
}
