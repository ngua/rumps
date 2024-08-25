use std::borrow::Cow;

use nom::bytes::complete::take_while1;
use nom::combinator::map;
use nom::error::context;
use nom::{IResult, Parser as _};
use nom_locate::LocatedSpan;
use nom_supreme::error::ErrorTree;
use nom_supreme::final_parser::final_parser;

use crate::error::{FormattedError, ParseError};
use crate::value::Ident;

mod command;
mod string;
mod value;

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
