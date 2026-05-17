//! Lexer for the RUMPS query language.
//!
//! Converts source text into a stream of tokens with spans. Handles:
//! - Train-case identifiers (`my-var`) vs spaced subtraction (`a - b`)
//! - Comments (`;` to end of line)
//! - Indentation tracking (`Indent`/`Dedent` tokens)
//! - String literals with escape sequences

#![allow(dead_code)]

use std::ops::Range;

use chumsky::prelude::*;
use chumsky::primitive::any;
use nonempty::NonEmpty;
use ordered_float::OrderedFloat;

use crate::intern::{StringId, StringInterner};
use crate::{Error, Result, Span, Token};

/// A token paired with its source span.
///
/// The type parameter `S` is the string representation; use `StringId` (the
/// default) for interned tokens and `String` for raw, pre-interning tokens.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned<S = StringId> {
    pub(crate) tok: Token<S>,
    pub(crate) span: Span,
}

impl<S> Spanned<S> {
    fn new(tok: Token<S>, span: Span) -> Self {
        Self { tok, span }
    }

    fn from_range(tok: Token<S>, range: Range<usize>) -> Self {
        Self {
            tok,
            span: Span::from(range),
        }
    }
}

impl<S: Clone + PartialEq> Spanned<S> {
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
        struct State<S> {
            result: Vec<Spanned<S>>,
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
}

impl Spanned<String> {
    /// Converts `Colon` to `ColonNoSpace` for class method syntax.
    ///
    /// Only converts when `:` has NO space on either side AND immediately
    /// follows either:
    /// - An uppercase identifier (class names like `Filterable:filter`)
    /// - A `]` token (for `Class[T]:method` syntax)
    ///
    /// This distinguishes `Class:method` from type annotations like `x: Type`.
    fn process_colon(mut tokens: Vec<Self>) -> Vec<Self> {
        (0..tokens.len()).for_each(|i| {
            if tokens[i].tok == Token::Colon {
                let prev = (i > 0).then(|| &tokens[i - 1]);
                let next = tokens.get(i + 1);

                // Check: adjacent to previous uppercase ident (len > 1) OR `]`
                let adj_prev = prev
                    .map(|p| {
                        let is_adj = p.span.end == tokens[i].span.start;
                        let is_class = matches!(
                            &p.tok,
                            Token::Ident(s)
                                if s.len() > 1
                                    && s.starts_with(char::is_uppercase)
                        );
                        let is_rbracket = matches!(&p.tok, Token::RBracket);
                        is_adj && (is_class || is_rbracket)
                    })
                    .unwrap_or(false);

                // Check: adjacent to next token (no space after colon)
                let adj_next = next
                    .map(|n| tokens[i].span.end == n.span.start)
                    .unwrap_or(false);

                // Naked class method: `:method` where the previous
                // adjacent token is not something that could end an
                // expression (that would be an inline type annotation
                // like `1:Int` or `x:Type`)
                let prev_ends_expr = prev
                    .map(|p| {
                        p.span.end == tokens[i].span.start
                            && matches!(
                                &p.tok,
                                Token::Ident(_)
                                    | Token::Int(_)
                                    | Token::Float(_)
                                    | Token::String(_)
                                    | Token::Char(_)
                                    | Token::True
                                    | Token::False
                                    | Token::Null
                                    | Token::RParen
                                    | Token::RBracket
                                    | Token::RBrace
                            )
                    })
                    .unwrap_or(false);
                // Require lowercase ident after `:` to avoid
                // treating `:Type` (inline type annotation) as a
                // naked method; method names are always lowercase
                let naked = !adj_prev
                    && !prev_ends_expr
                    && adj_next
                    && next
                        .map(|n| {
                            matches!(
                                &n.tok,
                                Token::Ident(s)
                                    if s.starts_with(char::is_lowercase)
                            )
                        })
                        .unwrap_or(false);

                if (adj_prev && adj_next) || naked {
                    tokens[i].tok = Token::ColonNoSpace;
                }
            }
        });
        tokens
    }

    /// Post-processes tokens to merge `Ident`/`Global` + `LBrace` into
    /// `IdentBrace`/`GlobalBrace` when adjacent (no whitespace).
    ///
    /// This allows the parser to distinguish:
    /// - `data{1, 2}` -> DB ref literal (IdentBrace)
    /// - `data { ... }` -> variable followed by block (Ident + LBrace)
    fn process_ref_braces(tokens: Vec<Self>) -> Vec<Self> {
        // State: (accumulated tokens, pending ident/global that might merge)
        let (mut acc, pending) = tokens.into_iter().fold(
            (Vec::new(), None::<Self>),
            |(mut acc, pending), t| match pending {
                Some(p) => {
                    // Check if current token is `LBrace` adjacent to pending
                    let merged = (t.tok == Token::LBrace
                        && p.span.end == t.span.start)
                        .then(|| match &p.tok {
                            Token::Ident(name) => Self::new(
                                Token::IdentBrace(name.clone()),
                                Span::new(p.span.start, t.span.end),
                            ),
                            Token::Global(name) => Self::new(
                                Token::GlobalBrace(name.clone()),
                                Span::new(p.span.start, t.span.end),
                            ),
                            _ => unreachable!(),
                        });
                    match merged {
                        Some(m) => {
                            acc.push(m);
                            (acc, None)
                        }
                        None => {
                            acc.push(p);
                            // Current token might be ident/global for next merge
                            match &t.tok {
                                Token::Ident(_) | Token::Global(_) => {
                                    (acc, Some(t))
                                }
                                _ => {
                                    acc.push(t);
                                    (acc, None)
                                }
                            }
                        }
                    }
                }
                None => match &t.tok {
                    Token::Ident(_) | Token::Global(_) => (acc, Some(t)),
                    _ => {
                        acc.push(t);
                        (acc, None)
                    }
                },
            },
        );
        // Flush any pending token
        if let Some(p) = pending {
            acc.push(p);
        }
        acc
    }
}

impl Spanned<String> {
    /// Interns all string-carrying tokens, converting `Vec<Spanned<String>>` to
    /// `Vec<Spanned<StringId>>`.
    pub(crate) fn intern_all(
        tokens: Vec<Self>,
        interner: &mut StringInterner,
    ) -> Vec<Spanned> {
        tokens
            .into_iter()
            .map(|s| Spanned {
                tok: s.tok.intern(interner),
                span: s.span,
            })
            .collect()
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

    /// Lexes source code into a stream of raw (pre-interning) spanned tokens.
    pub(crate) fn lex(self) -> Result<Vec<Spanned<String>>> {
        Self::lexer()
            .parse(self.src)
            .map(Spanned::process_dot_dot)
            .map(Spanned::process_colon)
            .map(Spanned::process_ref_braces)
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
    fn lexer() -> impl Parser<char, Vec<Spanned<String>>, Error = LexErr> {
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
    fn newline() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone {
        just('\n')
            .map_with_span(|_, span| Spanned::from_range(Token::Newline, span))
    }

    /// A single token (not whitespace, not comment, not newline).
    fn token() -> impl Parser<char, Spanned<String>, Error = LexErr> {
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

    /// DB intrinsic: `@set`, `@get`, `@kill`, `@data`, `@order`, `@query`.
    fn intrinsic() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone
    {
        just('@').ignore_then(Self::ident_chars()).map_with_span(
            |name, span| {
                Token::intrinsic(&name)
                    .map(|tok| Spanned::from_range(tok, span.clone()))
                    .unwrap_or_else(|| {
                        // Unknown `@xxx` is an error; we'll let it fall through
                        // as an identifier which will cause a parse error later
                        Spanned::from_range(
                            Token::Ident(format!("@{name}")),
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
    fn regex_lit() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone
    {
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

    fn string_lit() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone
    {
        just('"')
            .ignore_then(Self::string_content())
            .then_ignore(just('"'))
            .map_with_span(|parts, span| {
                // If no interpolation (single literal part), return plain string
                if parts.len() == 1 {
                    Spanned::from_range(
                        Token::String(
                            parts.into_iter().next().unwrap_or_default(),
                        ),
                        span,
                    )
                } else {
                    Spanned::from_range(Token::Interpolation(parts), span)
                }
            })
    }

    /// Parse string content, returning alternating literal/expression parts.
    ///
    /// - Even indices: literal text (may be empty)
    /// - Odd indices: expression source code
    ///
    /// Handles `{{` and `}}` as escaped braces in literals.
    fn string_content() -> impl Parser<char, Vec<String>, Error = LexErr> + Clone
    {
        let escape = just('\\').ignore_then(choice((
            just('n').to('\n'),
            just('r').to('\r'),
            just('t').to('\t'),
            just('\\').to('\\'),
            just('"').to('"'),
            just('{').to('{'),
            just('}').to('}'),
            just('0').to('\0'),
        )));

        // `{{` produces literal `{`
        let escaped_open = just("{{").to('{');
        // `}}` produces literal `}`
        let escaped_close = just("}}").to('}');

        // Regular char: not `"`, `\`, `{`, `}`, or newline
        let regular = filter(|c: &char| {
            *c != '"' && *c != '\\' && *c != '{' && *c != '}' && *c != '\n'
        });

        // Literal segment char: escape, escaped brace, or regular
        let lit_char = choice((escape, escaped_open, escaped_close, regular));

        // Literal segment: zero or more literal chars
        let literal_seg = lit_char.repeated().collect::<String>();

        // Expression content: balanced braces, handles nested `{}` and strings
        let expr_content = Self::interpolation_expr();

        // Interpolation: `{` expr `}`
        let interpolation =
            just('{').ignore_then(expr_content).then_ignore(just('}'));

        // Alternating: literal, then optionally (expr, literal)*
        literal_seg
            .then(interpolation.then(literal_seg).repeated())
            .map(|(first, rest)| {
                let mut parts = vec![first];
                rest.into_iter().for_each(|(expr, lit)| {
                    parts.push(expr);
                    parts.push(lit);
                });
                parts
            })
    }

    /// Parse interpolation expression content (balanced braces).
    ///
    /// Handles nested `{}`, strings, and chars within the expression.
    fn interpolation_expr() -> impl Parser<char, String, Error = LexErr> + Clone
    {
        recursive(
            |expr: chumsky::recursive::Recursive<char, String, LexErr>| {
                // Nested string: "..."
                let nested_string = just('"')
                    .then(
                        choice((
                            just('\\').then(any()).map(
                                |(a, b): (char, char)| {
                                    let mut s = String::new();
                                    s.push(a);
                                    s.push(b);
                                    s
                                },
                            ),
                            filter(|c: &char| {
                                *c != '"' && *c != '\\' && *c != '\n'
                            })
                            .map(|c: char| c.to_string()),
                        ))
                        .repeated(),
                    )
                    .then(just('"'))
                    .map(
                        |((open, chars), close): (
                            (char, Vec<String>),
                            char,
                        )| {
                            let mut s = String::new();
                            s.push(open);
                            chars.into_iter().for_each(|c| s.push_str(&c));
                            s.push(close);
                            s
                        },
                    );

                // Nested char: '.'
                let nested_char = just('\'')
                    .then(
                        just('\\')
                            .then(any())
                            .map(|(a, b): (char, char)| {
                                let mut s = String::new();
                                s.push(a);
                                s.push(b);
                                s
                            })
                            .or(filter(|c: &char| *c != '\'' && *c != '\n')
                                .map(|c: char| c.to_string())),
                    )
                    .then(just('\''))
                    .map(|((open, ch), close): ((char, String), char)| {
                        let mut s = String::new();
                        s.push(open);
                        s.push_str(&ch);
                        s.push(close);
                        s
                    });

                // Nested braces: { ... }
                let nested_braces = just('{').then(expr).then(just('}')).map(
                    |((open, inner), close): ((char, String), char)| {
                        let mut s = String::new();
                        s.push(open);
                        s.push_str(&inner);
                        s.push(close);
                        s
                    },
                );

                // Regular char: not `{`, `}`, `"`, `'`, or newline
                let regular = filter(|c: &char| {
                    *c != '{'
                        && *c != '}'
                        && *c != '"'
                        && *c != '\''
                        && *c != '\n'
                })
                .map(|c: char| c.to_string());

                // Combine all and repeat
                choice((nested_string, nested_char, nested_braces, regular))
                    .repeated()
                    .collect::<Vec<String>>()
                    .map(|parts| parts.join(""))
            },
        )
    }

    fn char_lit() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone
    {
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

    fn number() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone {
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

    fn global() -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone {
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

    fn ident_or_keyword(
    ) -> impl Parser<char, Spanned<String>, Error = LexErr> + Clone {
        Self::ident_chars().map_with_span(|ident, span| {
            Token::keyword(&ident)
                .map(|kw| Spanned::from_range(kw, span.clone()))
                .unwrap_or_else(|| {
                    Spanned::from_range(Token::Ident(ident), span)
                })
        })
    }

    fn operator_or_punct() -> impl Parser<char, Spanned<String>, Error = LexErr>
    {
        // Split into groups to avoid tuple size limits
        let two_char = choice((
            just("++").to(Token::Concat),
            just("**").to(Token::StarStar),
            just("//").to(Token::FloorDiv),
            just("==").to(Token::Eq),
            just("!=").to(Token::Ne),
            just("<=").to(Token::Le),
            just(">=").to(Token::Ge),
            just("<<").to(Token::Shl),
            just(">>").to(Token::Shr),
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
            just("?[").to(Token::QuestionLBracket),
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
            just('&').to(Token::Amp),
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
struct TokenWithCol<S> {
    tok: Token<S>,
    span: Span,
    col: usize,
}

impl<S: Clone + PartialEq> TokenWithCol<S> {
    /// Computes column positions for each token.
    fn from_spanned(tokens: &[Spanned<S>]) -> Vec<Self> {
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
