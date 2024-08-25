use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::complete::escaped;
use nom::character::complete::{self, digit1, none_of};
use nom::combinator::{cut, map, opt, recognize};
use nom::error::context;
use nom::sequence::{delimited, pair, tuple};
use nom::Parser as _;
use nom_supreme::tag;
use ordered_float::OrderedFloat;

use super::{Parse, ParsedResult, Span};
use crate::value::Scalar;

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
            "boolean literal",
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
            "character literal",
            delimited(
                complete::char('\''),
                complete::anychar,
                complete::char('\''),
            ),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for Cow<'a, str> {
    // FIXME Needs to be way more sophisticated
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        let esc = escaped(none_of("\\\""), '\\', complete::char('\"'));
        let or_empty = alt((esc, tag::complete::tag("")));
        context(
            "string literal",
            delimited(
                complete::char('\"'),
                map(or_empty, |s: Span| Cow::from(s.to_string())),
                complete::char('\"'),
            ),
        )
        .parse(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use crate::value::Ident;

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
}
