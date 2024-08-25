use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::complete::{escaped, take_while1};
use nom::character::complete::{self, digit1, none_of, space0};
use nom::combinator::{cut, map, opt, recognize};
use nom::error::context;
use nom::multi::separated_list0;
use nom::sequence::{delimited, pair, preceded, terminated, tuple};
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
            map(F::parse, Self::Float),
            map(i64::parse, Self::Int),
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

impl<'a> Parse<'a> for f64 {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context("float literal", |input| {
            let (rest, fstr): (_, Span) = recognize(tuple((
                opt(alt((complete::char('+'), complete::char('-')))),
                alt((
                    map(
                        tuple((digit1, pair(complete::char('.'), opt(digit1)))),
                        |_| (),
                    ),
                    map(tuple((complete::char('.'), digit1)), |_| ()),
                )),
                opt(tuple((
                    alt((complete::char('e'), complete::char('E'))),
                    opt(alt((complete::char('+'), complete::char('-')))),
                    cut(digit1),
                ))),
            )))
            .parse(input)?;

            // Should be safe to unwrap, since the span was just parsed as a
            // float
            let f = fstr.parse::<f64>().unwrap();

            Ok((rest, f))
        })
        .parse(input)
    }
}

impl<'a> Parse<'a> for OrderedFloat<f64> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        map(f64::parse, Self::from).parse(input)
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
        let scalar = parse::<Scalar<f64>>;

        let expected = Scalar::String(Cow::from("rumps is rumps"));
        let parsed = scalar(r#""rumps is rumps""#).unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Bool(true);
        let parsed = scalar("true").unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Char('r');
        let parsed = scalar("'r'").unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Int(1120);
        let parsed = scalar("1120").unwrap();
        assert_eq!(expected, parsed);

        let expected = Scalar::Float(1.12345);
        let parsed = scalar("1.12345").unwrap();
        assert_eq!(expected, parsed);
    }

    #[test]
    fn indicesp() {
        let expected = Indices {
            ident: Ident(Cow::from("rumps")),
            path: vec![
                Scalar::String(Cow::from("foo")),
                Scalar::String(Cow::from("g")),
                Scalar::Int(100),
            ],
        };

        let parsed = parse(r#"rumps("foo", "g", 100)"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps("foo" , "g" , 100)"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps("foo","g",100)"#).unwrap();
        assert_eq!(expected, parsed);

        let expected = Indices {
            ident: Ident(Cow::from("rumps")),
            path: vec![Scalar::Int(100), Scalar::Int(200), Scalar::Int(300)],
        };

        let parsed = parse(r#"rumps(100, 200, 300)"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps(100 , 200 , 300)"#).unwrap();
        assert_eq!(expected, parsed);

        let parsed = parse(r#"rumps(100,200,300)"#).unwrap();
        assert_eq!(expected, parsed);
    }
}
