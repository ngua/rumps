//! Postfix operation parsing for RUMPS expressions.

use chumsky::prelude::{choice, just, select};
use chumsky::Parser as _;

use super::{ParseErr, Parser};
use crate::ast::JsonAccessKind;
use crate::intern::StringId;
use crate::parser::cst;
use crate::{Span, Token};

impl Parser {
    /// Postfix operators parser; returns zero or more `PostfixOp`s.
    ///
    /// Separated from `postfix_expr` so that other parsers (e.g. `unary_expr`
    /// for intrinsics) can also apply postfix operators.
    pub(super) fn postfix_ops(
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, Vec<PostfixOp>, Error = ParseErr> + Clone
    {
        // Field access: `.field` or tuple index `.0`, `.1`, etc.
        let field_or_tuple_idx = just(Token::Dot).ignore_then(
            // Try tuple index first (integer literal)
            select! { Token::Int(n) => n }
                .try_map(|n, span| {
                    u32::try_from(n).map_err(|_| {
                        chumsky::error::Simple::custom(
                            span,
                            "tuple index too large",
                        )
                    })
                })
                .map_with_span(PostfixOp::TupleIndex)
                // Otherwise, it's a field access
                .or(Self::ident().map_with_span(PostfixOp::Field)),
        );

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
        ))
        .repeated()
    }

    /// Postfix: field access `.field`, index `[expr]`, call `(args...)`
    pub(super) fn postfix_expr(
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
                    chumsky::error::Simple::custom(
                        span,
                        "function call requires identifier",
                    )
                })
            })
    }

    /// Folds postfix operations left-to-right.
    pub(super) fn fold_postfix(
        base: cst::Expr,
        ops: Vec<PostfixOp>,
    ) -> Option<cst::Expr> {
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
                PostfixOp::JsonArrow(key, _) => Some(cst::Expr::new(
                    cst::ExprKind::JsonAccess(
                        Box::new(acc),
                        JsonAccessKind::Json,
                        cst::JsonAccessKey::Expr(key),
                    ),
                    span,
                )),
                PostfixOp::JsonArrowArrow(key, _) => Some(cst::Expr::new(
                    cst::ExprKind::JsonAccess(
                        Box::new(acc),
                        JsonAccessKind::Scalar,
                        cst::JsonAccessKey::Expr(key),
                    ),
                    span,
                )),
            }
        })
    }
}

/// Helper enum for postfix operations during folding.
pub(super) enum PostfixOp {
    Field(StringId, Span),
    OptionalField(StringId, Span),
    TupleIndex(u32, Span),
    Index(Box<cst::Expr>, Span),
    /// Safe index access: `?[expr]` (returns `Option[T]`).
    OptionalIndex(Box<cst::Expr>, Span),
    Call(Vec<cst::Expr>, Span),
    Unwrap(Span),
    /// JSON scalar static field: `..field` (returns `Option[T]`).
    JsonScalarField(StringId, Span),
    /// JSON access with dynamic key: `->(expr)` (returns `Json`).
    JsonArrow(Box<cst::Expr>, Span),
    /// JSON scalar access with dynamic key: `->>(expr)` (returns `Option[T]`).
    JsonArrowArrow(Box<cst::Expr>, Span),
}

impl PostfixOp {
    pub(super) fn end(&self) -> Span {
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
            | Self::JsonArrowArrow(_, s) => *s,
        }
    }
}
