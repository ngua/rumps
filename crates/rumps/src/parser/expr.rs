use std::borrow::Cow;

use nom::branch::alt;
use nom::character::complete::{self, digit1, multispace0, space0};
use nom::combinator::{cut, map, opt, recognize, value};
use nom::error::context;
use nom::multi::separated_list0;
use nom::sequence::{delimited, pair, preceded, terminated, tuple};
use nom::Parser as _;
use nom_supreme::tag;
use ordered_float::OrderedFloat;

use super::{Parse, ParsedResult, Span};
use crate::expr::{BinOp, Expr, Ident, Indices, Literal, Value, Var};

impl<'a> Parse<'a> for Expr<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "expression",
            alt((
                map(Value::parse, Self::Value),
                map(
                    preceded(
                        terminated(Value::parse, multispace0),
                        |_| todo!(),
                    ),
                    |(lhs, op, rhs)| Self::BinOp(lhs, op, rhs),
                ),
            )),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for BinOp {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context(
            "binary operator",
            alt((
                value(
                    Self::Eq,
                    tuple((complete::char('='), complete::char('='))),
                ),
                value(
                    Self::Pow,
                    tuple((complete::char('*'), complete::char('*'))),
                ),
                value(
                    Self::Gte,
                    tuple((complete::char('>'), complete::char('='))),
                ),
                value(
                    Self::Lte,
                    tuple((complete::char('<'), complete::char('='))),
                ),
                value(
                    Self::Xor,
                    tuple((complete::char('|'), complete::char('|'))),
                ),
                value(Self::Gt, complete::char('>')),
                value(Self::Lt, complete::char('<')),
                value(Self::Add, complete::char('+')),
                value(Self::Sub, complete::char('-')),
                value(Self::Mul, complete::char('*')),
                value(Self::Div, complete::char('/')),
                value(Self::Mod, complete::char('#')),
                value(Self::Concat, complete::char('~')),
                value(Self::And, complete::char('&')),
                value(Self::Or, complete::char('|')),
            )),
        )
        .parse(input)
    }
}

impl<'a> Parse<'a> for Value<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        alt((
            map(Literal::parse, Self::Literal),
            map(Var::parse, Self::Var),
        ))
        .parse(input)
    }
}

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

// FIXME Add support for indexing by variable
impl<'a> Parse<'a> for Indices<'a> {
    fn parse(input: Span<'a>) -> ParsedResult<'a, Self> {
        context("indexed", |input| {
            let (input, ident) = Ident::parse(input)?;
            let (input, path) = preceded(
                complete::char('('),
                cut(terminated(
                    separated_list0(
                        preceded(space0, preceded(complete::char(','), space0)),
                        Literal::parse,
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

impl<'a, F> Parse<'a> for Literal<'a, F>
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
            let f = fstr.parse::<Self>().unwrap();

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::Ident;
    use crate::parser::parse;

    #[test]
    fn identp() {
        let expected = Ident::from("rumps");
        let parsed = parse("rumps").unwrap();
        assert_eq!(expected, parsed);
    }

    #[test]
    fn litp() {
        let val = parse::<Literal<f64>>;

        let expected = Literal::String(Cow::from("한글 rumps is rumps\n"));
        let parsed = val(r#""한글 rumps is rumps\n""#).unwrap();
        assert_eq!(expected, parsed);

        let expected = Literal::Bool(true);
        let parsed = val("true").unwrap();
        assert_eq!(expected, parsed);

        let expected = Literal::Char('r');
        let parsed = val("'r'").unwrap();
        assert_eq!(expected, parsed);

        let expected = Literal::Int(1120);
        let parsed = val("1120").unwrap();
        assert_eq!(expected, parsed);

        let expected = Literal::Float(1.12345);
        let parsed = val("1.12345").unwrap();
        assert_eq!(expected, parsed);
    }

    mod var {
        use std::borrow::Cow;

        use ordered_float::OrderedFloat;

        use crate::expr::{Ident, Indices, Literal, Var};
        use crate::parser::parse;

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
                    Literal::String(Cow::from("foo")),
                    Literal::String(Cow::from("g")),
                    Literal::Int(100),
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
                    Literal::Int(100),
                    Literal::Int(200),
                    Literal::Int(300),
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
                path: vec![Literal::Float(OrderedFloat::from(1.1))],
            });

            let parsed = parse(r#"rumps(1.1)"#).unwrap();
            assert_eq!(expected, parsed);
        }
    }
}
