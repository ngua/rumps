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

use chumsky::prelude::{
    choice, end, filter_map, just, recursive, select, Simple,
};
use chumsky::Parser as _;
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

mod cst;
mod lower;

use crate::ast::{BinOp, JsonAccessKind, Literal, UnOp};
use crate::parser::cst::TypePattern;
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

    /// Parse source code with a source file path for resolving relative imports.
    pub(crate) fn parse_with_path(
        src: &str,
        src_path: &std::path::Path,
    ) -> Result<ParseResult> {
        let tokens = Lexer::new(src).lex()?;
        Self::parse_tokens_with_path(&tokens, Some(src_path))
    }

    /// Parse a token stream into an AST.
    ///
    /// This is the two-pass entry point:
    /// 1. Parse tokens into CST (this module)
    /// 2. Lower CST to AST (`lower.rs`)
    pub(crate) fn parse_tokens(tokens: &[Spanned]) -> Result<ParseResult> {
        Self::parse_tokens_with_path(tokens, None)
    }

    /// Parse a token stream into an AST with optional source file context.
    fn parse_tokens_with_path(
        tokens: &[Spanned],
        src_path: Option<&std::path::Path>,
    ) -> Result<ParseResult> {
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
            .and_then(|cst_stmts| {
                let (ast, stmts) =
                    lower::program_with_path(cst_stmts, src_path)?;
                Ok(ParseResult { ast, stmts })
            })
    }

    /// Parse a token stream into CST (without lowering to AST).
    ///
    /// Used by `lower_module_from_file` to parse imported module files
    /// with the calling module's context for relative path resolution.
    pub(super) fn parse_to_cst(tokens: &[Spanned]) -> Result<Vec<cst::Stmt>> {
        let parser = Self::program();

        let eof_span = tokens
            .iter()
            .find_map(|s| matches!(s.tok, Token::Eof).then_some(s.span))
            .unwrap_or(Span::new(0, 0));

        let stream = chumsky::Stream::from_iter(
            eof_span,
            tokens
                .iter()
                .filter(|s| !matches!(s.tok, Token::Eof))
                .map(|s| (s.tok.clone(), s.span)),
        );

        parser.parse(stream).map_err(|errs| {
            NonEmpty::collect(errs.into_iter().map(Into::into))
                .map(Error::multiple)
                .unwrap_or_else(|| {
                    Error::runtime_no_span("unknown parse error")
                })
        })
    }

    /// Program: zero or more statements separated by newlines or commas.
    fn program() -> impl chumsky::Parser<Token, Vec<cst::Stmt>, Error = ParseErr>
    {
        Self::opt_newlines()
            .ignore_then(
                Self::stmt().separated_by(Self::item_sep()).allow_trailing(),
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

    /// Separator for items (statements, match arms): comma or newlines.
    ///
    /// Allows either:
    /// - A comma (optionally followed by newlines): `a, b` or `a,\n  b`
    /// - One or more newlines: `a\nb`
    ///
    /// This enables both inline and multi-line styles:
    /// ```text
    /// LET x = 1, LET y = 2
    /// LET z = x + y
    ///
    /// MATCH v { Foo => 1, Bar => 2 }
    /// ```
    fn item_sep() -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        choice((
            Self::newlines(),
            just(Token::Comma)
                .then_ignore(Self::opt_newlines())
                .ignored(),
        ))
    }

    /// A single statement.
    fn stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        recursive(|stmt| {
            let import_stmt = Self::import_stmt();
            let let_stmt = Self::let_stmt(stmt.clone());
            let set_stmt = Self::set_stmt(stmt.clone());
            let kill_stmt = Self::kill_stmt(stmt.clone());
            let output_stmt = Self::output_stmt(stmt.clone());
            let fun_stmt = Self::fun_stmt(stmt.clone());
            let type_stmt = Self::type_stmt();
            let newtype_stmt = Self::newtype_stmt();
            let union_stmt = Self::union_stmt();
            let module_stmt = Self::module_stmt(stmt.clone());
            let expr_stmt = Self::expr_stmt(stmt);

            choice((
                import_stmt,
                let_stmt,
                set_stmt,
                kill_stmt,
                output_stmt,
                fun_stmt,
                type_stmt,
                newtype_stmt,
                union_stmt,
                module_stmt,
                expr_stmt,
            ))
        })
    }

    /// `LET pattern = expr` or `LET pattern: Type = expr`
    ///
    /// Supports destructuring patterns:
    /// - `LET x = ...` (simple binding)
    /// - `LET (a, b) = ...` (tuple)
    /// - `LET { x, y } = ...` (object shorthand)
    /// - `LET { x: a, y: b } = ...` (object with rename)
    /// - `LET [a, b] = ...` (array)
    /// - `LET [h, ...t] = ...` (array with rest)
    /// - `LET _ = ...` (wildcard)
    fn let_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let type_ann =
            just(Token::Colon).ignore_then(Self::type_expr()).or_not();

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Let))
            .then(Self::binding_pattern())
            .then(type_ann)
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(Self::expr(stmt))
            .map_with_span(|(((vis, pat), ty_ann), val), span| {
                cst::Stmt::new(cst::StmtKind::Let(pat, ty_ann, val, vis), span)
            })
    }

    /// Parse a binding pattern for destructuring.
    fn binding_pattern(
    ) -> impl chumsky::Parser<Token, cst::BindingPattern, Error = ParseErr> + Clone
    {
        recursive(|pat| {
            // Wildcard: `_`
            let wildcard = select! { Token::Ident(s) if s == "_" => () }
                .to(cst::BindingPattern::Wildcard);

            // Simple variable: any identifier except `_`
            let var = select! { Token::Ident(s) if s != "_" => s }
                .map(cst::BindingPattern::Var);

            // Tuple pattern: `(a, b, c)` or `(a, b,)`
            let tuple_sep =
                just(Token::Comma).then_ignore(Self::opt_newlines());
            let tuple_pat = just(Token::LParen)
                .ignore_then(Self::opt_newlines())
                .ignore_then(
                    pat.clone()
                        .separated_by(tuple_sep)
                        .allow_trailing()
                        .at_least(1),
                )
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen))
                .map(cst::BindingPattern::Tuple);

            // Object field pattern: `name` (shorthand) or `name: pattern`
            let obj_field = select! { Token::Ident(s) if s != "_" => s }
                .then(
                    just(Token::Colon)
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(pat.clone())
                        .or_not(),
                )
                .map(|(name, maybe_pat)| {
                    let p = maybe_pat.unwrap_or_else(|| {
                        cst::BindingPattern::Var(name.clone())
                    });
                    (name, p)
                });

            // Object pattern: `{ name, age }` or `{ name: n, age: a }`
            let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let obj_pat = just(Token::LBrace)
                .ignore_then(Self::opt_newlines())
                .ignore_then(
                    obj_field
                        .separated_by(obj_sep)
                        .allow_trailing()
                        .at_least(1),
                )
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RBrace))
                .map(cst::BindingPattern::Object);

            // Rest patterns: `..` (ignore) or `...name` (bind)
            let rest_bind = just(Token::DotDotDot)
                .ignore_then(select! { Token::Ident(s) if s != "_" => s })
                .map(ArrayPatElem::RestBind);
            let rest_ignore = just(Token::DotDot).to(ArrayPatElem::RestIgnore);

            // Array element: rest-bind, rest-ignore, or regular pattern
            let arr_elem = rest_bind
                .or(rest_ignore)
                .or(pat.clone().map(ArrayPatElem::Pat));

            // Array pattern: `[a, b]` or `[head, ...tail]`
            let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let arr_pat = just(Token::LBracket)
                .ignore_then(Self::opt_newlines())
                .ignore_then(arr_elem.separated_by(arr_sep).allow_trailing())
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RBracket))
                .try_map(Self::build_array_pattern);

            choice((wildcard, tuple_pat, obj_pat, arr_pat, var))
        })
    }

    /// Build an array pattern from parsed elements.
    ///
    /// The rest pattern (`..` or `...name`) must be the last element if present.
    fn build_array_pattern(
        elems: Vec<ArrayPatElem>,
        span: Span,
    ) -> std::result::Result<cst::BindingPattern, ParseErr> {
        let mut pats = Vec::new();
        let mut rest: Option<cst::RestPattern> = None;

        elems.into_iter().try_for_each(|e| match e {
            ArrayPatElem::Pat(p) => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "rest pattern must be last in array destructuring",
                    ))
                } else {
                    pats.push(p);
                    Ok(())
                }
            }
            ArrayPatElem::RestIgnore => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "only one rest pattern allowed in array destructuring",
                    ))
                } else {
                    rest = Some(cst::RestPattern::Ignore);
                    Ok(())
                }
            }
            ArrayPatElem::RestBind(name) => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "only one rest pattern allowed in array destructuring",
                    ))
                } else {
                    rest = Some(cst::RestPattern::Bind(name));
                    Ok(())
                }
            }
        })?;

        Ok(cst::BindingPattern::Array(pats, rest))
    }

    /// Build a match array pattern from parsed elements.
    ///
    /// The rest pattern (`..` or `...name`) must be the last element if present.
    fn build_match_array_pattern(
        elems: Vec<MatchArrayPatElem>,
        span: Span,
    ) -> std::result::Result<cst::MatchPattern, ParseErr> {
        let mut pats = Vec::new();
        let mut rest: Option<cst::RestPattern> = None;

        elems.into_iter().try_for_each(|e| match e {
            MatchArrayPatElem::Pat(p) => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "rest pattern must be last in array pattern",
                    ))
                } else {
                    pats.push(p);
                    Ok(())
                }
            }
            MatchArrayPatElem::RestIgnore => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "only one rest pattern allowed in array pattern",
                    ))
                } else {
                    rest = Some(cst::RestPattern::Ignore);
                    Ok(())
                }
            }
            MatchArrayPatElem::RestBind(name) => {
                if rest.is_some() {
                    Err(Simple::custom(
                        span,
                        "only one rest pattern allowed in array pattern",
                    ))
                } else {
                    rest = Some(cst::RestPattern::Bind(name));
                    Ok(())
                }
            }
        })?;

        Ok(cst::MatchPattern::Array(pats, rest))
    }

    /// `SET name = expr` or `SET name(subs...) = expr`
    /// `SET ^global = expr` or `SET ^global(subs...) = expr`
    fn set_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let expr = Self::expr(stmt);

        just(Token::Set)
            .ignore_then(Self::ref_expr(expr.clone()))
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|(r, val), span| {
                cst::Stmt::new(cst::StmtKind::Set(r, val), span)
            })
    }

    /// `@KILL target`
    fn kill_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let expr = Self::expr(stmt);

        just(Token::Kill)
            .ignore_then(Self::ref_expr(expr))
            .map_with_span(|r, span| {
                cst::Stmt::new(cst::StmtKind::Kill(r), span)
            })
    }

    /// Parse a contextual identifier (case-insensitive match).
    ///
    /// Used for OUTPUT modifiers (`JSON`, `TO`, `ERROR`, `FILE`) which are not
    /// keywords but recognized contextually after `OUTPUT expr`.
    fn ctx_ident(
        expected: &'static str,
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        select! { Token::Ident(s) if s.eq_ignore_ascii_case(expected) => () }
    }

    /// `OUTPUT expr [JSON] [TO ERROR | TO FILE expr]`
    fn output_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        let format = Self::ctx_ident("JSON")
            .to(cst::OutputFormat::Json)
            .or_not()
            .map(|f| f.unwrap_or_default());

        let to_error = Self::ctx_ident("TO")
            .ignore_then(Self::ctx_ident("ERROR"))
            .to(cst::OutputTarget::Stderr);

        let to_file = Self::ctx_ident("TO")
            .ignore_then(Self::ctx_ident("FILE"))
            .ignore_then(Self::expr(stmt.clone()))
            .map(|e| cst::OutputTarget::File(Box::new(e)));

        let target =
            to_error.or(to_file).or_not().map(|t| t.unwrap_or_default());

        just(Token::Write)
            .ignore_then(Self::expr(stmt))
            .then(format)
            .then(target)
            .map_with_span(|((expr, format), target), span| {
                let output = cst::WriteStmt {
                    expr,
                    format,
                    target,
                };
                cst::Stmt::new(cst::StmtKind::Write(output), span)
            })
    }

    /// `WRITE expr [JSON] [TO ERROR | TO FILE expr]` as expression.
    ///
    /// Same syntax as `output_stmt`, but returns `cst::Expr` instead of
    /// `cst::Stmt`. Evaluates to `Unit` after performing the output.
    fn output_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let format = Self::ctx_ident("JSON")
            .to(cst::OutputFormat::Json)
            .or_not()
            .map(|f| f.unwrap_or_default());

        let to_error = Self::ctx_ident("TO")
            .ignore_then(Self::ctx_ident("ERROR"))
            .to(cst::OutputTarget::Stderr);

        let to_file = Self::ctx_ident("TO")
            .ignore_then(Self::ctx_ident("FILE"))
            .ignore_then(expr.clone())
            .map(|e| cst::OutputTarget::File(Box::new(e)));

        let target =
            to_error.or(to_file).or_not().map(|t| t.unwrap_or_default());

        just(Token::Write)
            .ignore_then(expr)
            .then(format)
            .then(target)
            .map_with_span(|((inner, format), target), span| {
                let output = cst::WriteStmt {
                    expr: inner,
                    format,
                    target,
                };
                cst::Expr::new(cst::ExprKind::Write(Box::new(output)), span)
            })
    }

    /// `@SET target = value` as expression.
    ///
    /// B-tree assignment that evaluates to `Unit`.
    fn set_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Set)
            .ignore_then(Self::ref_expr(expr.clone()))
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|(r, val), span| {
                cst::Expr::new(
                    cst::ExprKind::Set(Box::new(r), Box::new(val)),
                    span,
                )
            })
    }

    /// `@KILL target` as expression.
    ///
    /// Deletes a variable or subtree and evaluates to `Unit`.
    fn kill_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Kill)
            .ignore_then(Self::ref_expr(expr))
            .map_with_span(|r, span| {
                cst::Expr::new(cst::ExprKind::Kill(Box::new(r)), span)
            })
    }

    /// `RAISE expr` as expression.
    ///
    /// Raises a runtime error with the stringified value.
    fn raise_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Raise)
            .ignore_then(expr)
            .map_with_span(|e, span| {
                cst::Expr::new(cst::ExprKind::Raise(Box::new(e)), span)
            })
    }

    /// `FOREVER seed (state, cont) => body`
    ///
    /// Parses the forever loop expression.
    /// Uses `primary` for the seed (no postfix ops) to avoid parsing
    /// `(state, cont)` as a call expression.
    fn forever_expr(
        primary: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Parameter: `name` or `name: Type`
        let param = Self::ident().then(
            just(Token::Colon)
                .ignore_then(Self::opt_newlines())
                .ignore_then(Self::type_expr())
                .or_not(),
        );

        // Two parameters: `(state, cont)`
        let params = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(param.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Comma))
            .then_ignore(Self::opt_newlines())
            .then(param)
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen));

        // Use `primary` for seed (no postfix ops) to avoid parsing (state, cont) as call
        just(Token::Forever)
            .ignore_then(Self::opt_newlines())
            .ignore_then(primary)
            .then_ignore(Self::opt_newlines())
            .then(params)
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map_with_span(|((seed, (state_param, cont_param)), body), span| {
                cst::Expr::new(
                    cst::ExprKind::Forever {
                        seed: Box::new(seed),
                        state_param,
                        cont_param,
                        body: Box::new(body),
                    },
                    span,
                )
            })
    }

    /// Parse transaction modifiers (contextual identifiers).
    ///
    /// Syntax: `[ON CONFLICT ...] [WITH TIMEOUT expr] [WITH RETRIES n] [WITH ISOLATION ...]`
    fn transaction_modifiers(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::TransactionModifiers, Error = ParseErr>
           + Clone {
        // ON CONFLICT (ABORT | OVERWRITE)
        let conflict = Self::ctx_ident("ON")
            .ignore_then(Self::ctx_ident("CONFLICT"))
            .ignore_then(choice((
                Self::ctx_ident("ABORT").to(cst::ConflictModifier::Abort),
                Self::ctx_ident("OVERWRITE")
                    .to(cst::ConflictModifier::Overwrite),
            )));

        // WITH TIMEOUT expr
        let timeout = Self::ctx_ident("WITH")
            .ignore_then(Self::ctx_ident("TIMEOUT"))
            .ignore_then(expr)
            .map(Box::new);

        // WITH RETRIES n
        let retries = Self::ctx_ident("WITH")
            .ignore_then(Self::ctx_ident("RETRIES"))
            .ignore_then(select! { Token::Int(n) => n as u32 });

        // WITH ISOLATION SNAPSHOT
        let isolation = Self::ctx_ident("WITH")
            .ignore_then(Self::ctx_ident("ISOLATION"))
            .ignore_then(
                Self::ctx_ident("SNAPSHOT")
                    .to(cst::IsolationModifier::Snapshot),
            );

        // Modifiers must appear in this fixed order: conflict, timeout, retries,
        // isolation. Each modifier can appear at most once. Out-of-order or
        // duplicate modifiers will produce a parse error.
        conflict
            .or_not()
            .then(timeout.or_not())
            .then(retries.or_not())
            .then(isolation.or_not())
            .map(|(((conflict, timeout), retries), isolation)| {
                cst::TransactionModifiers {
                    conflict,
                    timeout,
                    retries,
                    isolation,
                }
            })
    }

    /// `TRANSACTION { stmts... [expr] } [modifiers]`
    fn transaction_expr(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        just(Token::Transaction)
            .ignore_then(Self::block(stmt))
            .then(Self::transaction_modifiers(expr))
            .map_with_span(|((stmts, _blk_span), modifiers), span| {
                // Split trailing expr statement from regular statements
                let has_tail = stmts
                    .last()
                    .map(|s| matches!(&s.kind, cst::StmtKind::Expr(_)))
                    .unwrap_or(false);

                let (block_stmts, tail) = if has_tail {
                    let n = stmts.len().saturating_sub(1);
                    let mut iter = stmts.into_iter();
                    let ss: Vec<_> = iter.by_ref().take(n).collect();
                    let t = iter.next().and_then(|s| match s.kind {
                        cst::StmtKind::Expr(e) => Some(Box::new(e)),
                        _ => None,
                    });
                    (ss, t)
                } else {
                    (stmts, None)
                };

                cst::Expr::new(
                    cst::ExprKind::Transaction(Box::new(
                        cst::TransactionExpr {
                            stmts: block_stmts,
                            expr: tail,
                            modifiers,
                        },
                    )),
                    span,
                )
            })
    }

    /// `FUN name (params) { body }` or `FUN name[T](params) -> Type { body }`
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

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Fun))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(Self::type_params())
            .then_ignore(Self::opt_newlines())
            .then(params)
            .then(ret_ty)
            .then(body)
            .map_with_span(
                |(
                    ((((vis, name), type_params), params_vec), ret),
                    (stmts, blk_span),
                ),
                 span| {
                    let params = SmallVec::from_vec(params_vec);
                    let body = Self::stmts_to_block(stmts, blk_span);
                    cst::Stmt::new(
                        cst::StmtKind::Fun {
                            name,
                            type_params,
                            params,
                            ret,
                            body,
                            vis,
                        },
                        span,
                    )
                },
            )
    }

    /// `TYPE Name = Variant1 | Variant2(T) | ...` (sum type)
    /// `TYPE Name[T] = Left(T) | Right(T)` (parameterized sum type)
    ///
    /// User-defined sum type declaration. For type aliases, use `NEWTYPE`.
    fn type_stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        // Variant: `Name` or `Name(Type, Type, ...)`
        let payload_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let payloads = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::type_expr().separated_by(payload_sep).allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .or_not()
            .map(|ps| ps.unwrap_or_default());

        // Uses `ident_or_contextual_keyword` because variant names like `Raise` may
        // also be keywords
        let variant = Self::ident_or_contextual_keyword()
            .then(payloads)
            .map(|(name, payloads)| cst::VariantCst { name, payloads });

        // Variants separated by `|`, allowing newlines
        let variant_sep = Self::opt_newlines()
            .ignore_then(just(Token::SinglePipe))
            .then_ignore(Self::opt_newlines());

        let sum_def = variant
            .separated_by(variant_sep)
            .at_least(1)
            .allow_leading() // Allow leading `|` for multi-line formatting
            .map(cst::TypeDefCst::Sum);

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Type))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(sum_def)
            .map_with_span(|(((vis, name), type_params), def), span| {
                cst::Stmt::new(
                    cst::StmtKind::Type {
                        name,
                        type_params,
                        def,
                        vis,
                    },
                    span,
                )
            })
    }

    /// `NEWTYPE Name = Type` (transparent type alias)
    /// `NEWTYPE Name[T] = Type` (parameterized type alias)
    ///
    /// Transparent type alias; `NEWTYPE I = Int` makes `I` interchangeable
    /// with `Int`.
    fn newtype_stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
    {
        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::NewType))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(Self::type_expr())
            .map_with_span(|(((vis, name), type_params), target), span| {
                cst::Stmt::new(
                    cst::StmtKind::NewType {
                        name,
                        type_params,
                        target,
                        vis,
                    },
                    span,
                )
            })
    }

    /// `UNION Name = Type1 | Type2 | ...`
    /// `UNION Name[T] = Type1 | Type2[T] | ...`
    ///
    /// Named union type declaration.
    fn union_stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
    {
        // Type members separated by `|`
        let member_sep = Self::opt_newlines()
            .ignore_then(just(Token::SinglePipe))
            .then_ignore(Self::opt_newlines());

        let members = Self::type_expr_atom()
            .separated_by(member_sep)
            .at_least(2)
            .allow_leading();

        // Optional `+` visibility prefix
        let vis = just(Token::Plus)
            .to(cst::Visibility::Public)
            .or_not()
            .map(|v| v.unwrap_or_default());

        vis.then_ignore(just(Token::Union))
            .then_ignore(Self::opt_newlines())
            .then(Self::ident())
            .then(Self::type_params())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(members)
            .map_with_span(|(((vis, name), type_params), members), span| {
                cst::Stmt::new(
                    cst::StmtKind::Union {
                        name,
                        type_params,
                        members,
                        vis,
                    },
                    span,
                )
            })
    }

    /// User-defined module declaration.
    ///
    /// Two forms are supported:
    /// - Inline: `MODULE Name { ... }`
    /// - File import: `MODULE Name FROM "path/to/module.rumps"`
    ///
    /// Accepts any statement inside inline modules; invalid statements
    /// (anything other than `FUN`, `LET`, or `MODULE`) are rejected during
    /// typechecking.
    fn module_stmt(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> {
        // Inline body: `{ ... }`
        let inline_body = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(stmt.separated_by(Self::item_sep()).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map(cst::ModuleSource::Inline);
        // File import: `FROM "path"`
        let file_import = just(Token::From)
            .ignore_then(Self::opt_newlines())
            .ignore_then(select! { Token::String(s) => s })
            .map(cst::ModuleSource::File);
        just(Token::Module)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::ident())
            .then_ignore(Self::opt_newlines())
            .then(inline_body.or(file_import))
            .map_with_span(|(name, source), span| {
                cst::Stmt::new(cst::StmtKind::Module { name, source }, span)
            })
    }

    /// `IMPORT Module.{ member, ... }` or `IMPORT Module.{ ... }`.
    ///
    /// Imports members from a module into the current scope.
    fn import_stmt() -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
    {
        // Module path: idents separated by `.`
        let path = Self::ident().separated_by(just(Token::Dot)).at_least(1);

        // Named with optional alias: `name` or `name AS alias`
        let named = Self::ident()
            .then(just(Token::As).ignore_then(Self::ident()).or_not())
            .map(|(name, alias)| cst::ImportItem::Named { name, alias });

        // Wildcard: `...`
        let wildcard = just(Token::DotDotDot).to(cst::ImportItem::Wildcard);

        // Exclusion: `-name`
        let exclude = just(Token::Minus)
            .ignore_then(Self::ident())
            .map(cst::ImportItem::Exclude);

        let item = choice((wildcard, exclude, named));

        let items = item
            .separated_by(just(Token::Comma))
            .allow_trailing()
            .delimited_by(just(Token::LBrace), just(Token::RBrace));

        just(Token::Import)
            .ignore_then(Self::opt_newlines())
            .ignore_then(path)
            .then_ignore(just(Token::Dot))
            .then(items)
            .map_with_span(|(path, items), span| {
                cst::Stmt::new(
                    cst::StmtKind::Import(cst::ImportStmt { path, items }),
                    span,
                )
            })
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
                stmt.separated_by(Self::item_sep())
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
        // Construct type_expr and type_pattern once BEFORE the recursive block.
        // Parser construction inside recursive() can cause stack overflow.
        let ty = Self::type_expr();
        let ty_pat = Self::type_pattern();

        recursive(move |expr| {
            // Define `pipe` (expr without CATCH) using nested recursive.
            // Intrinsics use `pipe` for operands so they don't consume CATCH.
            let pipe = recursive({
                let expr = expr.clone();
                let stmt = stmt.clone();
                let ty = ty.clone();
                let ty_pat = ty_pat.clone();
                move |pipe| {
                    let primary =
                        Self::primary_expr(expr.clone(), stmt.clone());
                    let postfix =
                        Self::postfix_expr(expr.clone(), primary.clone())
                            .boxed();
                    // Pass `pipe` to `unary_expr` for intrinsic operands
                    let unary = Self::unary_expr(pipe, primary, postfix);
                    let pow = Self::pow_expr(unary);
                    let mul = Self::mul_expr(pow).boxed();
                    let shift = Self::shift_expr(mul);
                    let add = Self::add_expr(shift);
                    let range = Self::range_expr(add);
                    let cmp = Self::cmp_expr(range).boxed();
                    let is = Self::is_expr(cmp, ty_pat.clone());
                    let matches = Self::matches_expr(is);
                    let as_cast = Self::as_expr(matches, ty.clone());
                    let read = Self::read_expr(as_cast, ty.clone()).boxed();
                    let bitand = Self::bitand_expr(read);
                    let bitor = Self::bitor_expr(bitand);
                    let and = Self::and_expr(bitor);
                    let or = Self::or_expr(and);
                    let coalesce = Self::coalesce_expr(or);
                    Self::pipe_expr(coalesce)
                }
            });

            Self::catch_expr(pipe)
        })
    }

    /// Pipeline: `expr |> expr`
    fn pipe_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::Pipe).to(BinOp::Pipe);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Catch: `expr CATCH handler` (loosest precedence)
    ///
    /// The handler is typically a closure: `expr CATCH e => handle(e)`.
    fn catch_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let catch_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Catch))
            .then_ignore(Self::opt_newlines())
            .ignore_then(operand.clone());

        operand.clone().then(catch_rhs.or_not()).map_with_span(
            |(expr, handler), span| match handler {
                Some(h) => cst::Expr::new(
                    cst::ExprKind::Catch(Box::new(expr), Box::new(h)),
                    span,
                ),
                None => expr,
            },
        )
    }

    /// Coalesce: `expr ?? expr`
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

    /// Bitwise OR: `expr | expr`
    fn bitor_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::SinglePipe).to(BinOp::BitOr);
        let op_rhs = Self::opt_newlines()
            .ignore_then(op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());
        operand.clone().then(op_rhs.repeated()).map_with_span(
            |(first, rest), span| Self::fold_binary(first, rest, span),
        )
    }

    /// Bitwise AND: `expr & expr`
    fn bitand_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = just(Token::Amp).to(BinOp::BitAnd);
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
        type_pattern: impl chumsky::Parser<Token, TypePattern, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
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

    /// Parse a `MATCHES` expression: `expr MATCHES regex`.
    fn matches_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let matches_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Matches))
            .then_ignore(Self::opt_newlines())
            .ignore_then(operand.clone());

        operand.clone().then(matches_rhs.or_not()).map_with_span(
            |(lhs, rhs), span| match rhs {
                Some(r) => cst::Expr::new(
                    cst::ExprKind::Matches(Box::new(lhs), Box::new(r)),
                    span,
                ),
                None => lhs,
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
        // Uses `ident_or_contextual_keyword` because variant names like `Raise` may
        // also be keywords
        let variant_pattern = Self::ident_or_contextual_keyword()
            .then_ignore(just(Token::Dot))
            .then(Self::ident_or_contextual_keyword())
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

        // Structural object pattern: `{ name: Type, age: Int }`
        // Using boxed() to reduce stack pressure from parser construction
        let field = Self::ident()
            .then_ignore(just(Token::Colon))
            .then(Self::type_expr())
            .boxed();
        let struct_pat = just(Token::LBrace)
            .ignore_then(
                field.separated_by(just(Token::Comma)).allow_trailing(),
            )
            .then_ignore(just(Token::RBrace))
            .map(TypePattern::Object);

        // Simple type pattern: `Int`, `Array[String]`, `Map[Int, String]`
        let simple_type = Self::simple_type_expr().map(TypePattern::Type);

        variant_pattern.or(struct_pat).or(simple_type)
    }

    /// Simplified type expression parser for use in type patterns.
    ///
    /// Supports named types, one level of type application (e.g., `Int`,
    /// `Array[String]`), and tuple types (e.g., `(Int, String)`). Does not
    /// support nested type params like `Map[K, V]` where K or V are themselves
    /// parameterized.
    ///
    /// This is intentionally non-recursive to avoid stack overflow issues
    /// when combined with the expression parser's recursive structure.
    fn simple_type_expr(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        // Inner type for type params: just named types, no nesting
        let inner_ty = Self::ident().map_with_span(|name, span| {
            cst::TypeExpr::new(cst::TypeExprKind::Named(name), span)
        });

        // Type parameters: `[T]` or `[T, E]` (one level only)
        let type_params = inner_ty
            .clone()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .delimited_by(just(Token::LBracket), just(Token::RBracket));

        // Named type optionally with type params
        let named = Self::ident().then(type_params.or_not()).map_with_span(
            |(name, params), span| {
                let kind = match params {
                    None => cst::TypeExprKind::Named(name),
                    Some(ps) => cst::TypeExprKind::App(name, ps),
                };
                cst::TypeExpr::new(kind, span)
            },
        );

        // Tuple types: `()`, `(T,)`, `(T, U, ...)`
        // Parse as (elem ,)* [elem] to track trailing commas
        let sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let elem_comma = inner_ty.clone().then_ignore(sep);
        let tuple = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(elem_comma.repeated().then(inner_ty.or_not()))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .try_map(|(with_comma, final_), span| {
                let mut elems: Vec<_> = with_comma;
                let trailing = final_.is_none() && !elems.is_empty();
                if let Some(f) = final_ {
                    elems.push(f);
                }
                // `(T)` without trailing comma is just parenthesized, not tuple
                if elems.len() == 1 && !trailing {
                    elems.into_iter().next().ok_or_else(|| {
                        Simple::custom(span, "internal: expected type")
                    })
                } else {
                    // `()`, `(T,)`, or `(T, U, ...)` are tuples
                    Ok(cst::TypeExpr::new(
                        cst::TypeExprKind::Tuple(elems),
                        span,
                    ))
                }
            });

        tuple.or(named)
    }

    /// Type cast: `expr AS Type`
    fn as_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        ty: impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let as_rhs = Self::opt_newlines()
            .ignore_then(just(Token::As))
            .then_ignore(Self::opt_newlines())
            .ignore_then(ty);

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

    /// Fallible conversion: `expr READ Type`
    fn read_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        ty: impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let read_rhs = Self::opt_newlines()
            .ignore_then(just(Token::Read))
            .then_ignore(Self::opt_newlines())
            .ignore_then(ty);

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

    /// Range: `start..end` (exclusive) or `start..=end` (inclusive)
    ///
    /// Creates a lazy range iterator. Binds looser than additive operators
    /// but tighter than comparison: `1..n + 1` parses as `1..(n + 1)`.
    fn range_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        // Parse the range operator and whether it's inclusive
        let range_op = choice((
            just(Token::DotDotEquals).to(true), // ..= inclusive
            just(Token::DotDot).to(false),      // .. exclusive
        ));

        let range_rhs = Self::opt_newlines()
            .ignore_then(range_op)
            .then_ignore(Self::opt_newlines())
            .then(operand.clone());

        operand.clone().then(range_rhs.or_not()).map_with_span(
            |(start, suffix), span| match suffix {
                Some((inclusive, end)) => {
                    let range_span = Span::new(start.span.start, end.span.end);
                    cst::Expr::new(
                        cst::ExprKind::Range(
                            Box::new(start),
                            Box::new(end),
                            inclusive,
                        ),
                        range_span,
                    )
                }
                None => start.with_span(span),
            },
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

    /// Shift: `<<`, `>>`
    fn shift_expr(
        operand: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let op = choice((
            just(Token::Shl).to(BinOp::Shl),
            just(Token::Shr).to(BinOp::Shr),
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

    /// Unary: `NOT`, `!`, `-`, and intrinsics/keywords (`@GET`, `@SET`, `RAISE`, etc.).
    ///
    /// `intrinsic_op` is the operand parser for intrinsics; it excludes `CATCH`
    /// so that `RAISE x CATCH ...` parses as `(RAISE x) CATCH ...`.
    fn unary_expr(
        intrinsic_op: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
        primary: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
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
            just(Token::Question).to(UnOp::Wrap),
        ));

        // Postfix operators for intrinsic expressions (e.g., `@GET d(1)!`).
        let postfix_ops = Self::postfix_ops(intrinsic_op.clone());

        recursive(move |unary| {
            let with_op = op.clone().then(unary.clone()).map_with_span(
                |(op, inner), span| {
                    cst::Expr::new(
                        cst::ExprKind::Unary(op, Box::new(inner)),
                        span,
                    )
                },
            );

            // GET target (with optional postfix ops like `!`)
            let get_expr = just(Token::Get)
                .ignore_then(Self::ref_expr(intrinsic_op.clone()))
                .map_with_span(|r, span| {
                    cst::Expr::new(cst::ExprKind::Get(Box::new(r)), span)
                })
                .then(postfix_ops.clone())
                .map_with_span(|(base, ops), span| {
                    Self::fold_postfix(base, ops).unwrap_or_else(|| {
                        cst::Expr::new(
                            cst::ExprKind::Error("postfix fold failed".into()),
                            span,
                        )
                    })
                });

            // DATA target (with optional postfix ops)
            let data_expr = just(Token::Data)
                .ignore_then(Self::ref_expr(intrinsic_op.clone()))
                .map_with_span(|r, span| {
                    cst::Expr::new(cst::ExprKind::Data(Box::new(r)), span)
                })
                .then(postfix_ops.clone())
                .map_with_span(|(base, ops), span| {
                    Self::fold_postfix(base, ops).unwrap_or_else(|| {
                        cst::Expr::new(
                            cst::ExprKind::Error("postfix fold failed".into()),
                            span,
                        )
                    })
                });

            // ORDER target (with optional postfix ops)
            let order_expr = just(Token::Order)
                .ignore_then(Self::ref_expr(intrinsic_op.clone()))
                .map_with_span(|r, span| {
                    cst::Expr::new(cst::ExprKind::Order(Box::new(r)), span)
                })
                .then(postfix_ops.clone())
                .map_with_span(|(base, ops), span| {
                    Self::fold_postfix(base, ops).unwrap_or_else(|| {
                        cst::Expr::new(
                            cst::ExprKind::Error("postfix fold failed".into()),
                            span,
                        )
                    })
                });

            // QUERY target (with optional postfix ops)
            let query_expr = just(Token::Query)
                .ignore_then(Self::ref_expr(intrinsic_op.clone()))
                .map_with_span(|r, span| {
                    cst::Expr::new(cst::ExprKind::Query(Box::new(r)), span)
                })
                .then(postfix_ops.clone())
                .map_with_span(|(base, ops), span| {
                    Self::fold_postfix(base, ops).unwrap_or_else(|| {
                        cst::Expr::new(
                            cst::ExprKind::Error("postfix fold failed".into()),
                            span,
                        )
                    })
                });

            // OUTPUT expr [JSON] [TO target]
            let output = Self::output_expr(intrinsic_op.clone());

            // SET target = value
            let set = Self::set_expr(intrinsic_op.clone());

            // KILL target
            let kill = Self::kill_expr(intrinsic_op.clone());

            // RAISE expr
            let raise = Self::raise_expr(intrinsic_op.clone());

            // FOREVER seed (state, cont) => body
            // Use primary for seed (no postfix ops) to avoid parsing (state, cont) as a call
            let forever = Self::forever_expr(primary, intrinsic_op);

            choice((
                with_op, get_expr, data_expr, order_expr, query_expr, output,
                set, kill, raise, forever,
            ))
            .or(operand.clone())
        })
    }

    /// Parse a B-tree variable reference (local or global with subscripts).
    ///
    /// Returns `cst::DbRef` for use in `GET`, `SET`, `KILL`, `DATA`, `ORDER`, `QUERY`.
    ///
    /// NOTE: Bare locals (`name`) are NOT valid; use `name{}` for root refs.
    /// Bare identifiers are parsed as variable references by `ref_arg`.
    fn db_ref(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::DbRef, Error = ParseErr> + Clone {
        // Global with subscripts: `^name{...}` (GlobalBrace token)
        // For root refs, use `^name{}` (empty subscripts)
        let global_with_subs = select! { Token::GlobalBrace(name) => name }
            .then(Self::subscript_contents(expr.clone()))
            .map(|(name, subs)| cst::DbRef::Global(name, subs));

        // Local with subscripts: `name{...}` (IdentBrace token)
        // For root refs, use `name{}` (empty subscripts)
        let local_with_subs = select! { Token::IdentBrace(name) => name }
            .then(Self::subscript_contents(expr))
            .map(|(name, subs)| cst::DbRef::Local(name, subs));

        // No bare cases; both `name` and `^name` require `{}`
        choice((global_with_subs, local_with_subs))
    }

    /// Parse a ref expression for intrinsics.
    ///
    /// Accepts either:
    /// - Inline `DbRef` as `RefLit`: `data{1}`, `^global{}`, `data{}`
    /// - Variable: bare identifier (must have type `Ref`)
    fn ref_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let ref_lit = Self::db_ref(expr).map_with_span(|dbref, span| {
            cst::Expr::new(cst::ExprKind::RefLit(dbref), span)
        });
        let var = Self::ident().map_with_span(|name, span| {
            cst::Expr::new(cst::ExprKind::Var(name), span)
        });
        // Try ref_lit first (more specific due to IdentBrace/GlobalBrace)
        ref_lit.or(var)
    }

    /// Postfix operators parser; returns zero or more `PostfixOp`s.
    ///
    /// Separated from `postfix_expr` so that other parsers (e.g. `unary_expr`
    /// for intrinsics) can also apply postfix operators.
    fn postfix_ops(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<PostfixOp>, Error = ParseErr> + Clone
    {
        // Field access: `.field` or tuple index `.0`, `.1`, etc.
        // Uses `ident_or_contextual_keyword` so keywords work as variant names
        // (e.g., `Action.Raise`)
        let field_or_tuple_idx = just(Token::Dot).ignore_then(
            // Try tuple index first (integer literal)
            select! { Token::Int(n) => n }
                .try_map(|n, span| {
                    u32::try_from(n).map_err(|_| {
                        Simple::custom(span, "tuple index too large")
                    })
                })
                .map_with_span(PostfixOp::TupleIndex)
                // Otherwise, it's a field access
                .or(Self::ident_or_contextual_keyword()
                    .map_with_span(PostfixOp::Field)),
        );

        // Optional field access: `?.field`
        let opt_field = just(Token::QuestionDot)
            .ignore_then(Self::ident_or_contextual_keyword())
            .map_with_span(PostfixOp::OptionalField);

        // Index: `[expr]`
        let index = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|idx, span| PostfixOp::Index(Box::new(idx), span));

        // Optional index: `?[expr]` (safe indexing, returns Option)
        let opt_index = just(Token::QuestionLBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|idx, span| {
                PostfixOp::OptionalIndex(Box::new(idx), span)
            });

        // Call: `(args...)`
        let call_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let call = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr.clone().separated_by(call_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
            .map_with_span(PostfixOp::Call);

        // Unwrap: `!` (postfix; extracts from Result/Option or errors)
        let unwrap =
            just(Token::Bang).map_with_span(|_, span| PostfixOp::Unwrap(span));

        // JSON scalar static field: `..field` (returns Option[T])
        // Uses DotDotNoSpace which requires no space before `..`.
        // Note: `.field` on Json is handled by regular Field postfix.
        let json_scalar_field = just(Token::DotDotNoSpace)
            .ignore_then(Self::ident())
            .map_with_span(PostfixOp::JsonScalarField);

        // JSON access with dynamic key: `->(expr)` (returns Json)
        let json_arrow_expr = just(Token::Arrow)
            .ignore_then(just(Token::LParen))
            .ignore_then(expr.clone())
            .then_ignore(just(Token::RParen))
            .map_with_span(|e, span| PostfixOp::JsonArrow(Box::new(e), span));

        // JSON scalar access with dynamic key: `->>(expr)` (returns Option[T])
        let json_arrow_arrow_expr = just(Token::ArrowArrow)
            .ignore_then(just(Token::LParen))
            .ignore_then(expr)
            .then_ignore(just(Token::RParen))
            .map_with_span(|e, span| {
                PostfixOp::JsonArrowArrow(Box::new(e), span)
            });

        // Type annotation: `: Type`
        let annotate = just(Token::Colon)
            .ignore_then(Self::opt_newlines())
            .ignore_then(Self::type_expr())
            .map_with_span(PostfixOp::Annotate);

        choice((
            field_or_tuple_idx,
            opt_field,
            index,
            opt_index,
            call,
            unwrap,
            // JSON scalar static field access `..field`
            json_scalar_field,
            // Dynamic key access with parens
            json_arrow_arrow_expr,
            json_arrow_expr,
            // Type annotation
            annotate,
        ))
        .repeated()
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
        operand
            .then(Self::postfix_ops(expr))
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
                PostfixOp::TupleIndex(idx, _) => Some(cst::Expr::new(
                    cst::ExprKind::TupleIndex(Box::new(acc), idx),
                    span,
                )),
                PostfixOp::Index(idx, _) => Some(cst::Expr::new(
                    cst::ExprKind::Index(Box::new(acc), idx),
                    span,
                )),
                PostfixOp::OptionalIndex(idx, _) => Some(cst::Expr::new(
                    cst::ExprKind::OptionalIndex(Box::new(acc), idx),
                    span,
                )),
                PostfixOp::Call(args, _) => Some(cst::Expr::new(
                    cst::ExprKind::Call(Box::new(acc), args),
                    span,
                )),
                PostfixOp::Unwrap(_) => Some(cst::Expr::new(
                    cst::ExprKind::Unwrap(Box::new(acc)),
                    span,
                )),
                PostfixOp::JsonScalarField(name, _) => Some(cst::Expr::new(
                    cst::ExprKind::JsonAccess(
                        Box::new(acc),
                        JsonAccessKind::Scalar,
                        cst::JsonAccessKey::Field(name),
                    ),
                    span,
                )),
                PostfixOp::JsonArrow(e, _) => Some(cst::Expr::new(
                    cst::ExprKind::JsonAccess(
                        Box::new(acc),
                        JsonAccessKind::Json,
                        cst::JsonAccessKey::Expr(e),
                    ),
                    span,
                )),
                PostfixOp::JsonArrowArrow(e, _) => Some(cst::Expr::new(
                    cst::ExprKind::JsonAccess(
                        Box::new(acc),
                        JsonAccessKind::Scalar,
                        cst::JsonAccessKey::Expr(e),
                    ),
                    span,
                )),
                PostfixOp::Annotate(ty, _) => Some(cst::Expr::new(
                    cst::ExprKind::Annotate(Box::new(acc), ty),
                    span,
                )),
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
        let char_lit = select! { Token::Char(c) => Literal::Char(c) };
        let str_lit = select! { Token::String(s) => Literal::String(s) };
        let bool_lit = choice((
            just(Token::True).to(Literal::Bool(true)),
            just(Token::False).to(Literal::Bool(false)),
        ));
        let null_lit = just(Token::Null).to(Literal::Null);
        let unit_lit =
            select! { Token::Ident(s) if s == "Unit" => Literal::Unit };

        let literal = choice((
            int_lit, float_lit, char_lit, str_lit, bool_lit, null_lit, unit_lit,
        ))
        .map_with_span(|lit, span| {
            cst::Expr::new(cst::ExprKind::Literal(lit), span)
        });

        // Regex literal: `/pattern/`
        let regex_lit =
            select! { Token::Regex(s) => s }.map_with_span(|pattern, span| {
                cst::Expr::new(cst::ExprKind::Regex(pattern), span)
            });

        // Interpolated string: `"Hello {name}!"`
        let interpolation = select! { Token::Interpolation(parts) => parts }
            .map_with_span(|parts, span| {
                cst::Expr::new(cst::ExprKind::Interpolation(parts), span)
            });

        // Database reference literals: `name{...}` or `^global{...}`.
        //
        // Creates a first-class `Ref` value. Uses `IdentBrace`/`GlobalBrace`
        // tokens which only form when there's NO space between name and `{`.
        // This allows `IF cond { ... }` to work (space means block, not ref).
        let ref_local = select! { Token::IdentBrace(name) => name }
            .then(Self::subscript_contents(expr.clone()))
            .map_with_span(|(name, subs), span| {
                cst::Expr::new(
                    cst::ExprKind::RefLit(cst::DbRef::Local(name, subs)),
                    span,
                )
            });
        let ref_global = select! { Token::GlobalBrace(name) => name }
            .then(Self::subscript_contents(expr.clone()))
            .map_with_span(|(name, subs), span| {
                cst::Expr::new(
                    cst::ExprKind::RefLit(cst::DbRef::Global(name, subs)),
                    span,
                )
            });

        // Lexical variable or mempty (`_`)
        let var = Self::ident().map_with_span(|name, span| {
            if name == "_" {
                cst::Expr::new(cst::ExprKind::Mempty, span)
            } else {
                cst::Expr::new(cst::ExprKind::Var(name), span)
            }
        });

        // Parenthesized expression or tuple literal
        // - `(expr)` -> parenthesized expression (unwrapped)
        // - `(expr,)` -> single-element tuple
        // - `(expr, expr, ...)` -> multi-element tuple
        // - `()` -> empty tuple
        //
        // Type annotations are handled as postfix operators: `expr: Type`.
        // This works uniformly for `(expr): T`, `(expr: T)`, `10: Int`, etc.
        //
        // We manually detect trailing comma instead of using `allow_trailing()`
        // so we can distinguish `(x)` from `(x,)`.
        let paren_or_tuple = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                // Empty: `()`
                just(Token::RParen).to(ParenContents::Empty).or(
                    // First element
                    expr.clone()
                        .then(
                            // More elements or trailing comma
                            just(Token::Comma)
                                .ignore_then(Self::opt_newlines())
                                .ignore_then(
                                    expr.clone()
                                        .separated_by(
                                            just(Token::Comma).then_ignore(
                                                Self::opt_newlines(),
                                            ),
                                        )
                                        .allow_trailing(),
                                )
                                .or_not(),
                        )
                        .then_ignore(Self::opt_newlines())
                        .then_ignore(just(Token::RParen))
                        .map(|(first, rest)| match rest {
                            None => {
                                // `(x)` - single element, no comma
                                ParenContents::Elements(vec![first], false)
                            }
                            Some(mut more) => {
                                // `(x,)` or `(x, y, ...)` - has comma
                                let mut elems = vec![first];
                                elems.append(&mut more);
                                ParenContents::Elements(elems, true)
                            }
                        }),
                ),
            )
            .map_with_span(|contents, span| match contents {
                ParenContents::Empty => {
                    cst::Expr::new(cst::ExprKind::Tuple(vec![]), span)
                }
                ParenContents::Elements(mut elems, has_comma) => {
                    if elems.len() == 1 && !has_comma {
                        // Single element without comma: unwrap parens
                        // SAFETY: len checked above; `unwrap` is OK
                        #[allow(clippy::unwrap_used)]
                        elems.pop().unwrap()
                    } else {
                        cst::Expr::new(cst::ExprKind::Tuple(elems), span)
                    }
                }
            });
        let paren = paren_or_tuple;

        // Array literal with spread support
        let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let spread_elem = just(Token::DotDotDot)
            .ignore_then(expr.clone())
            .map(cst::ArrayElem::Spread);
        let single_elem = expr.clone().map(cst::ArrayElem::Elem);
        let arr_elem = spread_elem.or(single_elem);
        let array = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(arr_elem.separated_by(arr_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .map_with_span(|elems, span| {
                cst::Expr::new(cst::ExprKind::Array(elems), span)
            });

        // Object/JSON literal with spread support:
        // - `{ field: expr, ... }` (unquoted keys → Object)
        // - `{ "field": expr, ... }` (quoted keys → JSON)
        // - `{ ...expr, field: value }` (spread + fields → Object)
        // Mixed quoted/unquoted keys produce a parse error.
        // Spreads are only valid in Object context (not JSON).

        // Field entry: either quoted or unquoted key
        let unquoted_key = Self::ident().map(|s| (s, false));
        let quoted_key = select! { Token::String(s) => (s, true) };
        let obj_key = quoted_key.or(unquoted_key);

        // Entry kind: Spread, or Field(key, quoted)
        #[derive(Clone)]
        enum ObjEntryKind {
            Field(String, cst::Expr, bool), // (key, value, quoted)
            Spread(cst::Expr),
        }

        let obj_field = obj_key
            .then_ignore(just(Token::Colon))
            .then(expr.clone())
            .map(|((key, quoted), value)| {
                ObjEntryKind::Field(key, value, quoted)
            });

        let obj_spread = just(Token::DotDotDot)
            .ignore_then(expr.clone())
            .map(ObjEntryKind::Spread);

        let obj_entry = obj_spread.or(obj_field);

        let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let object_or_json = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(obj_entry.separated_by(obj_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|entries: Vec<ObjEntryKind>, span| {
                // Collect fields and spreads
                let has_spread = entries
                    .iter()
                    .any(|e| matches!(e, ObjEntryKind::Spread(_)));
                let fields: Vec<_> = entries
                    .iter()
                    .filter_map(|e| match e {
                        ObjEntryKind::Field(k, _, q) => Some((k.clone(), *q)),
                        ObjEntryKind::Spread(_) => None,
                    })
                    .collect();

                let all_quoted =
                    !fields.is_empty() && fields.iter().all(|(_, q)| *q);
                let all_unquoted = fields.iter().all(|(_, q)| !*q);
                let has_mixed =
                    !all_quoted && !all_unquoted && !fields.is_empty();

                // Spread with quoted keys is an error
                if has_spread && all_quoted {
                    cst::Expr::new(
                        cst::ExprKind::Error(
                            "cannot use spread in JSON object (quoted keys)"
                                .into(),
                        ),
                        span,
                    )
                } else if has_mixed {
                    cst::Expr::new(
                        cst::ExprKind::Error(
                            "cannot mix quoted and unquoted keys in object"
                                .into(),
                        ),
                        span,
                    )
                } else if all_quoted && !fields.is_empty() {
                    // JSON (all quoted, no spreads)
                    let json_fields: Vec<(String, cst::Expr)> = entries
                        .into_iter()
                        .filter_map(|e| match e {
                            ObjEntryKind::Field(k, v, _) => Some((k, v)),
                            ObjEntryKind::Spread(_) => None, // unreachable
                        })
                        .collect();
                    cst::Expr::new(cst::ExprKind::Json(json_fields), span)
                } else {
                    // Object (unquoted keys, possibly with spreads)
                    let obj_entries: Vec<cst::ObjectEntry> = entries
                        .into_iter()
                        .map(|e| match e {
                            ObjEntryKind::Field(k, v, _) => {
                                cst::ObjectEntry::Field(k, v)
                            }
                            ObjEntryKind::Spread(e) => {
                                cst::ObjectEntry::Spread(e)
                            }
                        })
                        .collect();
                    cst::Expr::new(cst::ExprKind::Object(obj_entries), span)
                }
            });

        // Map literal: { key => value, ... }
        let map_entry = expr
            .clone()
            .then_ignore(just(Token::FatArrow))
            .then(expr.clone());

        let map_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let map_lit = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(map_entry.separated_by(map_sep).allow_trailing())
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace))
            .map_with_span(|entries, span| {
                cst::Expr::new(cst::ExprKind::MapLit(entries), span)
            });

        // Try object/json first (key + `:`), then fall back to map (expr + `=>`).
        //
        // NOTE: `{}` is ambiguous and parses as an empty object, not an empty map.
        // Use `Map.empty()` for empty maps.
        let object_or_map = object_or_json.or(map_lit);

        // Block expression
        let block_parser = Self::block(stmt.clone());
        let block_expr =
            block_parser
                .clone()
                .map_with_span(|(stmts, blk_span), span| {
                    Self::stmts_to_block(stmts, blk_span).with_span(span)
                });

        // Transaction expression
        let txn_expr = Self::transaction_expr(stmt, expr.clone());

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
                        type_params: vec![],
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

        // Multi-param closure: `(params) => expr`, `(params) -> Type => expr`,
        // or with type params: `[T](params) -> Type => expr`
        let param_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let params_or_empty =
            just(Token::RParen).to(Vec::new()).or(closure_param
                .separated_by(param_sep)
                .allow_trailing()
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen)));
        let closure_multi = Self::type_params()
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::LParen))
            .then_ignore(Self::opt_newlines())
            .then(params_or_empty)
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
            .then(expr.clone())
            .map_with_span(|(((type_params, params_vec), ret), body), span| {
                let params = SmallVec::from_vec(params_vec);
                cst::Expr::new(
                    cst::ExprKind::Closure {
                        type_params,
                        params,
                        ret,
                        body: Box::new(body),
                    },
                    span,
                )
            });

        // Match expression
        let match_expr = Self::match_expr(expr);

        // Order matters: ref literals before var (IdentBrace is distinct from
        // Ident so they won't conflict). Closures before var since both can
        // start with ident but closure needs `=>`.
        choice((
            literal,
            interpolation,
            regex_lit,
            closure_single,
            closure_multi,
            ref_local,
            ref_global,
            var,
            paren,
            array,
            object_or_map,
            block_expr,
            txn_expr,
            if_expr,
            match_expr,
        ))
    }

    /// Parse a match pattern.
    ///
    /// Patterns include wildcards, variables, literals, variants, objects, and
    /// tuples. This is recursive to handle nested patterns.
    fn match_pattern(
    ) -> impl chumsky::Parser<Token, cst::MatchPattern, Error = ParseErr> + Clone
    {
        recursive(|pat| {
            // Wildcard: `_`
            let wildcard = select! { Token::Ident(s) if s == "_" => () }
                .to(cst::MatchPattern::Wildcard);

            // Literals
            let int_lit = select! { Token::Int(n) => cst::MatchPattern::Literal(Literal::Int(n)) };
            let float_lit = select! {
                Token::Float(OrderedFloat(n)) => cst::MatchPattern::Literal(Literal::Float(n))
            };
            let char_lit = select! {
                Token::Char(c) => cst::MatchPattern::Literal(Literal::Char(c))
            };
            let str_lit = select! {
                Token::String(s) => cst::MatchPattern::Literal(Literal::String(s))
            };
            let bool_lit = choice((
                just(Token::True)
                    .to(cst::MatchPattern::Literal(Literal::Bool(true))),
                just(Token::False)
                    .to(cst::MatchPattern::Literal(Literal::Bool(false))),
            ));
            let null_lit =
                just(Token::Null).to(cst::MatchPattern::Literal(Literal::Null));
            let literal = choice((
                int_lit, float_lit, char_lit, str_lit, bool_lit, null_lit,
            ));

            // Variant pattern: `Type.Variant` or `Type.Variant(pat, pat, ...)`
            let variant_args_sep =
                just(Token::Comma).then_ignore(Self::opt_newlines());
            let variant_args = just(Token::LParen)
                .ignore_then(Self::opt_newlines())
                .ignore_then(
                    pat.clone().separated_by(variant_args_sep).allow_trailing(),
                )
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen));

            // Variant pattern: `Type.Variant` or `Module.Type.Variant`
            // Parse a path of at least two segments; the last is the variant,
            // everything else (joined by `.`) is the type path.
            // Uses `ident_or_contextual_keyword` because variant names like `Raise`
            // may also be keywords.
            let variant_pat = Self::ident_or_contextual_keyword()
                .separated_by(just(Token::Dot))
                .at_least(2)
                .then(variant_args.or_not())
                .try_map(|(segments, args), span| {
                    segments
                        .split_last()
                        .map(|(var, type_path)| {
                            cst::MatchPattern::Variant(
                                type_path.join("."),
                                var.clone(),
                                args.unwrap_or_default(),
                            )
                        })
                        .ok_or_else(|| {
                            chumsky::error::Simple::custom(
                                span,
                                "variant pattern requires at least Type.Variant",
                            )
                        })
                });

            // Tuple pattern: `(pat, pat, ...)`
            let tuple_sep =
                just(Token::Comma).then_ignore(Self::opt_newlines());
            let tuple_pat = just(Token::LParen)
                .ignore_then(Self::opt_newlines())
                .ignore_then(
                    pat.clone().separated_by(tuple_sep).allow_trailing(),
                )
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen))
                .map(cst::MatchPattern::Tuple);

            // Object pattern field: `name` (shorthand) or `name: pattern`
            let obj_field = Self::ident()
                .then(
                    just(Token::Colon)
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(pat.clone())
                        .or_not(),
                )
                .map(|(name, maybe_pat)| {
                    let p = maybe_pat.unwrap_or_else(|| {
                        cst::MatchPattern::Var(name.clone())
                    });
                    (name, p)
                });

            // Object pattern: `{ name, age }` or `{ name: n, age: a }`
            let obj_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let obj_pat = just(Token::LBrace)
                .ignore_then(Self::opt_newlines())
                .ignore_then(obj_field.separated_by(obj_sep).allow_trailing())
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RBrace))
                .map(cst::MatchPattern::Object);

            // Rest patterns for arrays: `..` (ignore) or `...name` (bind)
            let arr_rest_bind = just(Token::DotDotDot)
                .ignore_then(select! { Token::Ident(s) if s != "_" => s })
                .map(MatchArrayPatElem::RestBind);
            // Accept both DotDot and DotDotNoSpace for rest ignore in patterns
            let arr_rest_ignore = just(Token::DotDot)
                .or(just(Token::DotDotNoSpace))
                .to(MatchArrayPatElem::RestIgnore);

            // Array element: rest-bind, rest-ignore, or regular pattern
            let arr_elem = arr_rest_bind
                .or(arr_rest_ignore)
                .or(pat.clone().map(MatchArrayPatElem::Pat));

            // Array pattern: `[a, b]`, `[a, b, ..]`, or `[head, ...tail]`
            let arr_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let arr_pat = just(Token::LBracket)
                .ignore_then(Self::opt_newlines())
                .ignore_then(arr_elem.separated_by(arr_sep).allow_trailing())
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RBracket))
                .try_map(Self::build_match_array_pattern);

            // Variable: any identifier except `_`
            let var_pat = select! { Token::Ident(s) if s != "_" => s }
                .map(cst::MatchPattern::Var);

            // Type-narrowing pattern: `name IS Type`
            let is_pat = select! { Token::Ident(s) if s != "_" => s }
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::Is))
                .then_ignore(Self::opt_newlines())
                .then(Self::type_expr())
                .map(|(name, ty)| cst::MatchPattern::Is(name, ty));

            // Order: is_pat before var (so `x IS Type` is parsed correctly)
            // variant before var (so `Type.Variant` is parsed correctly)
            // arr_pat before literal (so `[1, 2]` parses as pattern)
            choice((
                wildcard,
                literal,
                variant_pat,
                tuple_pat,
                obj_pat,
                arr_pat,
                is_pat,
                var_pat,
            ))
        })
    }

    /// Parse a match arm: `pattern => body` or `pattern IF guard => body`.
    fn match_arm(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::MatchArm, Error = ParseErr> + Clone
    {
        Self::match_pattern()
            .then(
                Self::opt_newlines()
                    .ignore_then(just(Token::If))
                    .ignore_then(Self::opt_newlines())
                    .ignore_then(expr.clone())
                    .or_not(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::FatArrow))
            .then_ignore(Self::opt_newlines())
            .then(expr)
            .map(|((pattern, guard), body)| cst::MatchArm {
                pattern,
                guard,
                body,
            })
    }

    /// Parse a match expression: `MATCH expr { arm... }`.
    fn match_expr(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::Expr, Error = ParseErr> + Clone {
        let arms = just(Token::LBrace)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::match_arm(expr.clone())
                    .separated_by(Self::item_sep())
                    .allow_leading()
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBrace));

        just(Token::Match)
            .ignore_then(Self::opt_newlines())
            .ignore_then(expr)
            .then_ignore(Self::opt_newlines())
            .then(arms)
            .map_with_span(|(scrutinee, arms), span| {
                cst::Expr::new(
                    cst::ExprKind::Match(Box::new(scrutinee), arms),
                    span,
                )
            })
    }

    /// Parse an identifier token.
    fn ident() -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone
    {
        select! { Token::Ident(s) => s }
    }

    /// Parse an identifier or contextual keyword that can appear as a variant
    /// name. Keywords like `Raise`, `Catch`, `Write` can be used as enum
    /// variant names (e.g., `Error.Raise`).
    ///
    /// Uses `Token::as_contextual_ident` to stay synchronized with the token
    /// definitions.
    fn ident_or_contextual_keyword(
    ) -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone {
        filter_map(|span, tok: Token| match tok {
            Token::Ident(s) => Ok(s),
            _ => tok
                .as_contextual_ident()
                .map(|s| s.to_owned())
                .ok_or_else(|| Simple::custom(span, "expected identifier")),
        })
    }

    /// Parse a user-facing constraint name.
    ///
    /// Recognizes: `Numeric`, `Stringable`, `Jsonable`, `Subscriptable`,
    /// `Storable`, `Iterable[T]`, `Monoid`, `BitLike`, `Fallible[T]`.
    fn constraint(
    ) -> impl chumsky::Parser<Token, cst::ParamConstraint, Error = ParseErr> + Clone
    {
        // Inner type for HKT constraints: `[T]` or `[Int]` etc.
        // Supports simple named types only (no function types, tuples, etc.)
        let inner_type = just(Token::LBracket)
            .ignore_then(Self::type_expr_atom())
            .then_ignore(just(Token::RBracket));

        select! { Token::Ident(s) => s }
            .then(inner_type.or_not())
            .try_map(|(name, inner), span| match name.as_str() {
                "Numeric" => Ok(cst::ParamConstraint::Numeric),
                "Stringable" => Ok(cst::ParamConstraint::Stringable),
                "Jsonable" => Ok(cst::ParamConstraint::Jsonable),
                "Subscriptable" => Ok(cst::ParamConstraint::Subscriptable),
                "Storable" => Ok(cst::ParamConstraint::Storable),
                // HKT-style constraints; require explicit type (type param or concrete)
                "Iterable" => inner.map_or_else(
                    || {
                        Err(Simple::custom(
                            span,
                            "`Iterable` requires a type argument; use `Iterable[T]`",
                        ))
                    },
                    |ty| Ok(cst::ParamConstraint::Iterable(ty)),
                ),
                "Monoid" => Ok(cst::ParamConstraint::Monoid),
                "BitLike" => Ok(cst::ParamConstraint::BitLike),
                "Fallible" => inner.map_or_else(
                    || {
                        Err(Simple::custom(
                            span,
                            "`Fallible` requires a type argument; use `Fallible[T]`",
                        ))
                    },
                    |ty| Ok(cst::ParamConstraint::Fallible(ty)),
                ),
                _ => Err(Simple::custom(
                    span,
                    format!(
                        "unknown constraint `{name}`; valid constraints are: \
                         Numeric, Stringable, Jsonable, Subscriptable, \
                         Storable, Iterable[T], Monoid, BitLike, Fallible[T]"
                    ),
                )),
            })
    }

    /// Parse a type parameter with optional constraints: `T` or `T: C1 + C2`.
    fn type_param(
    ) -> impl chumsky::Parser<Token, cst::TypeParam, Error = ParseErr> + Clone
    {
        let constraints = just(Token::Colon)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::constraint()
                    .separated_by(
                        Self::opt_newlines()
                            .ignore_then(just(Token::Plus))
                            .then_ignore(Self::opt_newlines()),
                    )
                    .at_least(1),
            )
            .or_not()
            .map(|cs| SmallVec::from_vec(cs.unwrap_or_default()));

        Self::ident()
            .then(constraints)
            .map(|(name, constraints)| cst::TypeParam { name, constraints })
    }

    /// Parse a type parameter list: `[T]`, `[T, U]`, or `[T: C1, U: C2 + C3]`.
    fn type_params(
    ) -> impl chumsky::Parser<Token, Vec<cst::TypeParam>, Error = ParseErr> + Clone
    {
        let sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::type_param()
                    .separated_by(sep)
                    .at_least(1)
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .or_not()
            .map(|ps| ps.unwrap_or_default())
    }

    /// Parse a global variable name.
    fn global_name(
    ) -> impl chumsky::Parser<Token, String, Error = ParseErr> + Clone {
        select! { Token::Global(s) => s }
    }

    /// Parse subscripts: `{expr, expr, ...}` with optional spread.
    ///
    /// Supports both regular subscript elements and spread syntax:
    /// - `d{1, "key"}` uses `Elem` for each subscript
    /// - `d{...keys}` uses `Spread` to expand an `Array[Subscript]`
    /// - `d{1, ...rest}` mixes both
    ///
    /// Note: Uses `{}` to distinguish from function calls `f(args)`.
    /// Parse subscript contents (after the opening `{`) and closing `}`.
    ///
    /// Used with `IdentBrace`/`GlobalBrace` tokens where `{` is already consumed.
    fn subscript_contents(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<cst::SubscriptElem>, Error = ParseErr> + Clone
    {
        let spread = just(Token::DotDotDot)
            .ignore_then(expr.clone())
            .map(cst::SubscriptElem::Spread);
        let single = expr.map(cst::SubscriptElem::Elem);
        let elem = spread.or(single);

        elem.separated_by(just(Token::Comma))
            .allow_trailing()
            .then_ignore(just(Token::RBrace))
    }

    /// Parse full subscripts including opening `{` and closing `}`.
    ///
    /// Used for contexts where `{` is a separate token (not merged).
    fn subscripts(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<cst::SubscriptElem>, Error = ParseErr> + Clone
    {
        just(Token::LBrace).ignore_then(Self::subscript_contents(expr))
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

            // Atom: named type (possibly qualified: `Module.Type`) optionally with type params
            // Parse one or more idents separated by `.` and join them into a type path
            let atom = Self::ident()
                .separated_by(just(Token::Dot))
                .at_least(1)
                .then(type_params.or_not())
                .map_with_span(|(segments, params), span| {
                    let name = segments.join(".");
                    let kind = match params {
                        None => cst::TypeExprKind::Named(name),
                        Some(ps) => cst::TypeExprKind::App(name, ps),
                    };
                    TypeAtomOrParams::Single(cst::TypeExpr::new(kind, span))
                });

            // Parenthesized: `()`, `(T)`, `(T,)`, or `(T, U, ...)`
            // Parse as (elem ,)* [elem] to track trailing commas
            let sep = just(Token::Comma).then_ignore(Self::opt_newlines());
            let elem_comma =
                ty.clone().then_ignore(sep.clone()).map(|t| (t, true));
            let final_elem = ty.clone().map(|t| (t, false));
            let paren = just(Token::LParen)
                .ignore_then(Self::opt_newlines())
                .ignore_then(elem_comma.repeated().then(final_elem.or_not()))
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RParen))
                .map_with_span(|(with_comma, final_), span| {
                    let mut elems: Vec<_> =
                        with_comma.into_iter().map(|(t, _)| t).collect();
                    let trailing = final_.is_none() && !elems.is_empty();
                    if let Some((f, _)) = final_ {
                        elems.push(f);
                    }
                    TypeAtomOrParams::Params(elems, span, trailing)
                });

            // Structural object type: `{ field: Type, ... }`
            let struct_field = Self::ident()
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::Colon))
                .then_ignore(Self::opt_newlines())
                .then(ty.clone());
            let struct_ty = just(Token::LBrace)
                .ignore_then(Self::opt_newlines())
                .ignore_then(struct_field.separated_by(sep).allow_trailing())
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::RBrace))
                .map_with_span(|fields, span| {
                    TypeAtomOrParams::Single(cst::TypeExpr::new(
                        cst::TypeExprKind::Object(fields),
                        span,
                    ))
                });

            // atom_or_params: structural object, parenthesized, or named type
            let atom_or_params = struct_ty.or(paren).or(atom);

            // Function type with `->`
            let fn_or_single = atom_or_params
                .then(
                    Self::opt_newlines()
                        .ignore_then(just(Token::Arrow))
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(ty)
                        .or_not(),
                )
                .try_map(|(left, arrow_ret), span| {
                    Self::build_fn_type(left, arrow_ret, span)
                });

            // Union type: `T | U | ...`
            // Unions bind looser than function types, so `A | B -> C` = `A | (B -> C)`
            fn_or_single
                .clone()
                .then(
                    Self::opt_newlines()
                        .ignore_then(just(Token::SinglePipe))
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(fn_or_single)
                        .repeated(),
                )
                .map_with_span(|(first, rest), span| {
                    if rest.is_empty() {
                        first
                    } else {
                        let mut members = vec![first];
                        members.extend(rest);
                        cst::TypeExpr::new(
                            cst::TypeExprKind::Union(members),
                            span,
                        )
                    }
                })
        })
    }

    /// Parse a type expression atom (named type with optional params).
    ///
    /// Does not parse unions or function types; used for simple contexts.
    /// Supports qualified names like `Module.Type`.
    fn type_expr_atom(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        // Type parameters: `[T]` or `[T, E]`
        let type_params = Self::type_expr()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .delimited_by(just(Token::LBracket), just(Token::RBracket));

        Self::ident()
            .separated_by(just(Token::Dot))
            .at_least(1)
            .then(type_params.or_not())
            .map_with_span(|(segments, params), span| {
                let name = segments.join(".");
                let kind = match params {
                    None => cst::TypeExprKind::Named(name),
                    Some(ps) => cst::TypeExprKind::App(name, ps),
                };
                cst::TypeExpr::new(kind, span)
            })
    }

    /// Build a function type, tuple type, or standalone type from parsed components.
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
            (TypeAtomOrParams::Params(params, _, _), Some(ret)) => {
                Ok(cst::TypeExpr::new(
                    cst::TypeExprKind::Fn(params, Box::new(ret)),
                    span,
                ))
            }
            // `T`: standalone type
            (TypeAtomOrParams::Single(ty), None) => Ok(ty),
            // `(T)` without trailing comma: parenthesized single type
            (TypeAtomOrParams::Params(mut params, _, false), None)
                if params.len() == 1 =>
            {
                params.pop().ok_or_else(|| {
                    Simple::custom(span, "internal: expected single type")
                })
            }
            // `(T,)` with trailing comma: single-element tuple type
            (TypeAtomOrParams::Params(params, _, true), None)
                if params.len() == 1 =>
            {
                Ok(cst::TypeExpr::new(cst::TypeExprKind::Tuple(params), span))
            }
            // `()`: empty tuple / unit type
            (TypeAtomOrParams::Params(params, _, _), None)
                if params.is_empty() =>
            {
                Ok(cst::TypeExpr::new(cst::TypeExprKind::Tuple(params), span))
            }
            // `(T, U, ...)`: multi-element tuple type
            (TypeAtomOrParams::Params(params, _, _), None) => {
                Ok(cst::TypeExpr::new(cst::TypeExprKind::Tuple(params), span))
            }
        }
    }
}

/// Helper for parsing function type syntax.
#[derive(Clone)]
enum TypeAtomOrParams {
    Single(cst::TypeExpr),
    /// (types, span, has_trailing_comma)
    Params(Vec<cst::TypeExpr>, Span, bool),
}

/// Helper enum for pattern arguments in `is` patterns.
#[derive(Clone)]
enum PatternArgs {
    Wildcard,
    Bindings(SmallVec<[String; 2]>),
}

/// Helper enum for parenthesized expressions vs tuples.
#[derive(Clone)]
enum ParenContents {
    Empty,
    Elements(Vec<cst::Expr>, bool), // (elements, has_trailing_comma)
}

/// Helper enum for postfix operations during folding.
enum PostfixOp {
    Field(String, Span),
    OptionalField(String, Span),
    TupleIndex(u32, Span),
    Index(Box<cst::Expr>, Span),
    /// Safe index access: `?[expr]` (returns `Option[T]`).
    OptionalIndex(Box<cst::Expr>, Span),
    Call(Vec<cst::Expr>, Span),
    Unwrap(Span),
    /// JSON scalar static field: `..field` (returns `Option[T]`).
    JsonScalarField(String, Span),
    /// JSON access with dynamic key: `->(expr)` (returns `Json`).
    JsonArrow(Box<cst::Expr>, Span),
    /// JSON scalar access with dynamic key: `->>(expr)` (returns `Option[T]`).
    JsonArrowArrow(Box<cst::Expr>, Span),
    /// Type annotation: `expr: Type`.
    Annotate(cst::TypeExpr, Span),
}

/// Helper enum for array pattern elements during parsing.
#[derive(Clone)]
enum ArrayPatElem {
    /// Regular pattern: `a`, `(x, y)`, etc.
    Pat(cst::BindingPattern),
    /// Rest ignore: `..`
    RestIgnore,
    /// Rest bind: `...name`
    RestBind(String),
}

/// Helper enum for match array pattern elements during parsing.
#[derive(Clone)]
enum MatchArrayPatElem {
    /// Regular pattern: `a`, `(x, y)`, `Option.Some(x)`, etc.
    Pat(cst::MatchPattern),
    /// Rest ignore: `..`
    RestIgnore,
    /// Rest bind: `...name`
    RestBind(String),
}

impl PostfixOp {
    fn end(&self) -> Span {
        match self {
            Self::Field(_, s)
            | Self::OptionalField(_, s)
            | Self::TupleIndex(_, s)
            | Self::Index(_, s)
            | Self::OptionalIndex(_, s)
            | Self::Call(_, s)
            | Self::Unwrap(s)
            | Self::JsonScalarField(_, s)
            | Self::JsonArrow(_, s)
            | Self::JsonArrowArrow(_, s)
            | Self::Annotate(_, s) => *s,
        }
    }
}
