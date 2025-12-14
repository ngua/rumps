//! Lexer for the RUMPS query language.
//!
//! Converts source text into a stream of tokens with spans. Handles:
//! - Train-case identifiers (`my-var`) vs spaced subtraction (`a - b`)
//! - Case-insensitive keywords
//! - Comments (`;` to end of line)
//! - Indentation tracking (`Indent`/`Dedent` tokens)
//! - String literals with escape sequences

#![allow(dead_code)]

use std::ops::Range;

use chumsky::prelude::*;

use crate::{Error, Span, Token};

/// A token paired with its source span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned(pub Token, pub Span);

impl Spanned {
    fn new(tok: Token, span: Span) -> Self {
        Self(tok, span)
    }

    fn from_range(tok: Token, range: Range<usize>) -> Self {
        Self(tok, Span::from(range))
    }

    /// Post-processes tokens to add `Indent` and `Dedent` tokens.
    fn process_indentation(tokens: Vec<Self>) -> Vec<Self> {
        let with_cols = TokenWithCol::from_spanned(&tokens);
        let mut result = Vec::with_capacity(tokens.len());
        let mut indent_stack: Vec<usize> = vec![0];

        Self::process_indent_loop(
            &with_cols,
            0,
            &mut result,
            &mut indent_stack,
        );
        Self::emit_final_dedents(&mut result, &indent_stack);

        result
    }

    fn process_indent_loop(
        tokens: &[TokenWithCol],
        idx: usize,
        result: &mut Vec<Self>,
        indent_stack: &mut Vec<usize>,
    ) {
        tokens.get(idx).map(|t| match &t.tok {
            Token::Newline => {
                result.push(Self::new(t.tok.clone(), t.span));

                let (next_idx, indent) =
                    TokenWithCol::measure_indent(tokens, idx + 1);
                let current = indent_stack.last().copied().unwrap_or(0);

                (indent > current)
                    .then(|| {
                        indent_stack.push(indent);
                        result.push(Self::new(Token::Indent, t.span));
                    })
                    .or_else(|| {
                        Self::emit_dedents_to(
                            result,
                            indent_stack,
                            indent,
                            t.span,
                        );
                        Some(())
                    });

                Self::process_indent_loop(
                    tokens,
                    next_idx,
                    result,
                    indent_stack,
                );
            }
            Token::Eof => {
                result.push(Self::new(t.tok.clone(), t.span));
            }
            _ => {
                result.push(Self::new(t.tok.clone(), t.span));
                Self::process_indent_loop(
                    tokens,
                    idx + 1,
                    result,
                    indent_stack,
                );
            }
        });
    }

    /// Emits `Dedent` tokens to return to target indent level.
    fn emit_dedents_to(
        result: &mut Vec<Self>,
        indent_stack: &mut Vec<usize>,
        target: usize,
        span: Span,
    ) {
        let should_dedent = indent_stack
            .last()
            .map(|&level| level > target)
            .unwrap_or(false);

        should_dedent.then(|| {
            indent_stack.pop();
            result.push(Self::new(Token::Dedent, span));
            Self::emit_dedents_to(result, indent_stack, target, span);
        });
    }

    /// Emits final dedents at EOF.
    fn emit_final_dedents(result: &mut Vec<Self>, indent_stack: &[usize]) {
        let eof_span = result
            .iter()
            .rev()
            .find_map(|Self(t, s)| matches!(t, Token::Eof).then_some(*s))
            .unwrap_or_default();

        let eof = result.pop();

        (1..indent_stack.len()).for_each(|_| {
            result.push(Self::new(Token::Dedent, eof_span));
        });

        eof.map(|e| result.push(e));
    }
}

/// Lexer for RUMPS source code.
pub(crate) struct Lexer<'a> {
    src: &'a str,
}

impl<'a> Lexer<'a> {
    /// Creates a new lexer for the given source.
    pub(crate) fn new(src: &'a str) -> Self {
        Self { src }
    }

    /// Lexes source code into a stream of spanned tokens.
    ///
    /// Returns either a vector of tokens or a vector of lex errors.
    pub(crate) fn lex(self) -> Result<Vec<Spanned>, Vec<Error>> {
        Self::lexer()
            .parse(self.src)
            .map(Spanned::process_indentation)
            .map_err(|errs| errs.into_iter().map(Self::to_error).collect())
    }
}

/// Chumsky error type for char-based lexing.
///
/// The AST parser will use `Simple<Token>` instead (different input type).
type LexErr = Simple<char>;

// Uses `Lexer<'_>` as namespace for internal combinators. Put any new
// combinators into this private block
impl Lexer<'_> {
    /// Converts a chumsky error to our `Error` type.
    fn to_error(e: LexErr) -> Error {
        let span = Span::from(e.span());
        let msg = e
            .found()
            .map(|c| format!("unexpected character `{c}`"))
            .unwrap_or_else(|| "unexpected end of input".into());
        Error::lex(span, msg)
    }

    /// Main lexer combinator; parses all tokens from source.
    fn lexer() -> impl Parser<char, Vec<Spanned>, Error = LexErr> {
        let tok = Self::token().map(Some);
        let comment = Self::comment().to(None);
        let newline = Self::newline().map(Some);
        let ws = Self::horizontal_ws().to(None);

        choice((comment, newline, tok, ws))
            .repeated()
            .then_ignore(end())
            .map_with_span(|opts, span: Range<usize>| {
                let eof_pos = span.end as u32;
                opts.into_iter()
                    .flatten()
                    .chain(std::iter::once(Spanned::new(
                        Token::Eof,
                        Span::new(eof_pos, eof_pos),
                    )))
                    .collect()
            })
    }

    /// Horizontal whitespace (space, tab, CR); not newlines.
    fn horizontal_ws() -> impl Parser<char, (), Error = LexErr> + Clone {
        filter(|c: &char| *c == ' ' || *c == '\t' || *c == '\r')
            .ignored()
            .repeated()
            .at_least(1)
            .ignored()
    }

    /// Comment: `;` to end of line.
    fn comment() -> impl Parser<char, (), Error = LexErr> + Clone {
        just(';')
            .then(take_until(just('\n').rewind().ignored().or(end())))
            .ignored()
    }

    /// Newline token.
    fn newline() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        just('\n')
            .map_with_span(|_, span| Spanned::from_range(Token::Newline, span))
    }

    /// A single token (not whitespace, not comment, not newline).
    fn token() -> impl Parser<char, Spanned, Error = LexErr> {
        choice((
            Self::string_lit(),
            Self::number(),
            Self::global(),
            Self::ident_or_keyword(),
            Self::operator_or_punct(),
        ))
    }

    fn string_lit() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        let escape = just('\\').ignore_then(choice((
            just('n').to('\n'),
            just('r').to('\r'),
            just('t').to('\t'),
            just('\\').to('\\'),
            just('"').to('"'),
            just('0').to('\0'),
        )));

        let char_in_string =
            escape.or(filter(|c: &char| *c != '"' && *c != '\\' && *c != '\n'));

        just('"')
            .ignore_then(char_in_string.repeated())
            .then_ignore(just('"'))
            .collect::<String>()
            .map_with_span(|s, span| {
                Spanned::from_range(Token::String(s), span)
            })
    }

    fn number() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        let digits = filter(|c: &char| c.is_ascii_digit())
            .repeated()
            .at_least(1)
            .collect::<String>();

        let int_part = digits;

        let frac_part = just('.').then(digits).map(|(_, frac)| frac);

        let exp_part = just('e')
            .or(just('E'))
            .then(just('-').or(just('+')).or_not())
            .then(digits)
            .map(|((_, sign), exp)| {
                let s = sign.unwrap_or('+');
                format!("e{s}{exp}")
            });

        int_part
            .then(frac_part.or_not())
            .then(exp_part.or_not())
            .map_with_span(|((int, frac), exp), span: Range<usize>| {
                let has_frac = frac.is_some();
                let has_exp = exp.is_some();

                if has_frac || has_exp {
                    let mut s = int;
                    frac.map(|f| {
                        s.push('.');
                        s.push_str(&f);
                    });
                    exp.map(|e| s.push_str(&e));
                    s.parse::<f64>()
                        .map(|f| {
                            Spanned::from_range(Token::Float(f), span.clone())
                        })
                        .unwrap_or_else(|_| {
                            Spanned::from_range(Token::Float(0.0), span)
                        })
                } else {
                    int.parse::<i64>()
                        .map(|n| {
                            Spanned::from_range(Token::Int(n), span.clone())
                        })
                        .unwrap_or_else(|_| {
                            // Overflow; treat as float
                            int.parse::<f64>()
                                .map(|f| {
                                    Spanned::from_range(
                                        Token::Float(f),
                                        span.clone(),
                                    )
                                })
                                .unwrap_or_else(|_| {
                                    Spanned::from_range(Token::Int(0), span)
                                })
                        })
                }
            })
    }

    fn global() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        just('^').ignore_then(Self::ident_chars()).map_with_span(
            |name, span| Spanned::from_range(Token::Global(name), span),
        )
    }

    /// Identifier start: alphabetic or `_`.
    fn ident_start() -> impl Parser<char, char, Error = LexErr> + Clone {
        filter(|c: &char| c.is_alphabetic() || *c == '_')
    }

    /// Identifier continuation: alphanumeric or `_`.
    fn ident_cont() -> impl Parser<char, char, Error = LexErr> + Clone {
        filter(|c: &char| c.is_alphanumeric() || *c == '_')
    }

    /// Identifier characters including train-case handling.
    ///
    /// Train-case: `-` followed immediately by a letter continues the identifier.
    fn ident_chars() -> impl Parser<char, String, Error = LexErr> + Clone {
        let base = Self::ident_start().then(Self::ident_cont().repeated()).map(
            |(first, rest)| {
                let mut s = String::with_capacity(1 + rest.len());
                s.push(first);
                rest.into_iter().for_each(|c| s.push(c));
                s
            },
        );

        // Train-case continuation: `-` immediately followed by alphabetic
        let train_cont = just('-')
            .then(filter(|c: &char| c.is_alphabetic()))
            .then(Self::ident_cont().repeated())
            .map(|((hyphen, first), rest)| {
                let mut s = String::with_capacity(2 + rest.len());
                s.push(hyphen);
                s.push(first);
                rest.into_iter().for_each(|c| s.push(c));
                s
            });

        base.then(train_cont.repeated()).map(|(mut base, conts)| {
            conts.into_iter().for_each(|c| base.push_str(&c));
            base
        })
    }

    fn ident_or_keyword() -> impl Parser<char, Spanned, Error = LexErr> + Clone
    {
        Self::ident_chars().map_with_span(|ident, span| {
            Token::keyword(&ident)
                .map(|kw| Spanned::from_range(kw, span.clone()))
                .unwrap_or_else(|| {
                    Spanned::from_range(Token::Ident(ident), span)
                })
        })
    }

    fn operator_or_punct() -> impl Parser<char, Spanned, Error = LexErr> {
        // Split into groups to avoid tuple size limits
        let two_char = choice((
            just("++").to(Token::Concat),
            just("//").to(Token::FloorDiv),
            just("==").to(Token::Eq),
            just("!=").to(Token::Ne),
            just("<=").to(Token::Le),
            just(">=").to(Token::Ge),
            just("&&").to(Token::AmpAmp),
            just("||").to(Token::PipePipe),
            just("..").to(Token::DotDot),
        ));

        let one_char_ops = choice((
            just('+').to(Token::Plus),
            just('-').to(Token::Minus),
            just('*').to(Token::Mul),
            just('/').to(Token::Div),
            just('%').to(Token::Modulo),
            just('=').to(Token::Assign),
            just('!').to(Token::Bang),
            just('<').to(Token::Lt),
            just('>').to(Token::Gt),
            just('.').to(Token::Dot),
        ));

        let punct = choice((
            just('(').to(Token::LParen),
            just(')').to(Token::RParen),
            just('{').to(Token::LBrace),
            just('}').to(Token::RBrace),
            just('[').to(Token::LBracket),
            just(']').to(Token::RBracket),
            just(',').to(Token::Comma),
            just(':').to(Token::Colon),
        ));

        // Two-char ops first for longest match
        two_char
            .or(one_char_ops)
            .or(punct)
            .map_with_span(Spanned::from_range)
    }
}

/// Token with its column position (bytes from start of line).
struct TokenWithCol {
    tok: Token,
    span: Span,
    col: usize,
}

impl TokenWithCol {
    /// Computes column positions for each token.
    fn from_spanned(tokens: &[Spanned]) -> Vec<Self> {
        let mut result = Vec::with_capacity(tokens.len());
        let mut line_start: u32 = 0;

        tokens.iter().for_each(|Spanned(tok, span)| {
            let col = (span.start - line_start) as usize;
            result.push(Self {
                tok: tok.clone(),
                span: *span,
                col,
            });
            (tok == &Token::Newline).then(|| line_start = span.end);
        });

        result
    }

    /// Measures indentation level of the next significant token.
    fn measure_indent(tokens: &[Self], idx: usize) -> (usize, usize) {
        tokens.get(idx).map_or((idx, 0), |t| match &t.tok {
            Token::Newline => Self::measure_indent(tokens, idx + 1),
            Token::Eof => (idx, 0),
            _ => (idx, t.col),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_ok(src: &str) -> Vec<Token> {
        Lexer::new(src)
            .lex()
            .expect("lex should succeed")
            .into_iter()
            .map(|Spanned(t, _)| t)
            .collect()
    }

    fn lex_err(src: &str) -> Vec<Error> {
        Lexer::new(src).lex().expect_err("lex should fail")
    }

    fn lex_spanned(src: &str) -> Vec<Spanned> {
        Lexer::new(src).lex().expect("should lex")
    }

    #[test]
    fn empty_source() {
        let tokens = lex_ok("");
        assert_eq!(tokens, vec![Token::Eof]);
    }

    #[test]
    fn simple_integer() {
        let tokens = lex_ok("42");
        assert_eq!(tokens, vec![Token::Int(42), Token::Eof]);
    }

    #[test]
    fn simple_float() {
        let tokens = lex_ok("3.14");
        assert_eq!(tokens, vec![Token::Float(3.14), Token::Eof]);
    }

    #[test]
    fn float_with_exponent() {
        let tokens = lex_ok("1e10");
        assert_eq!(tokens, vec![Token::Float(1e10), Token::Eof]);

        let tokens = lex_ok("2.5e-3");
        assert_eq!(tokens, vec![Token::Float(2.5e-3), Token::Eof]);
    }

    #[test]
    fn simple_string() {
        let tokens = lex_ok(r#""hello""#);
        assert_eq!(tokens, vec![Token::String("hello".into()), Token::Eof]);
    }

    #[test]
    fn string_with_escapes() {
        let tokens = lex_ok(r#""line1\nline2\ttab\\slash""#);
        assert_eq!(
            tokens,
            vec![Token::String("line1\nline2\ttab\\slash".into()), Token::Eof]
        );
    }

    #[test]
    fn unterminated_string() {
        let errs = lex_err(r#""unterminated"#);
        assert!(!errs.is_empty());
    }

    #[test]
    fn simple_identifier() {
        let tokens = lex_ok("foo");
        assert_eq!(tokens, vec![Token::Ident("foo".into()), Token::Eof]);
    }

    #[test]
    fn train_case_identifier() {
        let tokens = lex_ok("my-var");
        assert_eq!(tokens, vec![Token::Ident("my-var".into()), Token::Eof]);

        let tokens = lex_ok("my-long-variable-name");
        assert_eq!(
            tokens,
            vec![Token::Ident("my-long-variable-name".into()), Token::Eof]
        );
    }

    #[test]
    fn subtraction_with_spaces() {
        let tokens = lex_ok("a - b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::Minus,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn train_case_subtraction_mixed() {
        let tokens = lex_ok("end-time - start-time");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("end-time".into()),
                Token::Minus,
                Token::Ident("start-time".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn keywords_case_insensitive() {
        assert_eq!(lex_ok("SET")[0], Token::Set);
        assert_eq!(lex_ok("set")[0], Token::Set);
        assert_eq!(lex_ok("Set")[0], Token::Set);
        assert_eq!(lex_ok("sEt")[0], Token::Set);
    }

    #[test]
    fn all_keywords() {
        let tokens =
            lex_ok("LET SET KILL OUTPUT IF ELSE AND OR NOT TRUE FALSE");
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Set,
                Token::Kill,
                Token::Output,
                Token::If,
                Token::Else,
                Token::And,
                Token::Or,
                Token::Not,
                Token::True,
                Token::False,
                Token::Eof
            ]
        );
    }

    #[test]
    fn global_variable() {
        let tokens = lex_ok("^PATIENT");
        assert_eq!(tokens, vec![Token::Global("PATIENT".into()), Token::Eof]);
    }

    #[test]
    fn global_train_case() {
        let tokens = lex_ok("^my-global");
        assert_eq!(tokens, vec![Token::Global("my-global".into()), Token::Eof]);
    }

    #[test]
    fn arithmetic_operators() {
        let tokens = lex_ok("+ - * / // %");
        assert_eq!(
            tokens,
            vec![
                Token::Plus,
                Token::Minus,
                Token::Mul,
                Token::Div,
                Token::FloorDiv,
                Token::Modulo,
                Token::Eof
            ]
        );
    }

    #[test]
    fn comparison_operators() {
        let tokens = lex_ok("== != < > <= >=");
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::Ne,
                Token::Lt,
                Token::Gt,
                Token::Le,
                Token::Ge,
                Token::Eof
            ]
        );
    }

    #[test]
    fn logical_operators() {
        let tokens = lex_ok("&& || !");
        assert_eq!(
            tokens,
            vec![Token::AmpAmp, Token::PipePipe, Token::Bang, Token::Eof]
        );
    }

    #[test]
    fn concat_operator() {
        let tokens = lex_ok("++");
        assert_eq!(tokens, vec![Token::Concat, Token::Eof]);
    }

    #[test]
    fn assignment() {
        let tokens = lex_ok("=");
        assert_eq!(tokens, vec![Token::Assign, Token::Eof]);
    }

    #[test]
    fn punctuation() {
        let tokens = lex_ok("( ) { } [ ] , : . ..");
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBrace,
                Token::RBrace,
                Token::LBracket,
                Token::RBracket,
                Token::Comma,
                Token::Colon,
                Token::Dot,
                Token::DotDot,
                Token::Eof
            ]
        );
    }

    #[test]
    fn comments_stripped() {
        let tokens = lex_ok("SET x = 10 ; this is a comment");
        assert_eq!(
            tokens,
            vec![
                Token::Set,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Int(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn comment_whole_line() {
        let tokens = lex_ok("; entire line is comment\nSET x = 1");
        assert_eq!(
            tokens,
            vec![
                Token::Newline,
                Token::Set,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Int(1),
                Token::Eof
            ]
        );
    }

    #[test]
    fn newlines_preserved() {
        let tokens = lex_ok("SET x = 10\nOUTPUT x");
        assert_eq!(
            tokens,
            vec![
                Token::Set,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Int(10),
                Token::Newline,
                Token::Output,
                Token::Ident("x".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn indentation_basic() {
        let tokens = lex_ok("IF x\n  OUTPUT y");
        assert!(tokens.contains(&Token::Indent));
    }

    #[test]
    fn indentation_dedent() {
        let tokens = lex_ok("IF x\n  OUTPUT y\nSET z = 1");
        assert!(tokens.contains(&Token::Indent));
        assert!(tokens.contains(&Token::Dedent));
    }

    #[test]
    fn complex_expression() {
        let tokens = lex_ok("SET sum = x + y * (z - 10)");
        assert_eq!(
            tokens,
            vec![
                Token::Set,
                Token::Ident("sum".into()),
                Token::Assign,
                Token::Ident("x".into()),
                Token::Plus,
                Token::Ident("y".into()),
                Token::Mul,
                Token::LParen,
                Token::Ident("z".into()),
                Token::Minus,
                Token::Int(10),
                Token::RParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn if_else_block() {
        let tokens = lex_ok(
            "IF x > 10 {\n  OUTPUT \"big\"\n} ELSE {\n  OUTPUT \"small\"\n}",
        );
        assert!(tokens.contains(&Token::If));
        assert!(tokens.contains(&Token::Else));
        assert!(tokens.contains(&Token::LBrace));
        assert!(tokens.contains(&Token::RBrace));
    }

    #[test]
    fn global_with_subscripts() {
        let tokens = lex_ok("^PATIENT(123, \"NAME\")");
        assert_eq!(
            tokens,
            vec![
                Token::Global("PATIENT".into()),
                Token::LParen,
                Token::Int(123),
                Token::Comma,
                Token::String("NAME".into()),
                Token::RParen,
                Token::Eof
            ]
        );
    }

    #[test]
    fn spans_correct() {
        let result = lex_spanned("SET x");
        assert_eq!(result[0], Spanned(Token::Set, Span::new(0, 3)));
        assert_eq!(
            result[1],
            Spanned(Token::Ident("x".into()), Span::new(4, 5))
        );
    }

    #[test]
    fn number_followed_by_range() {
        let tokens = lex_ok("1..10");
        assert_eq!(
            tokens,
            vec![Token::Int(1), Token::DotDot, Token::Int(10), Token::Eof]
        );
    }

    #[test]
    fn unknown_char_error() {
        let errs = lex_err("SET x = @invalid");
        assert!(!errs.is_empty());
        assert!(errs[0].to_string().contains("unexpected character"));
    }

    // NOTE: Single `&` and `|` now produce `unexpected character` errors
    // rather than specific "expected `&&`" messages.
    #[test]
    fn single_ampersand_error() {
        let errs = lex_err("a & b");
        assert!(!errs.is_empty());
    }

    #[test]
    fn single_pipe_error() {
        let errs = lex_err("a | b");
        assert!(!errs.is_empty());
    }
}
