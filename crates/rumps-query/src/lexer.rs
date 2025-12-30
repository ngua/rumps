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
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;

use crate::{Error, Result, Span, Token};

/// A token paired with its source span.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned {
    pub(crate) tok: Token,
    pub(crate) span: Span,
}

impl Spanned {
    fn new(tok: Token, span: Span) -> Self {
        Self { tok, span }
    }

    fn from_range(tok: Token, range: Range<usize>) -> Self {
        Self {
            tok,
            span: Span::from(range),
        }
    }

    /// Post-processes tokens to convert `DotDot` to `DotDotNoSpace`.
    ///
    /// A `DotDot` token immediately following another token (no whitespace)
    /// is converted to `DotDotNoSpace` for JSON scalar access syntax.
    fn process_dot_dot(tokens: Vec<Self>) -> Vec<Self> {
        tokens
            .into_iter()
            .scan(None::<u32>, |prev_end, mut t| {
                let is_adjacent =
                    prev_end.map(|e| e == t.span.start).unwrap_or(false);
                if t.tok == Token::DotDot && is_adjacent {
                    t.tok = Token::DotDotNoSpace;
                }
                *prev_end = Some(t.span.end);
                Some(t)
            })
            .collect()
    }

    /// Post-processes tokens to add `Indent` and `Dedent` tokens.
    ///
    /// Uses iterative `fold` instead of recursion to avoid stack overflow
    /// on large files.
    fn process_indentation(tokens: Vec<Self>) -> Vec<Self> {
        struct State {
            result: Vec<Spanned>,
            indent_stack: Vec<usize>,
            after_newline: bool,
            pending_span: Span,
        }

        let with_cols = TokenWithCol::from_spanned(&tokens);
        let cap = tokens.len();

        let st = with_cols.into_iter().fold(
            State {
                result: Vec::with_capacity(cap),
                indent_stack: vec![0],
                after_newline: false,
                pending_span: Span::default(),
            },
            |mut st, t| {
                match &t.tok {
                    Token::Newline => {
                        // Only push the first newline; skip consecutive ones
                        if !st.after_newline {
                            st.result.push(Self::new(t.tok.clone(), t.span));
                            st.after_newline = true;
                            st.pending_span = t.span;
                        }
                    }
                    Token::Eof => {
                        // Emit final dedents before EOF
                        (1..st.indent_stack.len()).for_each(|_| {
                            st.result.push(Self::new(Token::Dedent, t.span));
                        });
                        st.result.push(Self::new(t.tok.clone(), t.span));
                    }
                    _ => {
                        if st.after_newline {
                            let cur =
                                st.indent_stack.last().copied().unwrap_or(0);
                            if t.col > cur {
                                st.indent_stack.push(t.col);
                                st.result.push(Self::new(
                                    Token::Indent,
                                    st.pending_span,
                                ));
                            } else {
                                Self::emit_dedents(
                                    &mut st.result,
                                    &mut st.indent_stack,
                                    t.col,
                                    st.pending_span,
                                );
                            }
                            st.after_newline = false;
                        }
                        st.result.push(Self::new(t.tok.clone(), t.span));
                    }
                }
                st
            },
        );

        st.result
    }

    /// Emits `Dedent` tokens to return to target indent level.
    fn emit_dedents(
        result: &mut Vec<Self>,
        stack: &mut Vec<usize>,
        target: usize,
        span: Span,
    ) {
        let count = stack.iter().rev().take_while(|&&lvl| lvl > target).count();
        (0..count).for_each(|_| {
            stack.pop();
            result.push(Self::new(Token::Dedent, span));
        });
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
    pub(crate) fn lex(self) -> Result<Vec<Spanned>> {
        Self::lexer()
            .parse(self.src)
            .map(Spanned::process_dot_dot)
            .map(Spanned::process_indentation)
            .map_err(|errs| {
                NonEmpty::collect(errs.into_iter().map(Self::to_error))
                    .map(Error::multiple)
                    .unwrap_or_else(|| {
                        Error::runtime_no_span("unknown lex error")
                    })
            })
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
            Self::char_lit(),
            Self::regex_lit(),
            Self::number(),
            Self::global(),
            Self::intrinsic(),
            Self::ident_or_keyword(),
            Self::operator_or_punct(),
        ))
    }

    /// MUMPS intrinsic: `$SET`, `$GET`, `$KILL`, `$OUTPUT`, `$DATA`, `$ORDER`.
    ///
    /// Case-insensitive (e.g., `$set`, `$SET`, `$Set` all work).
    fn intrinsic() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        just('$').ignore_then(Self::ident_chars()).map_with_span(
            |name, span| {
                Token::intrinsic(&name)
                    .map(|tok| Spanned::from_range(tok, span.clone()))
                    .unwrap_or_else(|| {
                        // Unknown `$xxx` is an error; we'll let it fall through
                        // as an identifier which will cause a parse error later
                        Spanned::from_range(
                            Token::Ident(format!("${name}")),
                            span,
                        )
                    })
            },
        )
    }

    /// Regex literal: `/pattern/`.
    ///
    /// Handles escape sequences (`\/`, `\\`). Regex ends at unescaped `/`.
    /// Distinguished from division by requiring non-whitespace content
    /// immediately after the opening `/`.
    fn regex_lit() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        // Escape sequence: `\X` where X is any character except newline
        // Preserves the backslash in the output for the regex engine
        let escape_seq = just('\\')
            .then(filter(|c: &char| *c != '\n'))
            .map(|(bs, c)| format!("{bs}{c}"));

        // Regular character in regex (not `/`, `\`, or newline)
        let regular = filter(|c: &char| *c != '/' && *c != '\\' && *c != '\n')
            .map(|c: char| c.to_string());

        // Content character: escape sequence or regular character
        let content_char = escape_seq.or(regular);

        // First element must be non-whitespace to distinguish `/pattern/` from `a / b`
        // Can be either a non-whitespace regular char or an escape sequence
        let first_regular = filter(|c: &char| {
            !c.is_whitespace() && *c != '/' && *c != '\\' && *c != '\n'
        })
        .map(|c: char| c.to_string());
        let first_element = escape_seq.or(first_regular);

        // Regex pattern: `/` + first element + more content + `/`
        just('/')
            .ignore_then(first_element)
            .then(content_char.repeated().collect::<Vec<_>>())
            .then_ignore(just('/'))
            .map_with_span(|(first, rest), span| {
                let pattern =
                    std::iter::once(first).chain(rest).collect::<String>();
                Spanned::from_range(Token::Regex(pattern), span)
            })
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

    fn char_lit() -> impl Parser<char, Spanned, Error = LexErr> + Clone {
        let escape = just('\\').ignore_then(choice((
            just('n').to('\n'),
            just('r').to('\r'),
            just('t').to('\t'),
            just('\\').to('\\'),
            just('\'').to('\''),
            just('0').to('\0'),
        )));

        let char_in_lit = escape
            .or(filter(|c: &char| *c != '\'' && *c != '\\' && *c != '\n'));

        just('\'')
            .ignore_then(char_in_lit)
            .then_ignore(just('\''))
            .map_with_span(|c, span| Spanned::from_range(Token::Char(c), span))
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
                            Spanned::from_range(
                                Token::Float(OrderedFloat(f)),
                                span.clone(),
                            )
                        })
                        .unwrap_or_else(|_| {
                            Spanned::from_range(
                                Token::Float(OrderedFloat(0.0)),
                                span,
                            )
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
                                        Token::Float(OrderedFloat(f)),
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

        // Train-case continuation: `-` immediately followed by alphanumeric
        let train_cont = just('-')
            .then(filter(|c: &char| c.is_alphanumeric()))
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
            just("**").to(Token::StarStar),
            just("//").to(Token::FloorDiv),
            just("==").to(Token::Eq),
            just("!=").to(Token::Ne),
            just("<=").to(Token::Le),
            just(">=").to(Token::Ge),
            just("&&").to(Token::AmpAmp),
            just("||").to(Token::PipePipe),
            just("|>").to(Token::Pipe),
            just("...").to(Token::DotDotDot),
            just("..=").to(Token::DotDotEquals),
            just("..").to(Token::DotDot),
            just("->>").to(Token::ArrowArrow),
            just("->").to(Token::Arrow),
            just("=>").to(Token::FatArrow),
            just("??").to(Token::QuestionQuestion),
            just("?.").to(Token::QuestionDot),
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
            just('|').to(Token::SinglePipe),
            just('?').to(Token::Question),
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
        tokens
            .iter()
            .scan(0u32, |line_start, Spanned { tok, span }| {
                let col = (span.start - *line_start) as usize;
                if *tok == Token::Newline {
                    *line_start = span.end;
                }
                Some(Self {
                    tok: tok.clone(),
                    span: *span,
                    col,
                })
            })
            .collect()
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
            .map(|s| s.tok)
            .collect()
    }

    fn lex_err(src: &str) -> Error {
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
        assert_eq!(tokens, vec![Token::Float(OrderedFloat(3.14)), Token::Eof]);
    }

    #[test]
    fn float_with_exponent() {
        let tokens = lex_ok("1e10");
        assert_eq!(tokens, vec![Token::Float(OrderedFloat(1e10)), Token::Eof]);

        let tokens = lex_ok("2.5e-3");
        assert_eq!(
            tokens,
            vec![Token::Float(OrderedFloat(2.5e-3)), Token::Eof]
        );
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
        let err = lex_err(r#""unterminated"#);
        assert!(err.to_string().contains("unexpected"));
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

        // Trailing numbers
        let tokens = lex_ok("my-var-2");
        assert_eq!(tokens, vec![Token::Ident("my-var-2".into()), Token::Eof]);

        let tokens = lex_ok("var-123abc");
        assert_eq!(tokens, vec![Token::Ident("var-123abc".into()), Token::Eof]);
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
        assert_eq!(lex_ok("LET")[0], Token::Let);
        assert_eq!(lex_ok("let")[0], Token::Let);
        assert_eq!(lex_ok("Let")[0], Token::Let);
        assert_eq!(lex_ok("lEt")[0], Token::Let);
    }

    #[test]
    fn intrinsics_case_insensitive() {
        assert_eq!(lex_ok("$SET")[0], Token::Set);
        assert_eq!(lex_ok("$set")[0], Token::Set);
        assert_eq!(lex_ok("$Set")[0], Token::Set);
        assert_eq!(lex_ok("$sEt")[0], Token::Set);
    }

    #[test]
    fn all_keywords() {
        let tokens = lex_ok("LET IF ELSE AND OR NOT TRUE FALSE FUN TYPE");
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::If,
                Token::Else,
                Token::And,
                Token::Or,
                Token::Not,
                Token::True,
                Token::False,
                Token::Fun,
                Token::Type,
                Token::Eof
            ]
        );
    }

    #[test]
    fn all_intrinsics() {
        let tokens = lex_ok("$SET $GET $KILL $OUTPUT $DATA $ORDER");
        assert_eq!(
            tokens,
            vec![
                Token::Set,
                Token::Get,
                Token::Kill,
                Token::Output,
                Token::Data,
                Token::Order,
                Token::Eof
            ]
        );
    }

    #[test]
    fn read_is_keyword_not_intrinsic() {
        // READ is a keyword, not a `$`-prefixed intrinsic
        let tokens = lex_ok("READ read");
        assert_eq!(tokens, vec![Token::Read, Token::Read, Token::Eof]);
        // `$READ` becomes an identifier (unknown intrinsic)
        let tokens = lex_ok("$READ");
        assert_eq!(tokens[0], Token::Ident("$READ".into()));
    }

    #[test]
    fn intrinsics_are_not_keywords() {
        // Without `$` prefix, these are identifiers, not intrinsics
        let tokens = lex_ok("SET GET KILL OUTPUT DATA ORDER");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("SET".into()),
                Token::Ident("GET".into()),
                Token::Ident("KILL".into()),
                Token::Ident("OUTPUT".into()),
                Token::Ident("DATA".into()),
                Token::Ident("ORDER".into()),
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
        let tokens = lex_ok("$SET x = 10 ; this is a comment");
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
        let tokens = lex_ok("; entire line is comment\n$SET x = 1");
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
        let tokens = lex_ok("$SET x = 10\n$OUTPUT x");
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
    fn indentation_tokens_preserved() {
        // Indent/Dedent tokens are preserved for formatters; parser handles them
        let tokens = lex_ok("IF x\n  $OUTPUT y");
        assert!(tokens.contains(&Token::Indent));
        assert!(tokens.contains(&Token::Dedent));
        assert!(tokens.contains(&Token::If));
        assert!(tokens.contains(&Token::Output));
    }

    #[test]
    fn all_whitespace_tokens_preserved() {
        // All whitespace tokens preserved for formatters
        let tokens = lex_ok("IF x\n  $OUTPUT y\n$SET z = 1");
        assert!(tokens.contains(&Token::Indent));
        assert!(tokens.contains(&Token::Dedent));
        assert!(tokens.contains(&Token::Newline));
        assert!(tokens.contains(&Token::Set));
    }

    #[test]
    fn complex_expression() {
        let tokens = lex_ok("$SET sum = x + y * (z - 10)");
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
            "IF x > 10 {\n  $OUTPUT \"big\"\n} ELSE {\n  $OUTPUT \"small\"\n}",
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
    fn output_as_expr() {
        // `$OUTPUT` after `=` should lex correctly
        let tokens = lex_ok("LET x = $OUTPUT \"hello\"");
        assert_eq!(
            tokens,
            vec![
                Token::Let,
                Token::Ident("x".into()),
                Token::Assign,
                Token::Output,
                Token::String("hello".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn spans_correct() {
        let result = lex_spanned("$SET x");
        assert_eq!(
            result[0],
            Spanned {
                tok: Token::Set,
                span: Span::new(0, 4) // `$SET` is 4 chars
            }
        );
        assert_eq!(
            result[1],
            Spanned {
                tok: Token::Ident("x".into()),
                span: Span::new(5, 6)
            }
        );
    }

    #[test]
    fn number_followed_by_dot_dot_no_space() {
        // `1..10` without space is DotDotNoSpace (JSON scalar access)
        let tokens = lex_ok("1..10");
        assert_eq!(
            tokens,
            vec![
                Token::Int(1),
                Token::DotDotNoSpace,
                Token::Int(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn number_followed_by_range() {
        // `1 .. 10` with spaces is DotDot (range operator)
        let tokens = lex_ok("1 .. 10");
        assert_eq!(
            tokens,
            vec![Token::Int(1), Token::DotDot, Token::Int(10), Token::Eof]
        );
    }

    #[test]
    fn coalesce_operator() {
        let tokens = lex_ok("a ?? b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::QuestionQuestion,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn optional_chaining_operator() {
        let tokens = lex_ok("a?.b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::QuestionDot,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn optional_chaining_chain() {
        let tokens = lex_ok("a?.b?.c");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::QuestionDot,
                Token::Ident("b".into()),
                Token::QuestionDot,
                Token::Ident("c".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn arrow_token() {
        let tokens = lex_ok("Int -> Int");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("Int".into()),
                Token::Arrow,
                Token::Ident("Int".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn arrow_not_minus() {
        // `->` should lex as Arrow, not Minus followed by Gt
        let tokens = lex_ok("a->b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::Arrow,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn arrow_in_function_type() {
        // `(Int, Int) -> Int`
        let tokens = lex_ok("(Int, Int) -> Int");
        assert_eq!(
            tokens,
            vec![
                Token::LParen,
                Token::Ident("Int".into()),
                Token::Comma,
                Token::Ident("Int".into()),
                Token::RParen,
                Token::Arrow,
                Token::Ident("Int".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn minus_still_works() {
        // Ensure `-` still works when not followed by `>`
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
    fn unknown_char_error() {
        let err = lex_err("$SET x = @invalid");
        assert!(err.to_string().contains("unexpected character"));
    }

    #[test]
    fn pipe_operator() {
        let tokens = lex_ok("a |> b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::Pipe,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn pipe_chain() {
        let tokens = lex_ok("a |> b |> c");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::Pipe,
                Token::Ident("b".into()),
                Token::Pipe,
                Token::Ident("c".into()),
                Token::Eof
            ]
        );
    }

    // NOTE: Single `&` and `|` now produce `unexpected character` errors
    // rather than specific "expected `&&`" messages.
    #[test]
    fn single_ampersand_error() {
        let err = lex_err("a & b");
        assert!(err.to_string().contains("unexpected"));
    }

    #[test]
    fn single_pipe_token() {
        // `|` is a valid token (variant separator in TYPE declarations)
        let tokens = lex_ok("a | b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::SinglePipe,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn range_exclusive() {
        // Range with spaces
        let tokens = lex_ok("1 .. 10");
        assert_eq!(
            tokens,
            vec![Token::Int(1), Token::DotDot, Token::Int(10), Token::Eof]
        );
    }

    #[test]
    fn range_inclusive() {
        // Inclusive range; `..=` is always range (no JSON scalar equiv)
        let tokens = lex_ok("1..=10");
        assert_eq!(
            tokens,
            vec![
                Token::Int(1),
                Token::DotDotEquals,
                Token::Int(10),
                Token::Eof
            ]
        );
    }

    #[test]
    fn range_with_identifiers() {
        // Range with spaces
        let tokens = lex_ok("start .. end");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("start".into()),
                Token::DotDot,
                Token::Ident("end".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn range_vs_spread() {
        // `...` should be spread, not `..` + `.`
        let tokens = lex_ok("...x");
        assert_eq!(
            tokens,
            vec![Token::DotDotDot, Token::Ident("x".into()), Token::Eof]
        );
    }

    #[test]
    fn dot_dot_with_space_is_range() {
        // `a .. b` with spaces is range operator
        let tokens = lex_ok("a .. b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::DotDot,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn dot_dot_no_space_is_json_scalar() {
        // `a..b` without space before `..` is JSON scalar access
        let tokens = lex_ok("a..b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::DotDotNoSpace,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn dot_dot_mixed_spacing() {
        // Space after but not before: `a.. b`
        let tokens = lex_ok("a.. b");
        assert_eq!(
            tokens,
            vec![
                Token::Ident("a".into()),
                Token::DotDotNoSpace,
                Token::Ident("b".into()),
                Token::Eof
            ]
        );
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;

    #[test]
    fn continuation_preserves_all_tokens() {
        // Lexer preserves all tokens; parser handles continuation
        let src = "LET x = 1\n    + 2\n$OUTPUT x";
        let tokens: Vec<_> = Lexer::new(src)
            .lex()
            .expect("lex")
            .iter()
            .map(|t| t.tok.clone())
            .collect();

        // All whitespace tokens preserved for formatters
        assert!(tokens.contains(&Token::Indent));
        assert!(tokens.contains(&Token::Dedent));
        assert!(tokens.contains(&Token::Newline));

        // Actual tokens present
        assert!(tokens.contains(&Token::Int(1)));
        assert!(tokens.contains(&Token::Plus));
        assert!(tokens.contains(&Token::Int(2)));
    }

    #[test]
    fn multi_line_preserves_structure() {
        let src = "LET x = 1\n    + 2\n    + 3\n$OUTPUT x";
        let tokens: Vec<_> = Lexer::new(src)
            .lex()
            .expect("lex")
            .iter()
            .map(|t| t.tok.clone())
            .collect();

        // All tokens preserved
        assert!(tokens.contains(&Token::Indent));
        assert!(tokens.contains(&Token::Dedent));

        // All expression tokens present
        assert!(tokens.iter().filter(|t| **t == Token::Plus).count() == 2);
        assert!(tokens.contains(&Token::Int(1)));
        assert!(tokens.contains(&Token::Int(2)));
        assert!(tokens.contains(&Token::Int(3)));
    }

    #[test]
    fn simple_statements_have_newlines() {
        let src = "LET x = 1\n$OUTPUT x";
        let tokens: Vec<_> = Lexer::new(src)
            .lex()
            .expect("lex")
            .iter()
            .map(|t| t.tok.clone())
            .collect();

        // Newline separates statements
        assert!(tokens.contains(&Token::Newline));
    }
}

#[cfg(test)]
mod array_in_continuation_test {
    use super::*;

    #[test]
    fn array_after_continuations() {
        // This is the pattern that's failing
        let src = r#"LET x = 1
    + 2
LET arr = [
    1,
    2
]
$OUTPUT arr[0]"#;

        let tokens: Vec<_> = Lexer::new(src)
            .lex()
            .expect("lex")
            .iter()
            .map(|t| t.tok.clone())
            .collect();

        eprintln!("Tokens:");
        tokens.iter().for_each(|t| eprintln!("  {:?}", t));

        // Try parsing
        use crate::Parser;
        match Parser::parse(src) {
            Ok(r) => eprintln!("Parsed {} statements", r.stmts.len()),
            Err(e) => panic!("Parse error: {}", e),
        }
    }
}

#[cfg(test)]
mod array_indent_debug {
    use super::*;

    #[test]
    fn existing_array_tokens() {
        // This is from test 23 which works
        let src = r#"LET matrix3d = [
  [[1, 2], [3, 4]],
  [[5, 6], [7, 8]]
]"#;

        let tokens: Vec<_> = Lexer::new(src)
            .lex()
            .expect("lex")
            .iter()
            .map(|t| t.tok.clone())
            .collect();

        eprintln!("Tokens for working array:");
        tokens.iter().for_each(|t| eprintln!("  {:?}", t));
    }
}
