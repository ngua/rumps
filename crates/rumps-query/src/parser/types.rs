//! Type expression parsing for RUMPS.

use chumsky::prelude::{choice, just, recursive, select};
use chumsky::Parser as _;
use smallvec::SmallVec;

use super::{ParseErr, Parser};
use crate::parser::cst;
use crate::{Span, Token};

impl Parser {
    /// Parse a type expression.
    pub(super) fn type_expr(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        recursive(|ty| {
            // Type parameters: `[T]` or `[T, E]`
            let type_params = ty
                .clone()
                .separated_by(just(Token::Comma))
                .at_least(1)
                .delimited_by(just(Token::LBracket), just(Token::RBracket));

            // Wildcard: `_` (represents "any type" in type argument position)
            let wildcard = select! { Token::Ident(s) if s == "_" => () }
                .map_with_span(|(), span| {
                    TypeAtomOrParams::Single(cst::TypeExpr::new(
                        cst::TypeExprKind::Wildcard,
                        span,
                    ))
                });

            // Unqualified associated type: `:Index` (resolved from class context)
            let unqualified_assoc = just(Token::Colon)
                .ignore_then(Self::ident())
                .map_with_span(|name, span| {
                    TypeAtomOrParams::Single(cst::TypeExpr::new(
                        cst::TypeExprKind::AssocType { class: None, name },
                        span,
                    ))
                });

            // Qualified associated type: `Indexable:Index` (names the class)
            // NOTE: Uses `ColonNoSpace` because the lexer converts `:` to that
            // when adjacent to an uppercase ident (class name syntax).
            let qualified_assoc = Self::ident()
                .then_ignore(just(Token::ColonNoSpace))
                .then(Self::ident())
                .map_with_span(|(class, name), span| {
                    TypeAtomOrParams::Single(cst::TypeExpr::new(
                        cst::TypeExprKind::AssocType {
                            class: Some(class),
                            name,
                        },
                        span,
                    ))
                });

            // Named type (possibly qualified: `Module.Type`) with optional type params
            let named = Self::ident()
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

            // Atom: wildcard, associated types, or named type
            // Order: try qualified_assoc before named to match `Class:Assoc`
            let atom =
                wildcard.or(unqualified_assoc).or(qualified_assoc).or(named);

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
    /// Supports qualified names like `Module.Type` and wildcards (`_`).
    pub(super) fn type_expr_atom(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        // Type parameters: `[T]` or `[T, E]`
        let type_params = Self::type_expr()
            .separated_by(just(Token::Comma))
            .at_least(1)
            .delimited_by(just(Token::LBracket), just(Token::RBracket));

        // Wildcard: `_`
        let wildcard = select! { Token::Ident(s) if s == "_" => () }
            .map_with_span(|(), span| {
                cst::TypeExpr::new(cst::TypeExprKind::Wildcard, span)
            });

        // Named type: `Int`, `Module.Type`, `Option[T]`, etc.
        let named = Self::ident()
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
            });

        wildcard.or(named)
    }

    /// Simplified type expression parser for use in type patterns.
    ///
    /// Supports named types with up to 6 levels of nested type application
    /// (e.g., `Array[Option[Map[_, Result[_, _]]]]`), and tuple types.
    ///
    /// This is intentionally non-recursive (manually expanded) to avoid stack
    /// overflow when combined with the expression parser's recursive structure.
    /// Types nested deeper than 6 levels will produce a parse error.
    pub(super) fn simple_type_expr(
    ) -> impl chumsky::Parser<Token, cst::TypeExpr, Error = ParseErr> + Clone
    {
        // Wildcard: `_`
        let wildcard = select! { Token::Ident(s) if s == "_" => () }
            .map_with_span(|(), span| {
                cst::TypeExpr::new(cst::TypeExprKind::Wildcard, span)
            });

        // Helper to build a level: wildcard or named with optional params
        macro_rules! level {
            ($inner:expr) => {{
                let params = $inner
                    .clone()
                    .separated_by(just(Token::Comma))
                    .at_least(1)
                    .delimited_by(just(Token::LBracket), just(Token::RBracket));
                let named = Self::ident().then(params.or_not()).map_with_span(
                    |(name, params), span| {
                        let kind = match params {
                            None => cst::TypeExprKind::Named(name),
                            Some(ps) => cst::TypeExprKind::App(name, ps),
                        };
                        cst::TypeExpr::new(kind, span)
                    },
                );
                wildcard.or(named)
            }};
        }

        // Level 0: leaf (wildcard or simple named, no params)
        let named_leaf = Self::ident().map_with_span(|name, span| {
            cst::TypeExpr::new(cst::TypeExprKind::Named(name), span)
        });
        let level0 = wildcard.or(named_leaf);

        // Levels 1-5: each can have params from the previous level
        let level1 = level!(level0);
        let level2 = level!(level1);
        let level3 = level!(level2);
        let level4 = level!(level3);
        let level5 = level!(level4);

        // Level 6 (top): can have level5 params
        let top_level = level!(level5);

        // Tuple types: `()`, `(T,)`, `(T, U, ...)`
        // Uses level5 for elements (allows 5 levels of nesting in tuple elements)
        let sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let elem_comma = level5.clone().then_ignore(sep);
        let tuple = just(Token::LParen)
            .ignore_then(Self::opt_newlines())
            .ignore_then(elem_comma.repeated().then(level5.or_not()))
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
                        chumsky::error::Simple::custom(
                            span,
                            "internal: expected type",
                        )
                    })
                } else {
                    // `()`, `(T,)`, or `(T, U, ...)` are tuples
                    Ok(cst::TypeExpr::new(
                        cst::TypeExprKind::Tuple(elems),
                        span,
                    ))
                }
            });

        tuple.or(top_level)
    }

    /// Parse a type pattern for the `is` operator.
    pub(super) fn type_pattern(
    ) -> impl chumsky::Parser<Token, cst::TypePattern, Error = ParseErr> + Clone
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
                None => cst::TypePattern::Variant(ty, var),
                Some(PatternArgs::Wildcard) => {
                    cst::TypePattern::VariantWildcard(ty, var)
                }
                Some(PatternArgs::Bindings(names)) => {
                    cst::TypePattern::VariantBind(ty, var, names)
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
            .map(cst::TypePattern::Object);

        // Simple type pattern: `Int`, `Array[String]`, `Map[Int, String]`
        let simple_type = Self::simple_type_expr().map(cst::TypePattern::Type);

        variant_pattern.or(struct_pat).or(simple_type)
    }

    /// Parse a user-facing constraint name.
    ///
    /// HKT classes (`Iterable`, `Fallible`, etc.) reject type arguments;
    /// the element type is specified at usage sites (`F[T]`).
    /// Multi-param classes (`Into[T]`, `TryInto[T]`, `Indexable[E]`) require them.
    pub(super) fn constraint(
    ) -> impl chumsky::Parser<Token, cst::Class, Error = ParseErr> + Clone {
        let type_args = just(Token::LBracket)
            .ignore_then(
                Self::type_expr_atom()
                    .separated_by(just(Token::Comma))
                    .at_least(1),
            )
            .then_ignore(just(Token::RBracket));

        select! { Token::Ident(s) => s }
            .then(type_args.or_not())
            .try_map(|(name, args), span| {
                let has_args = args.is_some();
                let mut args = args.unwrap_or_default().into_iter();

                match name.as_str() {
                    "Iterable" if has_args => Err(chumsky::error::Simple::custom(
                        span,
                        "`Iterable` is higher-kinded; use `C: Iterable` \
                         and `C[T]` in type position, not `C: Iterable[T]`",
                    )),
                    "Iterable" => Ok(cst::Class::Iterable(None)),

                    "Fallible" if has_args => Err(chumsky::error::Simple::custom(
                        span,
                        "`Fallible` is higher-kinded; use `F: Fallible` \
                         and `F[T]` in type position, not `F: Fallible[T]`",
                    )),
                    "Fallible" => Ok(cst::Class::Fallible(None)),

                    "Mappable" if has_args => Err(chumsky::error::Simple::custom(
                        span,
                        "`Mappable` is higher-kinded; use `M: Mappable` \
                         and `M[T]` in type position, not `M: Mappable[T]`",
                    )),
                    "Mappable" => Ok(cst::Class::Mappable(None)),

                    "Foldable" if has_args => Err(chumsky::error::Simple::custom(
                        span,
                        "`Foldable` is higher-kinded; use `F: Foldable` \
                         and `F[T]` in type position, not `F: Foldable[T]`",
                    )),
                    "Foldable" => Ok(cst::Class::Foldable(None)),

                    "Filterable" if has_args => Err(chumsky::error::Simple::custom(
                        span,
                        "`Filterable` is higher-kinded; use `F: Filterable` \
                         and `F[T]` in type position, not `F: Filterable[T]`",
                    )),
                    "Filterable" => Ok(cst::Class::Filterable(None)),

                    "Into" => args.next().map_or_else(
                        || Err(chumsky::error::Simple::custom(
                            span,
                            "`Into` requires a target type: `Into[T]`",
                        )),
                        |ty| Ok(cst::Class::Into(ty)),
                    ),
                    "TryInto" => args.next().map_or_else(
                        || Err(chumsky::error::Simple::custom(
                            span,
                            "`TryInto` requires a target type: `TryInto[T]`",
                        )),
                        |ty| Ok(cst::Class::TryInto(ty)),
                    ),
                    "Indexable" => args.next().map_or_else(
                        || Err(chumsky::error::Simple::custom(
                            span,
                            "`Indexable` requires an element type: `Indexable[E]`",
                        )),
                        |ty| Ok(cst::Class::Indexable(ty)),
                    ),

                    "Numeric" => Ok(cst::Class::Numeric),
                    "Monoid" => Ok(cst::Class::Monoid),
                    "BitLike" => Ok(cst::Class::BitLike),
                    "Negatable" => Ok(cst::Class::Negatable),
                    "Ord" => Ok(cst::Class::Ord),
                    "Display" => Ok(cst::Class::Display),

                    _ => Err(chumsky::error::Simple::custom(
                        span,
                        format!(
                            "unknown class `{name}`; valid classes are: \
                             Numeric, Negatable, Iterable, Monoid, BitLike, \
                             Fallible, Into[T], TryInto[T], Indexable[E], \
                             Ord, Mappable, Foldable, Filterable, Display"
                        ),
                    )),
                }
            })
    }

    /// Parse a type parameter with optional constraints: `T` or `T: C1 + C2`.
    pub(super) fn type_param(
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
    pub(super) fn type_params(
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

    /// Build a function type, tuple type, or standalone type from parsed components.
    pub(super) fn build_fn_type(
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
                    chumsky::error::Simple::custom(
                        span,
                        "internal: expected single type",
                    )
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
pub(super) enum TypeAtomOrParams {
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
