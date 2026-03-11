//! Common parser utilities and helpers.

use chumsky::prelude::{choice, just, select};
use chumsky::Parser as _;

use super::{ParseErr, Parser};
use crate::intern::StringId;
use crate::parser::cst;
use crate::{Span, Token};

impl Parser {
    /// One or more newlines (skipping indentation tokens for now).
    pub(super) fn newlines(
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        Self::newline_or_indent().repeated().at_least(1).ignored()
    }

    /// Optional newlines.
    pub(super) fn opt_newlines(
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        Self::newline_or_indent().repeated().ignored()
    }

    /// A single newline or indentation token.
    pub(super) fn newline_or_indent(
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
    /// let x = 1, let y = 2
    /// let z = x + y
    ///
    /// match v { Foo => 1, Bar => 2 }
    /// ```
    pub(super) fn item_sep(
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        choice((
            Self::newlines(),
            just(Token::Comma)
                .then_ignore(Self::opt_newlines())
                .ignored(),
        ))
    }

    /// Parse an identifier token.
    pub(super) fn ident(
    ) -> impl chumsky::Parser<Token, StringId, Error = ParseErr> + Clone {
        select! { Token::Ident(s) => s }
    }

    /// Parse a contextual identifier by `StringId` comparison.
    ///
    /// Used for output modifiers (`json`, `to`, `error`, `file`) which are not
    /// keywords but recognized contextually after `write expr`.
    pub(super) fn ctx_ident(
        expected: StringId,
    ) -> impl chumsky::Parser<Token, (), Error = ParseErr> + Clone {
        select! { Token::Ident(s) if s == expected => () }
    }

    /// Parse a global variable name.
    pub(super) fn global_name(
    ) -> impl chumsky::Parser<Token, StringId, Error = ParseErr> + Clone {
        select! { Token::Global(s) => s }
    }

    /// Parse a B-tree variable reference (local or global with subscripts).
    ///
    /// Returns `cst::DbRef` for use in `get`, `set`, `kill`, `data`, `order`, `query`.
    ///
    /// NOTE: Bare locals (`name`) are NOT valid; use `name{}` for root refs.
    /// Bare identifiers are parsed as variable references by `ref_arg`.
    pub(super) fn db_ref(
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
    pub(super) fn ref_expr(
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

    /// Parse subscript contents (after the opening `{`) and closing `}`.
    ///
    /// Supports both regular subscript elements and spread syntax:
    /// - `d{1, "key"}` uses `Elem` for each subscript
    /// - `d{...keys}` uses `Spread` to expand an `Array[Subscript]`
    /// - `d{1, ...rest}` mixes both
    ///
    /// Used with `IdentBrace`/`GlobalBrace` tokens where `{` is already consumed.
    pub(super) fn subscript_contents(
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
    pub(super) fn subscripts(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<cst::SubscriptElem>, Error = ParseErr> + Clone
    {
        just(Token::LBrace).ignore_then(Self::subscript_contents(expr))
    }

    /// Convert statements to a block expression.
    ///
    /// If the last statement is an expression statement, extracts it as the
    /// block's trailing expression. Otherwise, the block has no trailing expression.
    pub(super) fn stmts_to_block(
        stmts: Vec<cst::Stmt>,
        span: Span,
    ) -> cst::Expr {
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

        cst::Expr::new(cst::ExprKind::Block(block_stmts, tail), span)
    }

    /// Parse a block: `{ stmts... [expr] }`.
    ///
    /// Returns the statements and the block's span.
    pub(super) fn block(
        stmt: impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, (Vec<cst::Stmt>, Span), Error = ParseErr> + Clone
    {
        just(Token::LBrace)
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
}
