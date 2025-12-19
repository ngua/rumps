//! Parser for the RUMPS query language.
//!
//! # Architecture
//!
//! The parser uses a two-pass approach to decouple parsing from AST construction:
//!
//! ```text
//! Tokens  -->  CST (Concrete Syntax Tree)  -->  AST (arena-allocated)
//!              ^^^^^^^^^^^^^^^^^^^^^^^^^^       ^^^^^^^^^^^^^^^^^^^^^
//!              chumsky produces (pass 1)        lowering produces (pass 2)
//! ```
//!
//! This design eliminates the `Rc<RefCell<Ast>>` pattern previously required
//! by chumsky's `Clone` constraint on parsers. See `cst.rs` for details.
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

#![allow(dead_code)]
// NOTE: This is because `ParseErr = Simple<Token, Span>`, which can be quite
// large. Boxing it would infect the entire parser. This is only for errors,
// which are not the happy path, so I'm not too concerned about size here.
// It's also a warning for 136 bytes, which is not _that_ large and anyway
// `Box`ing would add allocation overhead
#![allow(clippy::result_large_err)]

use chumsky::prelude::{choice, end, just, recursive, select, Simple};
use chumsky::Parser as _;
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

mod cst;
mod lower;

use crate::ast::{BinOp, Literal, TypePattern, UnOp};
use crate::{Ast, Error, Lexer, Result, Span, Spanned, StmtId, Token};

/// Parser error type for token-based parsing.
type ParseErr = Simple<Token, Span>;

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
    ///
    /// This is the two-pass entry point:
    /// 1. Parse tokens into CST (this module)
    /// 2. Lower CST to AST (`lower.rs`)
    pub(crate) fn parse_tokens(tokens: &[Spanned]) -> Result<ParseResult> {
        let parser = Self::program();

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
            .map(|cst_stmts| {
                let (ast, stmts) = lower::program(cst_stmts);
                ParseResult { ast, stmts }
            })
    }

    /// Program: zero or more statements separated by newlines, ending with EOF.
    fn program() -> impl chumsky::Parser<Token, Vec<cst::Stmt>, Error = ParseErr>
    {
        Self::opt_newlines()
            .ignore_then(
                Self::stmt().separated_by(Self::newlines()).allow_trailing(),
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
    fn stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        recursive(|stmt| {
            let let_stmt = Self::let_stmt(stmt.clone());
            let set_stmt = Self::set_stmt(stmt.clone());
            let kill_stmt = Self::kill_stmt(stmt.clone());
            let output_stmt = Self::output_stmt(stmt.clone());
            let fun_stmt = Self::fun_stmt(stmt.clone());
            let expr_stmt = Self::expr_stmt(stmt);

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
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let type_ann =
            just(Token::Colon).ignore_then(Self::type_expr()).or_not();

        just(Token::Let)
            .ignore_then(Self::ident())
            .then(type_ann)
            .then_ignore(just(Token::Assign))
            .then(Self::expr(stmt))
            .map_with_span(|((name, ty_ann), val), span| {
                cst::Stmt::new(cst::StmtKind::Let(name, ty_ann, val), span)
            })
    }

    /// `SET name = expr` or `SET name(subs...) = expr`
    /// `SET ^global = expr` or `SET ^global(subs...) = expr`
    fn set_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let expr = Self::expr(stmt);

        let local_set = just(Token::Set)
            .ignore_then(Self::ident())
            .then(Self::subscripts(expr.clone()).or_not())
            .then_ignore(just(Token::Assign))
            .then(expr.clone())
            .map_with_span(|((name, subs), val), span| {
                let subs = subs.unwrap_or_default();
                let target =
                    cst::Expr::new(cst::ExprKind::Local(name, subs), span);
                cst::Stmt::new(cst::StmtKind::Set(target, val), span)
            });

        let global_set = just(Token::Set)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(expr.clone()).or_not())
            .then_ignore(just(Token::Assign))
            .then(expr)
            .map_with_span(|((name, subs), val), span| {
                let subs = subs.unwrap_or_default();
                let target =
                    cst::Expr::new(cst::ExprKind::Global(name, subs), span);
                cst::Stmt::new(cst::StmtKind::Set(target, val), span)
            });

        global_set.or(local_set)
    }

    /// `KILL name` or `KILL name(subs...)`
    /// `KILL ^global` or `KILL ^global(subs...)`
    fn kill_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let expr = Self::expr(stmt);

        let local_kill = just(Token::Kill)
            .ignore_then(Self::ident())
            .then(Self::subscripts(expr.clone()).or_not())
            .map_with_span(|(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let target =
                    cst::Expr::new(cst::ExprKind::Local(name, subs), span);
                cst::Stmt::new(cst::StmtKind::Kill(target), span)
            });

        let global_kill = just(Token::Kill)
            .ignore_then(Self::global_name())
            .then(Self::subscripts(expr).or_not())
            .map_with_span(|(name, subs), span| {
                let subs = subs.unwrap_or_default();
                let target =
                    cst::Expr::new(cst::ExprKind::Global(name, subs), span);
                cst::Stmt::new(cst::StmtKind::Kill(target), span)
            });

        global_kill.or(local_kill)
    }

    /// `OUTPUT expr`
    fn output_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        just(Token::Output)
            .ignore_then(Self::expr(stmt))
            .map_with_span(|expr, span| {
                cst::Stmt::new(cst::StmtKind::Output(expr), span)
            })
    }

    /// `FUN name (params) { body }` or `FUN name (params) -> Type { body }`
    fn fun_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        // Parameter: `name` or `name: Type`
        let param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr())
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
            .ignore_then(Self::type_expr())
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
                |(((name, params_vec), ret), (stmts, blk_span)), span| {
                    let params = SmallVec::from_vec(params_vec);
                    let body = Self::stmts_to_block(stmts, blk_span);
                    cst::Stmt::new(
                        cst::StmtKind::Fun {
                            name,
                            params,
                            ret,
                            body,
                        },
                        span,
                    )
                },
            )
    }

    /// Convert a list of statements to a block expression.
    ///
    /// If the last statement is `cst::StmtKind::Expr(e)`, extracts `e` as the
    /// trailing expression (block's value). Otherwise, the block has no tail.
    fn stmts_to_block(stmts: Vec<cst::Stmt>, span: Span) -> cst::Expr {
        // Check if last statement is Expr; if so, use it as tail
        let has_tail = stmts
            .last()
            .map(|s| matches!(&s.kind, cst::StmtKind::Expr(_)))
            .unwrap_or(false);

        if has_tail {
            let n = stmts.len().saturating_sub(1);
            let mut iter = stmts.into_iter();
            let block_stmts: Vec<_> = iter.by_ref().take(n).collect();
            let tail = iter.next().and_then(|s| match s.kind {
                cst::StmtKind::Expr(e) => Some(Box::new(e)),
                _ => None,
            });
            cst::Expr::new(cst::ExprKind::Block(block_stmts, tail), span)
        } else {
            cst::Expr::new(cst::ExprKind::Block(stmts, None), span)
        }
    }

    /// `{ stmts... }` block, returns statements and the block's span.
    fn block(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone,
    ) -> impl chumsky::Parser<Token, (Vec<cst::Stmt>, Span), Error = ParseErr> + Clone
    {
        Self::opt_newlines()
            .ignore_then(just(Token::LBrace))
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                stmt.separated_by(Self::newlines())
                    .allow_leading()
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|stmts, span| (stmts, span))
    }

    /// Expression used as statement.
    fn expr_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        Self::expr(stmt).map_with_span(|expr, span| {
            cst::Stmt::new(cst::StmtKind::Expr(expr), span)
        })
    }

    /// Top-level expression parser with full precedence.
    fn expr(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        recursive(move |expr| {
            let primary = Self::primary_expr(expr.clone(), stmt.clone());
            let postfix = Self::postfix_expr(expr.clone(), primary);
            let unary = Self::unary_expr(expr, postfix);
            let pow = Self::pow_expr(unary);
            let mul = Self::mul_expr(pow);
            let add = Self::add_expr(mul);
            let cmp = Self::cmp_expr(add);
            let is = Self::is_expr(cmp);
            let as_cast = Self::as_expr(is);
            let read = Self::read_expr(as_cast);
            let and = Self::and_expr(read);
            let or = Self::or_expr(and);
            Self::coalesce_expr(or)
        })
    }

    /// Coalesce: `expr ?? expr` (lowest precedence)
    fn coalesce_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::QuestionQuestion).to(BinOp::Coalesce);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Logical OR: `expr || expr` or `expr OR expr`
    fn or_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((just(Token::PipePipe), just(Token::Or))).to(BinOp::Or);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Logical AND: `expr && expr` or `expr AND expr`
    fn and_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((just(Token::AmpAmp), just(Token::And))).to(BinOp::And);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Comparison: `<`, `>`, `<=`, `>=`, `==`, `!=`
    fn cmp_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Eq).to(BinOp::Eq),
            just(Token::Ne).to(BinOp::Ne),
            just(Token::Le).to(BinOp::Le),
            just(Token::Ge).to(BinOp::Ge),
            just(Token::Lt).to(BinOp::Lt),
            just(Token::Gt).to(BinOp::Gt),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Type check: `expr is Pattern`
    fn is_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let type_pattern = Self::type_pattern();

        let is_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Is))
            .then_ignore(Self::opt_newlines())
            .ignore_then(type_pattern);

        operand.clone().then(is_rhs.or_not()).map_with_span(
            |(expr, pattern), span| match pattern {
                Some(pat) => {
                    cst::Expr::new(cst::ExprKind::Is(Box::new(expr), pat), span)
                }
                None => expr,
            },
        )
    }

    /// Parse a type pattern for the `is` operator.
    fn type_pattern(
    ) -> impl chumsky::Parser<Token, TypePattern, Error = ParseErr> + Clone
    {
        // Wildcard: `_`
        let wildcard = select! { Token::Ident(s) if s == "_" => () };

        // Binding name (any identifier except `_`)
        let binding = select! { Token::Ident(s) if s != "_" => s };

        // Pattern arguments: `(name)`, `(name1, name2)`, or `(_)`
        let pattern_args = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(choice((
                wildcard.to(PatternArgs::Wildcard),
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

        // Simple type pattern
        let simple_type = Self::ident().map(TypePattern::Type);

        variant_pattern.or(simple_type)
    }

    /// Type cast: `expr as Type`
    fn as_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let as_rhs = Self::opt_newlines()
            .ignore_then(just(Token::As))
            .then_ignore(Self::opt_newlines())
            .ignore_then(Self::type_expr());

        operand.clone().then(as_rhs.or_not()).map_with_span(
            |(expr, ty), span| match ty {
                Some(ty_expr) => cst::Expr::new(
                    cst::ExprKind::As(Box::new(expr), ty_expr),
                    span,
                ),
                None => expr,
            },
        )
    }

    /// Fallible conversion: `expr read Type`
    fn read_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let read_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Read))
            .then_ignore(Self::opt_newlines())
            .ignore_then(Self::type_expr());

        operand.clone().then(read_rhs.or_not()).map_with_span(
            |(expr, ty), span| match ty {
                Some(ty_expr) => cst::Expr::new(
                    cst::ExprKind::Read(Box::new(expr), ty_expr),
                    span,
                ),
                None => expr,
            },
        )
    }

    /// Additive: `+`, `-`, `++`
    fn add_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Plus).to(BinOp::Add),
            just(Token::Minus).to(BinOp::Sub),
            just(Token::Concat).to(BinOp::Concat),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Power: `**` (right-associative)
    fn pow_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op_rhs = Self::opt_newlines()
            .ignore_then(just(Token::StarStar))
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), _span| Self::fold_binary_right(first, rest),
        )
    }

    /// Folds a sequence of power operations right-to-left.
    fn fold_binary_right(
        first: cst::Expr,
        rest: Vec<(Token, cst::Expr)>,
    ) -> cst::Expr {
        rest.into_iter()
            .rfold(None, |acc: Option<cst::Expr>, (_tok, expr)| match acc {
                None => Some(expr),
                Some(rhs) => {
                    let span = Span::new(expr.span.start, rhs.span.end);
                    Some(cst::Expr::new(
                        cst::ExprKind::Binary(
                            Box::new(expr),
                            BinOp::Pow,
                            Box::new(rhs),
                        ),
                        span,
                    ))
                }
            })
            .map_or(first.clone(), |rhs| {
                let span = Span::new(first.span.start, rhs.span.end);
                cst::Expr::new(
                    cst::ExprKind::Binary(
                        Box::new(first),
                        BinOp::Pow,
                        Box::new(rhs),
                    ),
                    span,
                )
            })
    }

    /// Multiplicative: `*`, `/`, `//`, `%`
    fn mul_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Mul).to(BinOp::Mul),
            just(Token::FloorDiv).to(BinOp::FloorDiv),
            just(Token::Div).to(BinOp::Div),
            just(Token::Modulo).to(BinOp::Mod),
        ));
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Folds a sequence of binary operations left-to-right.
    fn fold_binary(
        first: cst::Expr,
        rest: Vec<(BinOp, cst::Expr)>,
        _outer_span: Span,
    ) -> cst::Expr {
        rest.into_iter().fold(first, |lhs, (op, rhs)| {
            let span = Span::new(lhs.span.start, rhs.span.end);
            cst::Expr::new(
                cst::ExprKind::Binary(Box::new(lhs), op, Box::new(rhs)),
                span,
            )
        })
    }

    /// Unary: `NOT`, `!`, `-`, `GET`
    fn unary_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Not).to(UnOp::Not),
            just(Token::Bang).to(UnOp::Not),
            just(Token::Minus).to(UnOp::Neg),
        ));

        recursive(move |unary| {
            let with_op = op.clone().then(unary.clone()).map_with_span(
                |(op, inner), span| {
                    cst::Expr::new(
                        cst::ExprKind::Unary(op, Box::new(inner)),
                        span,
                    )
                },
            );

            // GET target
            let get_expr = just(Token::Get)
                .ignore_then(Self::gettable(expr.clone()))
                .map_with_span(|inner, span| {
                    cst::Expr::new(cst::ExprKind::Get(Box::new(inner)), span)
                });

            choice((with_op, get_expr)).or(operand.clone())
        })
    }

    /// Target for `GET`: a local or global B-tree variable.
    fn gettable(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let global = Self::global_name()
            .then(Self::subscripts(expr.clone()).or_not())
            .map_with_span(|(name, subs), span| {
                let subs = subs.unwrap_or_default();
                cst::Expr::new(cst::ExprKind::Global(name, subs), span)
            });

        let local = Self::ident()
            .then(Self::subscripts(expr).or_not())
            .map_with_span(|(name, subs), span| {
                let subs = subs.unwrap_or_default();
                cst::Expr::new(cst::ExprKind::Local(name, subs), span)
            });

        choice((global, local))
    }

    /// Postfix: field access `.field`, index `[expr]`, call `(args...)`
    fn postfix_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Field access: `.field`
        let field = just(Token::Dot)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::Field);

        // Optional field access: `?.field`
        let opt_field = just(Token::QuestionDot)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::OptionalField);

        // Index: `[expr]`
        let index = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|idx, span| PostfixOp::Index(Box::new(idx), span));

        // Call: `(args...)`
        let call_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let call = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.separated_by(call_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(PostfixOp::Call);

        let postfix_op = choice((field, opt_field, index, call));

        operand
            .then(postfix_op.repeated())
            .map_with_span(|x, span| (x, span))
            .try_map(|((base, ops), span), _| {
                Self::fold_postfix(base, ops).ok_or_else(|| {
                    Simple::custom(span, "function call requires identifier")
                })
            })
    }

    /// Folds postfix operations left-to-right.
    fn fold_postfix(base: cst::Expr, ops: Vec<PostfixOp>) -> Option<cst::Expr> {
        ops.into_iter().try_fold(base, |acc, op| {
            let span = Span::new(acc.span.start, op.end().end);
            match op {
                PostfixOp::Field(name, _) => Some(cst::Expr::new(
                    cst::ExprKind::Field(Box::new(acc), name),
                    span,
                )),
                PostfixOp::OptionalField(name, _) => Some(cst::Expr::new(
                    cst::ExprKind::OptionalField(Box::new(acc), name),
                    span,
                )),
                PostfixOp::Index(idx, _) => Some(cst::Expr::new(
                    cst::ExprKind::Index(Box::new(acc), idx),
                    span,
                )),
                PostfixOp::Call(args, _) => {
                    // Check for variant constructor `Type.Variant(args)`
                    let variant_opt = match &acc.kind {
                        cst::ExprKind::Field(inner, var_name)
                            if var_name
                                .chars()
                                .next()
                                .is_some_and(|c| c.is_uppercase()) =>
                        {
                            match &inner.kind {
                                cst::ExprKind::Var(ty_name) => {
                                    Some((ty_name.clone(), var_name.clone()))
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    };

                    let kind = variant_opt.map_or_else(
                        || {
                            cst::ExprKind::Call(
                                Box::new(acc.clone()),
                                args.clone(),
                            )
                        },
                        |(ty_name, var_name)| {
                            cst::ExprKind::Variant(
                                ty_name,
                                var_name,
                                args.clone(),
                            )
                        },
                    );
                    Some(cst::Expr::new(kind, span))
                }
            }
        })
    }

    /// Primary: literals, identifiers, globals, parenthesized, arrays, objects, if.
    fn primary_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
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
            .map_with_span(|lit, span| {
                cst::Expr::new(cst::ExprKind::Literal(lit), span)
            });

        // Lexical variable
        let var = Self::ident().map_with_span(|name, span| {
            cst::Expr::new(cst::ExprKind::Var(name), span)
        });

        // Global with optional subscripts
        let global = Self::global_name()
            .then(Self::subscripts(expr.clone()).or_not())
            .map_with_span(|(name, subs), span| {
                let subs = subs.unwrap_or_default();
                cst::Expr::new(cst::ExprKind::Global(name, subs), span)
            });

        // Parenthesized expression
        let paren = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Array literal
        let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let array = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone().separated_by(arr_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|elems, span| {
                cst::Expr::new(cst::ExprKind::Array(elems), span)
            });

        // Object literal
        let obj_field = Self::ident()
            .then_ignore(just(Token::Colon))
            .then(expr.clone());

        let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let object = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(obj_field.separated_by(obj_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|fields, span| {
                cst::Expr::new(cst::ExprKind::Object(fields), span)
            });

        // Block expression
        let block_parser = Self::block(stmt);
        let block_expr =
            block_parser
                .clone()
                .map_with_span(|(stmts, blk_span), span| {
                    Self::stmts_to_block(stmts, blk_span).with_span(span)
                });

        // IF expression
        let if_expr = just(Token::If)
            .ignore_then(expr.clone())
            .then(block_parser.clone())
            .then(
                just(Token::Else)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(block_parser)
                    .or_not(),
            )
            .map_with_span(
                |((cond, (then_stmts, then_span)), else_block), span| {
                    let then_expr = Self::stmts_to_block(then_stmts, then_span);
                    let else_expr = else_block.map(|(stmts, blk_span)| {
                        Self::stmts_to_block(stmts, blk_span)
                    });
                    cst::Expr::new(
                        cst::ExprKind::If(
                            Box::new(cond),
                            Box::new(then_expr),
                            else_expr.map(Box::new),
                        ),
                        span,
                    )
                },
            );

        // Closure: single param `x => expr`
        let closure_single = Self::ident()
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr.clone())
            .map_with_span(|(name, body), span| {
                let params = smallvec::smallvec![(name, None)];
                cst::Expr::new(
                    cst::ExprKind::Closure {
                        params,
                        ret: None,
                        body: Box::new(body),
                    },
                    span,
                )
            });

        // Closure param: `name` or `name: Type`
        let closure_param = Self::ident()
            .then(
                just(Token::Colon)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr())
                    .or_not(),
            )
            .map(|(name, ty)| (name, ty));

        // Multi-param closure: `(params) => expr` or `(params) -> Type => expr`
        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let params_or_empty =
            just(Token::RParen).to(Vec::new()).or(closure_param
                .separated_by(param_sep)
                .allow_trailing()
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen)));
        let closure_multi = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(params_or_empty)
            .then_ignore(Self::opt_newlines())
            .then(
                just(Token::Arrow)
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(Self::type_expr())
                    .or_not(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|((params_vec, ret), body), span| {
                let params = SmallVec::from_vec(params_vec);
                cst::Expr::new(
                    cst::ExprKind::Closure {
                        params,
                        ret,
                        body: Box::new(body),
                    },
                    span,
                )
            });

        // Order matters (see original parser for rationale)
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

    /// Parse a global variable name.
    fn global_name(
    ) -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone {
        select! { Token::Global(s) => s }
    }

    /// Parse subscripts: `(expr, expr, ...)`
    fn subscripts(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<cst::Expr>, Error = ParseErr> + Clone
    {
        just(Token::LParen)
            .ignore_then(
                expr.separated_by(just(Token::Comma))
                    .at_least(1)
                    .allow_trailing(),
            )
            .then_ignore(just(Token::RParen))
    }

    /// Parse a type expression.
    fn type_expr(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        recursive(|ty| {
            // Type parameters: `[T]` or `[T, E]`
            let type_params = ty
                .clone()
                .separated_by(just(Token::Comma))
                .at_least(1)
                .delimited_by(just(Token::LBracket), just(Token::RBracket));

            // Atom: named type optionally with type params
            let atom = Self::ident().then(type_params.or_not()).map_with_span(
                |(name, params), span| {
                    let kind = match params {
                        None => cst::TypeExprKind::Named(name),
                        Some(ps) => cst::TypeExprKind::App(name, ps),
                    };
                    TypeAtomOrParams::Single(cst::TypeExpr::new(kind, span))
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
                    TypeAtomOrParams::Params(types, span)
                });

            // atom_or_params
            let atom_or_params = paren.or(atom);

            // Function type with `->`
            atom_or_params
                .then(
                    Self::opt_newlines()
                        .ignore_then(just(Token::Arrow))
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(ty)
                        .or_not(),
                )
                .try_map(|(left, arrow_ret), span| {
                    Self::build_fn_type(left, arrow_ret, span)
                })
        })
    }

    /// Build a function type or standalone type from parsed components.
    fn build_fn_type(
        left: TypeAtomOrParams,
        arrow_ret: Option<cst::TypeExpr>,
        span: Span,
    ) -> std::result::Result<cst::TypeExpr, ParseErr> {
        match (left, arrow_ret) {
            // `T -> R`: single param function
            (TypeAtomOrParams::Single(param), Some(ret)) => {
                Ok(cst::TypeExpr::new(
                    cst::TypeExprKind::Fn(vec![param], Box::new(ret)),
                    span,
                ))
            }
            // `(T, U, ...) -> R` or `() -> R`
            (TypeAtomOrParams::Params(params, _), Some(ret)) => {
                Ok(cst::TypeExpr::new(
                    cst::TypeExprKind::Fn(params, Box::new(ret)),
                    span,
                ))
            }
            // `T`: standalone type
            (TypeAtomOrParams::Single(ty), None) => Ok(ty),
            // `(T)`: parenthesized single type
            (TypeAtomOrParams::Params(mut params, _), None)
                if params.len() == 1 =>
            {
                params.pop().ok_or_else(|| {
                    Simple::custom(span, "internal: expected single type")
                })
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
#[derive(Clone)]
enum TypeAtomOrParams {
    Single(cst::TypeExpr),
    Params(Vec<cst::TypeExpr>, Span),
}

/// Helper enum for pattern arguments in `is` patterns.
#[derive(Clone)]
enum PatternArgs {
    Wildcard,
    Bindings(SmallVec<[String; 2]>),
}

/// Helper enum for postfix operations during folding.
enum PostfixOp {
    Field(String, Span),
    OptionalField(String, Span),
    Index(Box<cst::Expr>, Span),
    Call(Vec<cst::Expr>, Span),
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
    use crate::ast::{Expr, Stmt};

    fn parse_ok(src: &str) -> ParseResult {
        Parser::parse(src).expect("should parse")
    }

    fn parse_expr_ok(src: &str) -> (Ast, crate::ExprId) {
        let result = parse_ok(src);
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
    fn parse_binary() {
        let (ast, id) = parse_expr_ok("1 + 2");
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Add, _)) => (),
            _ => panic!("expected Binary Add"),
        }
    }

    #[test]
    fn parse_binary_precedence() {
        let (ast, id) = parse_expr_ok("1 + 2 * 3");
        // Should be 1 + (2 * 3)
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Add, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Literal(Literal::Int(1)))
                );
                match ast.get_expr(*rhs) {
                    Some(Expr::Binary(_, BinOp::Mul, _)) => (),
                    _ => panic!("expected Mul on right"),
                }
            }
            _ => panic!("expected Binary Add"),
        }
    }

    #[test]
    fn parse_let() {
        let result = parse_ok("LET x = 42");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Let(name, None, _)) => assert_eq!(name, "x"),
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn parse_set_local() {
        let result = parse_ok("SET x = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Set(target, _)) => match result.ast.get_expr(*target) {
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
    fn parse_set_global() {
        let result = parse_ok("SET ^DATA = 10");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Set(target, _)) => match result.ast.get_expr(*target) {
                Some(Expr::Global(name, _)) => assert_eq!(name, "DATA"),
                _ => panic!("expected Global"),
            },
            _ => panic!("expected Set"),
        }
    }

    #[test]
    fn parse_if_expr() {
        let (ast, id) = parse_expr_ok("IF TRUE { 1 }");
        match ast.get_expr(id) {
            Some(Expr::If(_, _, None)) => (),
            _ => panic!("expected If without else"),
        }
    }

    #[test]
    fn parse_if_else() {
        let (ast, id) = parse_expr_ok("IF TRUE { 1 } ELSE { 0 }");
        match ast.get_expr(id) {
            Some(Expr::If(_, _, Some(_))) => (),
            _ => panic!("expected If with else"),
        }
    }

    #[test]
    fn parse_array() {
        let (ast, id) = parse_expr_ok("[1, 2, 3]");
        match ast.get_expr(id) {
            Some(Expr::Array(elems)) => assert_eq!(elems.len(), 3),
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn parse_object() {
        let (ast, id) = parse_expr_ok("{ a: 1, b: 2 }");
        match ast.get_expr(id) {
            Some(Expr::Object(fields)) => assert_eq!(fields.len(), 2),
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn parse_closure_single() {
        let (ast, id) = parse_expr_ok("x => x * 2");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, .. }) => {
                assert_eq!(params.len(), 1);
                assert!(ret.is_none());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_multi() {
        let (ast, id) = parse_expr_ok("(a, b) => a + b");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, .. }) => {
                assert_eq!(params.len(), 2);
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_closure_typed() {
        let (ast, id) = parse_expr_ok("(x: Int) -> Int => x * x");
        match ast.get_expr(id) {
            Some(Expr::Closure { params, ret, .. }) => {
                assert_eq!(params.len(), 1);
                assert!(params[0].1.is_some());
                assert!(ret.is_some());
            }
            _ => panic!("expected Closure"),
        }
    }

    #[test]
    fn parse_fun() {
        let result = parse_ok("FUN add (a, b) { a + b }");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Fun { name, params, .. }) => {
                assert_eq!(name, "add");
                assert_eq!(params.len(), 2);
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_fun_typed() {
        let result = parse_ok("FUN square (x: Int) -> Int { x * x }");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Fun {
                name, params, ret, ..
            }) => {
                assert_eq!(name, "square");
                assert_eq!(params.len(), 1);
                assert!(params[0].1.is_some());
                assert!(ret.is_some());
            }
            _ => panic!("expected Fun"),
        }
    }

    #[test]
    fn parse_variant_constructor() {
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
    fn parse_is_pattern() {
        let (ast, id) = parse_expr_ok("x is Int");
        match ast.get_expr(id) {
            Some(Expr::Is(_, TypePattern::Type(ty))) => {
                assert_eq!(ty, "Int");
            }
            _ => panic!("expected Is with Type pattern"),
        }
    }

    #[test]
    fn parse_is_variant() {
        let (ast, id) = parse_expr_ok("x is Option.None");
        match ast.get_expr(id) {
            Some(Expr::Is(_, TypePattern::Variant(ty, var))) => {
                assert_eq!(ty, "Option");
                assert_eq!(var, "None");
            }
            _ => panic!("expected Is with Variant pattern"),
        }
    }

    #[test]
    fn parse_is_variant_bind() {
        let (ast, id) = parse_expr_ok("x is Option.Some(val)");
        match ast.get_expr(id) {
            Some(Expr::Is(_, TypePattern::VariantBind(ty, var, names))) => {
                assert_eq!(ty, "Option");
                assert_eq!(var, "Some");
                assert_eq!(names.len(), 1);
                assert_eq!(names[0], "val");
            }
            _ => panic!("expected Is with VariantBind pattern"),
        }
    }

    #[test]
    fn parse_coalesce() {
        let (ast, id) = parse_expr_ok("x ?? 0");
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Coalesce, _)) => (),
            _ => panic!("expected Coalesce"),
        }
    }

    #[test]
    fn parse_optional_field() {
        let (ast, id) = parse_expr_ok("x?.field");
        match ast.get_expr(id) {
            Some(Expr::OptionalField(_, f)) => assert_eq!(f, "field"),
            _ => panic!("expected OptionalField"),
        }
    }

    #[test]
    fn parse_as_cast() {
        let (ast, id) = parse_expr_ok("42 as Float");
        match ast.get_expr(id) {
            Some(Expr::As(_, _)) => (),
            _ => panic!("expected As"),
        }
    }

    #[test]
    fn parse_read_convert() {
        let (ast, id) = parse_expr_ok("\"42\" read Int");
        match ast.get_expr(id) {
            Some(Expr::Read(_, _)) => (),
            _ => panic!("expected Read"),
        }
    }

    #[test]
    fn parse_power() {
        let (ast, id) = parse_expr_ok("2 ** 3");
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Pow, _)) => (),
            _ => panic!("expected Pow"),
        }
    }

    #[test]
    fn parse_power_right_assoc() {
        let (ast, id) = parse_expr_ok("2 ** 3 ** 4");
        // Should be 2 ** (3 ** 4)
        match ast.get_expr(id) {
            Some(Expr::Binary(lhs, BinOp::Pow, rhs)) => {
                assert_eq!(
                    ast.get_expr(*lhs),
                    Some(&Expr::Literal(Literal::Int(2)))
                );
                match ast.get_expr(*rhs) {
                    Some(Expr::Binary(_, BinOp::Pow, _)) => (),
                    _ => panic!("expected Pow on right"),
                }
            }
            _ => panic!("expected Pow"),
        }
    }

    #[test]
    fn parse_function_type() {
        let result = parse_ok("LET f: (Int) -> Int = x => x");
        let stmt = result.ast.get_stmt(result.stmts[0]);
        match stmt {
            Some(Stmt::Let(_, Some(ty_id), _)) => {
                let ty = result.ast.get_type_expr(*ty_id);
                match ty {
                    Some(crate::ast::AstTypeExpr::Fn(params, _)) => {
                        assert_eq!(params.len(), 1);
                    }
                    _ => panic!("expected Fn type"),
                }
            }
            _ => panic!("expected Let with type"),
        }
    }

    #[test]
    fn parse_multiline_array() {
        let src = "[\n  1,\n  2,\n  3\n]";
        let (ast, id) = parse_expr_ok(src);
        match ast.get_expr(id) {
            Some(Expr::Array(elems)) => assert_eq!(elems.len(), 3),
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn parse_multiline_object() {
        let src = "{\n  a: 1,\n  b: 2\n}";
        let (ast, id) = parse_expr_ok(src);
        match ast.get_expr(id) {
            Some(Expr::Object(fields)) => assert_eq!(fields.len(), 2),
            _ => panic!("expected Object"),
        }
    }

    #[test]
    fn parse_expression_continuation() {
        let src = "1\n    + 2";
        let (ast, id) = parse_expr_ok(src);
        match ast.get_expr(id) {
            Some(Expr::Binary(_, BinOp::Add, _)) => (),
            _ => panic!("expected Binary Add"),
        }
    }

    #[test]
    fn parse_chained_call() {
        let (ast, id) = parse_expr_ok("f(1)(2)");
        match ast.get_expr(id) {
            Some(Expr::Call(callee, args)) => {
                assert_eq!(args.len(), 1);
                match ast.get_expr(*callee) {
                    Some(Expr::Call(_, inner_args)) => {
                        assert_eq!(inner_args.len(), 1);
                    }
                    _ => panic!("expected inner Call"),
                }
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn parse_method_call() {
        let (ast, id) = parse_expr_ok("obj.method(1)");
        match ast.get_expr(id) {
            Some(Expr::Call(callee, args)) => {
                assert_eq!(args.len(), 1);
                match ast.get_expr(*callee) {
                    Some(Expr::Field(_, name)) => assert_eq!(name, "method"),
                    _ => panic!("expected Field as callee"),
                }
            }
            _ => panic!("expected Call"),
        }
    }

    #[test]
    fn parse_iife() {
        let (ast, id) = parse_expr_ok("(x => x * 2)(21)");
        match ast.get_expr(id) {
            Some(Expr::Call(callee, args)) => {
                assert_eq!(args.len(), 1);
                match ast.get_expr(*callee) {
                    Some(Expr::Closure { .. }) => (),
                    _ => panic!("expected Closure as callee"),
                }
            }
            _ => panic!("expected Call"),
        }
    }
}
