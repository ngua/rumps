use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::complete::{escaped, take_while1};
use nom::character::complete::{self, none_of, space0};
use nom::combinator::{cut, map};
use nom::error::context;
use nom::multi::separated_list0;
use nom::sequence::{delimited, preceded, terminated};
use nom::{IResult, Parser as _};
use nom_locate::LocatedSpan;
use nom_supreme::error::ErrorTree;
use nom_supreme::final_parser::final_parser;
use nom_supreme::tag;
use ordered_float::OrderedFloat;

use crate::error::{FormattedError, ParseError};
use crate::value::{Ident, Indices, Scalar};

pub fn parse<'a, T>(input: &'a str) -> Result<T, FormattedError<'a>>
where
    T: Parse<'a> + Sized,
{
    final_parser::<_, _, ErrorTree<Span>, _>(T::parse)(Span::new(input))
        .map_err(|e| FormattedError::from((input, e)))
}

pub(crate) type Span<'a> = LocatedSpan<&'a str>;

pub(crate) type ParsedResult<'a, T, E = ParseError<'a>> =
    IResult<Span<'a>, T, E>;

pub trait Parse<'a>: Sized {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self>;
}

impl<'a, F> Parse<'a> for Scalar<'a, F>
where
    F: Parse<'a> + Sized,
{
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        alt((
            map(i64::parse, Self::Int),
            map(F::parse, Self::Float),
            map(char::parse, Self::Char),
            map(bool::parse, Self::Bool),
            map(Cow::parse, Self::String),
        ))
        .parse(input)
    }
}

impl<'a> Parse<'a> for Indices<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context("Array indices", |input| {
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

impl<'a> Parse<'a> for Ident<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "Identifier",
            map(take_while1(|c: char| c.is_alphanumeric()), |s: Span| {
                Ident::from(Cow::from(s.to_string()))
            }),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for i64 {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        nom::character::complete::i64.parse(input)
    }
}

impl<'a> Parse<'a> for f32 {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        nom::number::complete::float.parse(input)
    }
}

impl<'a> Parse<'a> for OrderedFloat<f32> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        map(nom::number::complete::float, Self::from).parse(input)
    }
}

impl<'a> Parse<'a> for bool {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "Boolean literal",
            alt((
                nom::combinator::value(true, tag::complete::tag("true")),
                nom::combinator::value(false, tag::complete::tag("false")),
            )),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for char {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "Character literal",
            delimited(
                tag::complete::tag("'"),
                nom::character::complete::anychar,
                tag::complete::tag("'"),
            ),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for Cow<'a, str> {
    // NOTE Does not really deal with string escaping
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        let esc = escaped(none_of("\\\""), '\\', tag::complete::tag("\""));
        let or_empty = alt((esc, tag::complete::tag("")));
        context(
            "String literal",
            delimited(
                tag::complete::tag("\""),
                map(or_empty, |s: Span| Cow::from(s.to_string())),
                tag::complete::tag("\""),
            ),
        )
        .parse(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identp() {
        let expected = Ident::from("rumps");
        let parsed = parse("rumps").unwrap();
        assert_eq!(expected, parsed);
    }

    #[test]
    fn scalarp() {
        let scalar = parse::<Scalar<f32>>;

        let expected = Scalar::String(Cow::from("rumps is rumps"));
        let parsed = scalar(r#""rumps is rumps""#).unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Bool(true);
        let parsed = scalar("true").unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Char('r');
        let parsed = scalar("'r'").unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Int(1);
        let parsed = scalar("1").unwrap();
        assert_eq!(expected, parsed);
    }

    #[test]
    fn indicesp() {
        let expected = Indices {
            ident: Ident(Cow::from("rumps")),
            path: vec![
                Scalar::String(Cow::from("f")),
                Scalar::String(Cow::from("g")),
            ],
        };

        let parsed = parse(r#"rumps("f", "g")"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps("f" , "g")"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps("f","g")"#).unwrap();
        assert_eq!(expected, parsed);
    }
}
