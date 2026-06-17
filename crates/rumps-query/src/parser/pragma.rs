//! Pragma parsing.

use chumsky::prelude::{choice, filter, just, select};
use chumsky::Parser as _;
use smallvec::SmallVec;

use super::{ParseErr, Parser};
use crate::intern::{StringId, StringInterner};
use crate::parser::cst;
use crate::Token;

impl Parser {
    pub(super) fn pragma_stmt(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::Stmt, Error = ParseErr> + Clone {
        Self::pragma(interner).map_with_span(|p, span| {
            cst::Stmt::new(cst::StmtKind::Pragma(p), span)
        })
    }

    fn pragma(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::pragma::Kind, Error = ParseErr> + Clone
    {
        let options = interner.intern("options");
        let deriving = interner.intern("deriving");
        let transparent = interner.intern("transparent");
        let required = interner.intern("required");
        let default = interner.intern("default");
        let periodic = interner.intern("periodic");

        let name_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let names = Self::pragma_name()
            .separated_by(name_sep.clone())
            .at_least(1)
            .allow_trailing()
            .map(SmallVec::from_vec);
        let deriving_entries = Self::derived_class(interner)
            .separated_by(name_sep.clone())
            .at_least(1)
            .allow_trailing()
            .map(SmallVec::from_vec);

        let option_sep = just(Token::Comma).then_ignore(Self::opt_newlines());
        let options_prag = Self::ctx_ident(options)
            .ignore_then(Self::opt_newlines())
            .ignore_then(just(Token::Colon))
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::db_option(periodic)
                    .separated_by(option_sep)
                    .at_least(1)
                    .allow_trailing(),
            )
            .map(|opts| cst::pragma::Kind::Options(cst::pragma::Options(opts)));

        let deriving_prag = Self::ctx_ident(deriving)
            .ignore_then(Self::opt_newlines())
            .ignore_then(just(Token::Colon))
            .ignore_then(Self::opt_newlines())
            .ignore_then(deriving_entries)
            .map(|ds| cst::pragma::Kind::Deriving(cst::pragma::Deriving(ds)));

        let transparent_prag = Self::ctx_ident(transparent)
            .map_with_span(|_, span| cst::pragma::Kind::Transparent(span));

        let required_prag = Self::ctx_ident(required)
            .ignore_then(Self::opt_newlines())
            .ignore_then(just(Token::Colon))
            .ignore_then(Self::opt_newlines())
            .ignore_then(names)
            .map(|names| {
                cst::pragma::Kind::RequiredMethods(
                    cst::pragma::RequiredMethods(names),
                )
            });

        let default_prag =
            Self::ctx_ident(default).to(cst::pragma::Kind::DefaultDefinition);
        let unknown_prag = Self::unknown_pragma_name(
            options,
            deriving,
            transparent,
            required,
            default,
        )
        .then(filter(|t: &Token| *t != Token::RParen).repeated())
        .map(|(name, _)| cst::pragma::Kind::Unknown(name));

        just(Token::Hash)
            .ignore_then(just(Token::LParen))
            .ignore_then(Self::opt_newlines())
            .ignore_then(choice((
                options_prag,
                deriving_prag,
                transparent_prag,
                required_prag,
                default_prag,
                unknown_prag,
            )))
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RParen))
    }

    fn derived_class(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, cst::pragma::DerivedClass, Error = ParseErr>
           + Clone {
        let args = just(Token::LBracket)
            .ignore_then(Self::opt_newlines())
            .ignore_then(
                Self::type_expr(interner)
                    .separated_by(
                        just(Token::Comma).then_ignore(Self::opt_newlines()),
                    )
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::RBracket))
            .or_not()
            .map(|args| args.unwrap_or_default());

        select! { Token::Ident(tag) => tag }
            .then(args)
            .map_with_span(|(tag, args), span| cst::pragma::DerivedClass {
                tag,
                args: SmallVec::from_vec(args),
                span,
            })
    }

    fn pragma_name(
    ) -> impl chumsky::Parser<Token, cst::pragma::SpannedName, Error = ParseErr>
           + Clone {
        select! { Token::Ident(name) => name }
            .map_with_span(|name, span| cst::pragma::SpannedName { name, span })
    }

    fn unknown_pragma_name(
        options: StringId,
        deriving: StringId,
        transparent: StringId,
        required: StringId,
        default: StringId,
    ) -> impl chumsky::Parser<Token, cst::pragma::SpannedName, Error = ParseErr>
           + Clone {
        select! {
            Token::Ident(name)
                if name != options
                    && name != deriving
                    && name != transparent
                    && name != required
                    && name != default => name
        }
        .map_with_span(|name, span| cst::pragma::SpannedName { name, span })
    }

    fn pragma_value(
        periodic_id: StringId,
    ) -> impl chumsky::Parser<Token, cst::pragma::Value, Error = ParseErr> + Clone
    {
        let periodic = select! {
            Token::Ident(name) if name == periodic_id => name
        }
        .then(
            select! { Token::Int(v) => v }
                .map_with_span(|v, span| (v, span))
                .or_not(),
        )
        .map_with_span(|(_, interval_ms), span| {
            cst::pragma::Value::Periodic { interval_ms, span }
        });

        choice((
            periodic,
            Self::pragma_name().map(cst::pragma::Value::Ident),
            select! { Token::Int(v) => v }
                .map_with_span(cst::pragma::Value::Int),
            select! { Token::String(v) => v }
                .map_with_span(cst::pragma::Value::String),
        ))
    }

    fn db_option(
        periodic_id: StringId,
    ) -> impl chumsky::Parser<Token, cst::pragma::DbOption, Error = ParseErr> + Clone
    {
        Self::pragma_name()
            .then_ignore(Self::opt_newlines())
            .then_ignore(just(Token::Assign))
            .then_ignore(Self::opt_newlines())
            .then(Self::pragma_value(periodic_id))
            .map_with_span(|(name, value), span| cst::pragma::DbOption {
                name,
                value,
                span,
            })
    }
}
