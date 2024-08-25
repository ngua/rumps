// Adapted from https://github.com/rust-bakery/nom/blob/main/examples/string.rs

use std::borrow::Cow;

use nom::branch::alt;
use nom::bytes::streaming::{is_not, take_while_m_n};
use nom::character::complete;
use nom::character::streaming::multispace1;
use nom::combinator::{map, map_opt, map_res, value, verify};
use nom::multi::fold_many0;
use nom::sequence::{delimited, preceded};
use nom::Parser as _;

use super::{Parse, ParsedResult, Span};

impl<'a> Parse<'a> for Cow<'a, str> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        map(String::parse, Cow::from).parse(input)
    }
}

impl<'a> Parse<'a> for String {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        let strp =
            fold_many0(Fragment::parse, String::new, |mut string, fragment| {
                match fragment {
                    Fragment::Literal(s) => string.push_str(&s),
                    Fragment::CharEsc(c) => string.push(c),
                    Fragment::WsEsc => {}
                }
                string
            });

        delimited(complete::char('"'), strp, complete::char('"')).parse(input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Fragment<'a> {
    Literal(&'a str),
    CharEsc(char),
    WsEsc,
}

impl<'a> Parse<'a> for Fragment<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        let litp = |input| {
            verify(is_not("\"\\"), |s: &Span| !s.fragment().is_empty())
                .parse(input)
        };

        let escapedp = |input| {
            let unicodep = |input| {
                let u32p = map_res(
                    preceded(
                        complete::char('u'),
                        delimited(
                            complete::char('{'),
                            take_while_m_n(1, 6, |c: char| {
                                c.is_ascii_hexdigit()
                            }),
                            complete::char('}'),
                        ),
                    ),
                    move |s: Span| u32::from_str_radix(s.fragment(), 16),
                );

                map_opt(u32p, std::char::from_u32).parse(input)
            };

            preceded(
                complete::char('\\'),
                alt((
                    unicodep,
                    value('\n', complete::char('n')),
                    value('\r', complete::char('r')),
                    value('\t', complete::char('t')),
                    value('\u{08}', complete::char('b')),
                    value('\u{0C}', complete::char('f')),
                    value('\\', complete::char('\\')),
                    value('/', complete::char('/')),
                    value('"', complete::char('"')),
                )),
            )
            .parse(input)
        };

        alt((
            map(litp, |s: Span| Fragment::Literal(s.fragment())),
            map(escapedp, Fragment::CharEsc),
            value(Fragment::WsEsc, preceded(complete::char('\\'), multispace1)),
        ))
        .parse(input)
    }
}
