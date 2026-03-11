//! Pattern parsing for binding destructuring and match expressions.

use chumsky::prelude::{choice, just, recursive, select};
use chumsky::Parser as _;
use ordered_float::OrderedFloat;

use super::{ParseErr, Parser};
use crate::ast::{Literal, NumericLit};
use crate::intern::{StringId, StringInterner};
use crate::parser::cst;
use crate::{Span, Token};

impl Parser {
    /// Parse a binding pattern for destructuring.
    pub(super) fn binding_pattern(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::BindingPattern, Error = ParseErr> + Clone
    {
        let underscore = interner.intern("_");

        recursive(move |pat| {
            // Wildcard: `_`
            let wildcard = select! { Token::Ident(s) if s == underscore => () }
                .to(cst::BindingPattern::Wildcard);

            // Simple variable: any identifier except `_`
            let var = select! { Token::Ident(s) if s != underscore => s }
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
            let obj_field = select! { Token::Ident(s) if s != underscore => s }
                .then(
                    just(Token::Colon)
                        .ignore_then(Self::opt_newlines())
                        .ignore_then(pat.clone())
                        .or_not(),
                )
                .map(|(name, maybe_pat)| {
                    let p = maybe_pat.unwrap_or(cst::BindingPattern::Var(name));
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
                .ignore_then(
                    select! { Token::Ident(s) if s != underscore => s },
                )
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
    pub(super) fn build_array_pattern(
        elems: Vec<ArrayPatElem>,
        span: Span,
    ) -> std::result::Result<cst::BindingPattern, ParseErr> {
        let mut pats = Vec::new();
        let mut rest: Option<cst::RestPattern> = None;

        elems.into_iter().try_for_each(|e| match e {
            ArrayPatElem::Pat(p) => {
                if rest.is_some() {
                    Err(chumsky::error::Simple::custom(
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
                    Err(chumsky::error::Simple::custom(
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
                    Err(chumsky::error::Simple::custom(
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

    /// Parse a match pattern.
    ///
    /// Patterns include wildcards, variables, literals, variants, objects, and
    /// tuples. This is recursive to handle nested patterns.
    pub(super) fn match_pattern(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::MatchPattern, Error = ParseErr> + Clone
    {
        let underscore = interner.intern("_");
        let ty = Self::type_expr(interner);

        recursive(move |pat| {
            // Wildcard: `_`
            let wildcard = select! { Token::Ident(s) if s == underscore => () }
                .to(cst::MatchPattern::Wildcard);

            // Literals
            let int_lit = select! {
                Token::Int(n) => cst::MatchPattern::Literal(Literal::Numeric(NumericLit::Int(n)))
            };
            let float_lit = select! {
                Token::Float(OrderedFloat(n)) => cst::MatchPattern::Literal(Literal::Numeric(NumericLit::Float(n)))
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
            let variant_pat = Self::ident()
                .separated_by(just(Token::Dot))
                .at_least(2)
                .then(variant_args.or_not())
                .try_map(|(segments, args), span| {
                    segments
                        .split_last()
                        .map(|(var, type_path)| {
                            cst::MatchPattern::Variant(
                                type_path.to_vec(),
                                *var,
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
                    let p = maybe_pat.unwrap_or(cst::MatchPattern::Var(name));
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
                .ignore_then(
                    select! { Token::Ident(s) if s != underscore => s },
                )
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
            let var_pat = select! { Token::Ident(s) if s != underscore => s }
                .map(cst::MatchPattern::Var);

            // Type-narrowing pattern: `name IS Type`
            let is_pat = select! { Token::Ident(s) if s != underscore => s }
                .then_ignore(Self::opt_newlines())
                .then_ignore(just(Token::Is))
                .then_ignore(Self::opt_newlines())
                .then(ty.clone())
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

    /// Build a match array pattern from parsed elements.
    ///
    /// The rest pattern (`..` or `...name`) must be the last element if present.
    pub(super) fn build_match_array_pattern(
        elems: Vec<MatchArrayPatElem>,
        span: Span,
    ) -> std::result::Result<cst::MatchPattern, ParseErr> {
        let mut pats = Vec::new();
        let mut rest: Option<cst::RestPattern> = None;

        elems.into_iter().try_for_each(|e| match e {
            MatchArrayPatElem::Pat(p) => {
                if rest.is_some() {
                    Err(chumsky::error::Simple::custom(
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
                    Err(chumsky::error::Simple::custom(
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
                    Err(chumsky::error::Simple::custom(
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

    /// Parse a match arm: `pattern => body` or `pattern IF guard => body`.
    pub(super) fn match_arm(
        interner: &mut StringInterner,
        expr: impl chumsky::Parser<Token, cst::Expr, Error = ParseErr>
            + Clone
            + 'static,
    ) -> impl chumsky::Parser<Token, cst::MatchArm, Error = ParseErr> + Clone
    {
        Self::match_pattern(interner)
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
}

/// Helper enum for array pattern elements during parsing.
#[derive(Clone)]
pub(super) enum ArrayPatElem {
    /// Regular pattern: `a`, `(x, y)`, etc.
    Pat(cst::BindingPattern),
    /// Rest ignore: `..`
    RestIgnore,
    /// Rest bind: `...name`
    RestBind(StringId),
}

/// Helper enum for match array pattern elements during parsing.
#[derive(Clone)]
pub(super) enum MatchArrayPatElem {
    /// Regular pattern: `a`, `(x, y)`, `Option.Some(x)`, etc.
    Pat(cst::MatchPattern),
    /// Rest ignore: `..`
    RestIgnore,
    /// Rest bind: `...name`
    RestBind(StringId),
}
