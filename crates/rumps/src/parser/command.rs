use nom::branch::alt;
use nom::character::complete::{self, space0};
use nom::combinator::{cut, map};
use nom::error::context;
use nom::multi::separated_list0;
use nom::sequence::{preceded, terminated};
use nom::Parser as _;

use super::{Parse, ParsedResult, Span};
use crate::command::{Indices, Var};
use crate::value::{Ident, Scalar};

impl<'a> Parse<'a> for Var<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "variable",
            alt((
                map(Indices::parse, Self::Indexed),
                map(Ident::parse, Self::Simple),
                map(
                    |input| {
                        context(
                            "reference",
                            preceded(complete::char('@'), Ident::parse),
                        )
                        .parse(input)
                    },
                    Self::Ref,
                ),
            )),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for Indices<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context("indexed", |input| {
            let (input, ident) = Ident::parse(input)?;
            let (input, path) = preceded(
                complete::char('('),
                cut(terminated(
                    separated_list0(
                        preceded(space0, preceded(complete::char(','), space0)),
                        Scalar::parse,
                    ),
                    preceded(space0, complete::char(')')),
                )),
            )
            .parse(input)?;
            Ok((input, Self { ident, path }))
        })
        .parse(input)
    }
}

#[cfg(test)]
mod test {

    mod var {
        use std::borrow::Cow;

        use ordered_float::OrderedFloat;

        use crate::command::{Indices, Var};
        use crate::parser::parse;
        use crate::value::{Ident, Scalar};

        #[test]
        fn refp() {
            let expected = Var::Ref(Ident::from("rumps"));

            let parsed = parse("@rumps").unwrap();
            assert_eq!(expected, parsed);
        }

        #[test]
        fn simplep() {
            let expected = Var::Simple(Ident::from("rumps"));

            let parsed = parse("rumps").unwrap();
            assert_eq!(expected, parsed);
        }

        #[test]
        fn indexedp() {
            let expected = Var::Indexed(Indices {
                ident: Ident::from("rumps"),
                path: vec![
                    Scalar::String(Cow::from("foo")),
                    Scalar::String(Cow::from("g")),
                    Scalar::Int(100),
                ],
            });

            let parsed = parse(r#"rumps("foo", "g", 100)"#).unwrap();
            assert_eq!(expected, parsed);

            let parsed = parse(r#"rumps("foo" , "g" , 100)"#).unwrap();
            assert_eq!(expected, parsed);

            let parsed = parse(r#"rumps("foo","g",100)"#).unwrap();
            assert_eq!(expected, parsed);

            let expected = Var::Indexed(Indices {
                ident: Ident::from("rumps"),
                path: vec![
                    Scalar::Int(100),
                    Scalar::Int(200),
                    Scalar::Int(300),
                ],
            });

            let parsed = parse(r#"rumps(100, 200, 300)"#).unwrap();
            assert_eq!(expected, parsed);

            let parsed = parse(r#"rumps(100 , 200 , 300)"#).unwrap();
            assert_eq!(expected, parsed);

            let parsed = parse(r#"rumps(100,200,300)"#).unwrap();
            assert_eq!(expected, parsed);

            let expected = Var::Indexed(Indices {
                ident: Ident::from("rumps"),
                path: vec![Scalar::Float(OrderedFloat::from(1.1))],
            });

            let parsed = parse(r#"rumps(1.1)"#).unwrap();
            assert_eq!(expected, parsed);
        }
    }
}
