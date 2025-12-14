//! Lexer for the RUMPS query language.
//!
//! Converts source text into a stream of tokens with spans. Handles:
//! - Train-case identifiers (`my-var`) vs spaced subtraction (`a - b`)
//! - Case-insensitive keywords
//! - Comments (`;` to end of line)
//! - Indentation tracking (`Indent`/`Dedent` tokens)
//! - String literals with escape sequences

#![allow(dead_code)]

use crate::{Error, Span, Token};

/// A token with its source span.
pub(crate) type Spanned = (Token, Span);

/// Lexes source code into a stream of spanned tokens.
///
/// Returns either a vector of tokens or a vector of lex errors.
pub(crate) fn lex(src: &str) -> Result<Vec<Spanned>, Vec<Error>> {
    let state = LexState::new(src);
    let (tokens, errs) = state.lex_all();

    if errs.is_empty() {
        Ok(process_indentation(tokens))
    } else {
        Err(errs)
    }
}

/// Internal lexer state.
struct LexState<'a> {
    src: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    pos: usize,
}

impl<'a> LexState<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            chars: src.char_indices().peekable(),
            pos: 0,
        }
    }

    /// Lexes all tokens from source.
    fn lex_all(mut self) -> (Vec<Spanned>, Vec<Error>) {
        let mut tokens = Vec::new();
        let mut errs = Vec::new();

        // Use recursion via a helper that processes one token at a time
        self.lex_loop(&mut tokens, &mut errs);

        // Add EOF token
        tokens.push((Token::Eof, Span::point(self.pos as u32)));

        (tokens, errs)
    }

    /// Recursive helper to lex tokens (avoids explicit loop).
    fn lex_loop(&mut self, tokens: &mut Vec<Spanned>, errs: &mut Vec<Error>) {
        // Skip horizontal whitespace, preserving it for train-case detection
        self.skip_horizontal_ws();

        match self.peek() {
            None => {} // Done
            Some((_, c)) => {
                // Handle comment
                if c == ';' {
                    self.skip_comment();
                    self.lex_loop(tokens, errs);
                }
                // Handle newline
                else if c == '\n' {
                    let start = self.pos;
                    self.advance();
                    tokens.push((
                        Token::Newline,
                        Span::new(start as u32, self.pos as u32),
                    ));
                    self.lex_loop(tokens, errs);
                }
                // Handle tokens
                else {
                    match self.lex_token() {
                        Ok(Some(tok)) => {
                            tokens.push(tok);
                            self.lex_loop(tokens, errs);
                        }
                        Ok(None) => self.lex_loop(tokens, errs),
                        Err(e) => {
                            errs.push(e);
                            // Skip the problematic character and continue
                            self.advance();
                            self.lex_loop(tokens, errs);
                        }
                    }
                }
            }
        }
    }

    /// Lexes a single token.
    fn lex_token(&mut self) -> Result<Option<Spanned>, Error> {
        self.skip_horizontal_ws();

        match self.peek() {
            None => Ok(None),
            Some((start, c)) => {
                // String literal
                if c == '"' {
                    self.lex_string().map(Some)
                }
                // Number (or negative number)
                else if c.is_ascii_digit() {
                    self.lex_number().map(Some)
                }
                // Global variable
                else if c == '^' {
                    self.lex_global().map(Some)
                }
                // Identifier or keyword
                else if is_ident_start(c) {
                    Ok(Some(self.lex_ident_or_keyword()))
                }
                // Operators and punctuation
                else {
                    self.lex_operator_or_punct(start, c)
                }
            }
        }
    }

    /// Lexes a string literal with escape sequences.
    fn lex_string(&mut self) -> Result<Spanned, Error> {
        let start = self.pos;
        self.advance(); // consume opening "

        let mut s = String::new();

        self.lex_string_contents(&mut s, start)
    }

    /// Recursive helper for string contents.
    fn lex_string_contents(
        &mut self,
        s: &mut String,
        start: usize,
    ) -> Result<Spanned, Error> {
        match self.peek() {
            None => Err(Error::lex(
                Span::new(start as u32, self.pos as u32),
                "unterminated string literal",
            )),
            Some((_, '"')) => {
                self.advance(); // consume closing "
                Ok((
                    Token::String(s.clone()),
                    Span::new(start as u32, self.pos as u32),
                ))
            }
            Some((_, '\n')) => Err(Error::lex(
                Span::new(start as u32, self.pos as u32),
                "unterminated string literal (newline in string)",
            )),
            Some((_, '\\')) => {
                self.advance(); // consume backslash
                match self.peek() {
                    None => Err(Error::lex(
                        Span::new(start as u32, self.pos as u32),
                        "unterminated escape sequence",
                    )),
                    Some((esc_pos, esc_c)) => {
                        let escaped = match esc_c {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            '\\' => '\\',
                            '"' => '"',
                            '0' => '\0',
                            _ => {
                                let span = Span::new(
                                    esc_pos as u32,
                                    (esc_pos + 1) as u32,
                                );
                                Err(Error::lex(
                                    span,
                                    format!(
                                        "unknown escape sequence `\\{esc_c}`"
                                    ),
                                ))?
                            }
                        };
                        self.advance();
                        s.push(escaped);
                        self.lex_string_contents(s, start)
                    }
                }
            }
            Some((_, c)) => {
                self.advance();
                s.push(c);
                self.lex_string_contents(s, start)
            }
        }
    }

    /// Lexes a number (integer or float).
    fn lex_number(&mut self) -> Result<Spanned, Error> {
        let start = self.pos;
        let num_str = self.take_while(|c| c.is_ascii_digit());

        // Check for decimal point
        match self.peek() {
            Some((_, '.')) => {
                // Look ahead to distinguish `1.2` from `1..2`
                let after_dot = self.peek_nth(1);
                match after_dot {
                    Some(c) if c.is_ascii_digit() => {
                        self.advance(); // consume '.'
                        let frac = self.take_while(|c| c.is_ascii_digit());

                        // Check for exponent
                        let full = match self.peek() {
                            Some((_, 'e' | 'E')) => {
                                self.advance();
                                let exp_sign = match self.peek() {
                                    Some((_, '+' | '-')) => {
                                        let c = self
                                            .peek()
                                            .map(|(_, c)| c)
                                            .unwrap_or('+');
                                        self.advance();
                                        if c == '-' {
                                            "-"
                                        } else {
                                            ""
                                        }
                                    }
                                    _ => "",
                                };
                                let exp =
                                    self.take_while(|c| c.is_ascii_digit());
                                format!("{num_str}.{frac}e{exp_sign}{exp}")
                            }
                            _ => format!("{num_str}.{frac}"),
                        };

                        full.parse::<f64>()
                            .map(|f| {
                                (
                                    Token::Float(f),
                                    Span::new(start as u32, self.pos as u32),
                                )
                            })
                            .map_err(|_| {
                                Error::lex(
                                    Span::new(start as u32, self.pos as u32),
                                    format!("invalid float literal `{full}`"),
                                )
                            })
                    }
                    _ => {
                        // It's `1..` range or just `1.field`; parse as int
                        num_str
                            .parse::<i64>()
                            .map(|n| {
                                (
                                    Token::Int(n),
                                    Span::new(start as u32, self.pos as u32),
                                )
                            })
                            .map_err(|_| {
                                Error::lex(
                                    Span::new(start as u32, self.pos as u32),
                                    format!(
                                        "invalid integer literal `{num_str}`"
                                    ),
                                )
                            })
                    }
                }
            }
            Some((_, 'e' | 'E')) => {
                // Integer with exponent (scientific notation without decimal)
                self.advance();
                let exp_sign = match self.peek() {
                    Some((_, '+' | '-')) => {
                        let c = self.peek().map(|(_, c)| c).unwrap_or('+');
                        self.advance();
                        if c == '-' {
                            "-"
                        } else {
                            ""
                        }
                    }
                    _ => "",
                };
                let exp = self.take_while(|c| c.is_ascii_digit());
                let full = format!("{num_str}e{exp_sign}{exp}");

                full.parse::<f64>()
                    .map(|f| {
                        (
                            Token::Float(f),
                            Span::new(start as u32, self.pos as u32),
                        )
                    })
                    .map_err(|_| {
                        Error::lex(
                            Span::new(start as u32, self.pos as u32),
                            format!("invalid float literal `{full}`"),
                        )
                    })
            }
            _ => {
                // Plain integer
                num_str
                    .parse::<i64>()
                    .map(|n| {
                        (
                            Token::Int(n),
                            Span::new(start as u32, self.pos as u32),
                        )
                    })
                    .map_err(|_| {
                        Error::lex(
                            Span::new(start as u32, self.pos as u32),
                            format!("invalid integer literal `{num_str}`"),
                        )
                    })
            }
        }
    }

    /// Lexes a global variable (`^NAME`).
    fn lex_global(&mut self) -> Result<Spanned, Error> {
        let start = self.pos;
        self.advance(); // consume '^'

        match self.peek() {
            Some((_, c)) if is_ident_start(c) => {
                let name = self.take_ident();
                Ok((
                    Token::Global(name),
                    Span::new(start as u32, self.pos as u32),
                ))
            }
            _ => Err(Error::lex(
                Span::new(start as u32, self.pos as u32),
                "expected identifier after `^`",
            )),
        }
    }

    /// Lexes an identifier or keyword (including train-case).
    fn lex_ident_or_keyword(&mut self) -> Spanned {
        let start = self.pos;
        let ident = self.take_ident();
        let span = Span::new(start as u32, self.pos as u32);

        // Check if it's a keyword (case-insensitive)
        Token::keyword(&ident)
            .map(|kw| (kw, span))
            .unwrap_or_else(|| (Token::Ident(ident), span))
    }

    /// Takes an identifier, handling train-case.
    fn take_ident(&mut self) -> String {
        let mut ident = String::new();
        self.take_ident_into(&mut ident);
        ident
    }

    /// Recursive helper for taking identifier characters.
    fn take_ident_into(&mut self, ident: &mut String) {
        // Take alphanumeric and underscore chars
        let part = self.take_while(|c| c.is_alphanumeric() || c == '_');
        ident.push_str(&part);

        // Check for train-case continuation: `-` followed immediately by letter
        if let Some((_, '-')) = self.peek() {
            // Look ahead: is next char a letter?
            self.peek_nth(1).filter(|c| c.is_alphabetic()).map(|_| {
                ident.push('-');
                self.advance(); // consume '-'
                self.take_ident_into(ident);
            });
        }
    }

    /// Lexes operators and punctuation.
    fn lex_operator_or_punct(
        &mut self,
        start: usize,
        c: char,
    ) -> Result<Option<Spanned>, Error> {
        // Helper to create a span from start to current pos
        let span = |end: usize| Span::new(start as u32, end as u32);

        match c {
            // Two-char operators that start with these
            '+' => {
                self.advance();
                match self.peek() {
                    Some((_, '+')) => {
                        self.advance();
                        Ok(Some((Token::Concat, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Plus, span(self.pos)))),
                }
            }
            '-' => {
                self.advance();
                // Check if this is subtraction or could be negative number
                // Rule: `-` with space before and letter/digit after = subtraction
                // We only tokenize `-` as Minus here; numbers handle their own sign
                Ok(Some((Token::Minus, span(self.pos))))
            }
            '*' => {
                self.advance();
                Ok(Some((Token::Mul, span(self.pos))))
            }
            '/' => {
                self.advance();
                match self.peek() {
                    Some((_, '/')) => {
                        self.advance();
                        Ok(Some((Token::FloorDiv, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Div, span(self.pos)))),
                }
            }
            '%' => {
                self.advance();
                Ok(Some((Token::Modulo, span(self.pos))))
            }
            '=' => {
                self.advance();
                match self.peek() {
                    Some((_, '=')) => {
                        self.advance();
                        Ok(Some((Token::Eq, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Assign, span(self.pos)))),
                }
            }
            '!' => {
                self.advance();
                match self.peek() {
                    Some((_, '=')) => {
                        self.advance();
                        Ok(Some((Token::Ne, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Bang, span(self.pos)))),
                }
            }
            '<' => {
                self.advance();
                match self.peek() {
                    Some((_, '=')) => {
                        self.advance();
                        Ok(Some((Token::Le, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Lt, span(self.pos)))),
                }
            }
            '>' => {
                self.advance();
                match self.peek() {
                    Some((_, '=')) => {
                        self.advance();
                        Ok(Some((Token::Ge, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Gt, span(self.pos)))),
                }
            }
            '&' => {
                self.advance();
                match self.peek() {
                    Some((_, '&')) => {
                        self.advance();
                        Ok(Some((Token::AmpAmp, span(self.pos))))
                    }
                    _ => Err(Error::lex(
                        span(self.pos),
                        "expected `&&`, found single `&`",
                    )),
                }
            }
            '|' => {
                self.advance();
                match self.peek() {
                    Some((_, '|')) => {
                        self.advance();
                        Ok(Some((Token::PipePipe, span(self.pos))))
                    }
                    _ => Err(Error::lex(
                        span(self.pos),
                        "expected `||`, found single `|`",
                    )),
                }
            }
            '.' => {
                self.advance();
                match self.peek() {
                    Some((_, '.')) => {
                        self.advance();
                        Ok(Some((Token::DotDot, span(self.pos))))
                    }
                    _ => Ok(Some((Token::Dot, span(self.pos)))),
                }
            }
            // Single-char punctuation
            '(' => {
                self.advance();
                Ok(Some((Token::LParen, span(self.pos))))
            }
            ')' => {
                self.advance();
                Ok(Some((Token::RParen, span(self.pos))))
            }
            '{' => {
                self.advance();
                Ok(Some((Token::LBrace, span(self.pos))))
            }
            '}' => {
                self.advance();
                Ok(Some((Token::RBrace, span(self.pos))))
            }
            '[' => {
                self.advance();
                Ok(Some((Token::LBracket, span(self.pos))))
            }
            ']' => {
                self.advance();
                Ok(Some((Token::RBracket, span(self.pos))))
            }
            ',' => {
                self.advance();
                Ok(Some((Token::Comma, span(self.pos))))
            }
            ':' => {
                self.advance();
                Ok(Some((Token::Colon, span(self.pos))))
            }
            // Unknown character
            _ => Err(Error::lex(
                Span::point(start as u32),
                format!("unexpected character `{c}`"),
            )),
        }
    }

    /// Peeks at the current character without consuming.
    fn peek(&mut self) -> Option<(usize, char)> {
        self.chars.peek().copied()
    }

    /// Peeks at the nth character ahead (0 = current).
    fn peek_nth(&self, n: usize) -> Option<char> {
        self.src.get(self.pos..)?.chars().nth(n)
    }

    /// Advances to the next character.
    fn advance(&mut self) -> Option<(usize, char)> {
        self.chars.next().map(|(i, c)| {
            self.pos = i + c.len_utf8();
            (i, c)
        })
    }

    /// Skips horizontal whitespace (space, tab), but not newlines.
    fn skip_horizontal_ws(&mut self) {
        if let Some((_, ' ' | '\t' | '\r')) = self.peek() {
            self.advance();
            self.skip_horizontal_ws();
        }
    }

    /// Skips a comment from `;` to end of line.
    fn skip_comment(&mut self) {
        match self.peek() {
            None | Some((_, '\n')) => {}
            Some(_) => {
                self.advance();
                self.skip_comment();
            }
        }
    }

    /// Takes characters while predicate is true.
    fn take_while<F: Fn(char) -> bool>(&mut self, pred: F) -> String {
        let mut s = String::new();
        self.take_while_into(&mut s, pred);
        s
    }

    /// Recursive helper for take_while.
    fn take_while_into<F: Fn(char) -> bool>(
        &mut self,
        s: &mut String,
        pred: F,
    ) {
        match self.peek() {
            Some((_, c)) if pred(c) => {
                self.advance();
                s.push(c);
                self.take_while_into(s, pred);
            }
            _ => {}
        }
    }
}

/// Checks if a character can start an identifier.
fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

/// Post-processes tokens to add `Indent` and `Dedent` tokens.
///
/// Tracks indentation levels and emits `Indent` when indentation increases,
/// `Dedent` when it decreases. Indentation is measured as the column position
/// (bytes from start of line) of the first token on each line.
fn process_indentation(tokens: Vec<Spanned>) -> Vec<Spanned> {
    // First, compute the column position for each token
    let with_cols = compute_columns(&tokens);

    let mut result = Vec::with_capacity(tokens.len());
    let mut indent_stack: Vec<usize> = vec![0]; // Start at column 0

    process_indent_loop(&with_cols, 0, &mut result, &mut indent_stack);

    // Emit remaining dedents at EOF
    emit_final_dedents(&mut result, &indent_stack);

    result
}

/// Token with its column position (bytes from start of line).
type TokenWithCol = (Token, Span, usize);

/// Computes column positions for each token.
fn compute_columns(tokens: &[Spanned]) -> Vec<TokenWithCol> {
    let mut result = Vec::with_capacity(tokens.len());
    let mut line_start: u32 = 0;

    compute_columns_loop(tokens, 0, line_start, &mut result, &mut line_start);

    result
}

fn compute_columns_loop(
    tokens: &[Spanned],
    idx: usize,
    line_start: u32,
    result: &mut Vec<TokenWithCol>,
    next_line_start: &mut u32,
) {
    tokens.get(idx).map(|(tok, span)| {
        let col = (span.start - line_start) as usize;
        result.push((tok.clone(), *span, col));

        // If this is a newline, the next line starts after this token
        let new_line_start = match tok {
            Token::Newline => span.end,
            _ => line_start,
        };

        *next_line_start = new_line_start;
        compute_columns_loop(
            tokens,
            idx + 1,
            new_line_start,
            result,
            next_line_start,
        );
    });
}

/// Recursive helper for indentation processing.
fn process_indent_loop(
    tokens: &[TokenWithCol],
    idx: usize,
    result: &mut Vec<Spanned>,
    indent_stack: &mut Vec<usize>,
) {
    tokens.get(idx).map(|(tok, span, _col)| {
        match tok {
            Token::Newline => {
                result.push((tok.clone(), *span));

                // Measure indentation of next non-newline token
                let (next_idx, indent) = measure_next_indent(tokens, idx + 1);

                // Get current indent level
                let current = indent_stack.last().copied().unwrap_or(0);

                if indent > current {
                    // Increased indentation
                    indent_stack.push(indent);
                    result.push((Token::Indent, *span));
                } else {
                    // Decreased or same indentation; emit dedents as needed
                    emit_dedents_to(result, indent_stack, indent, *span);
                }

                process_indent_loop(tokens, next_idx, result, indent_stack);
            }
            Token::Eof => {
                result.push((tok.clone(), *span));
            }
            _ => {
                result.push((tok.clone(), *span));
                process_indent_loop(tokens, idx + 1, result, indent_stack);
            }
        }
    });
}

/// Measures indentation level of the next significant token.
/// Returns (next_idx, indent_column).
fn measure_next_indent(
    tokens: &[TokenWithCol],
    start_idx: usize,
) -> (usize, usize) {
    measure_indent_loop(tokens, start_idx)
}

fn measure_indent_loop(tokens: &[TokenWithCol], idx: usize) -> (usize, usize) {
    match tokens.get(idx) {
        None => (idx, 0),
        Some((Token::Newline, _, _)) => {
            // Another newline; continue looking
            measure_indent_loop(tokens, idx + 1)
        }
        Some((Token::Eof, _, _)) => (idx, 0),
        Some((_, _, col)) => {
            // Found a real token; its column is the indent
            (idx, *col)
        }
    }
}

/// Emits `Dedent` tokens to return to target indent level.
fn emit_dedents_to(
    result: &mut Vec<Spanned>,
    indent_stack: &mut Vec<usize>,
    target: usize,
    span: Span,
) {
    match indent_stack.last() {
        Some(&level) if level > target => {
            indent_stack.pop();
            result.push((Token::Dedent, span));
            emit_dedents_to(result, indent_stack, target, span);
        }
        _ => {}
    }
}

/// Emits final dedents at EOF.
fn emit_final_dedents(result: &mut Vec<Spanned>, indent_stack: &[usize]) {
    // Find the EOF token position
    let eof_span = result
        .iter()
        .rev()
        .find_map(|(t, s)| matches!(t, Token::Eof).then_some(*s))
        .unwrap_or_default();

    // Remove EOF, emit dedents, re-add EOF
    let eof = result.pop();

    emit_final_dedents_loop(
        result,
        indent_stack.len().saturating_sub(1),
        eof_span,
    );

    eof.map(|e| result.push(e));
}

fn emit_final_dedents_loop(
    result: &mut Vec<Spanned>,
    remaining: usize,
    span: Span,
) {
    (remaining > 0).then(|| {
        result.push((Token::Dedent, span));
        emit_final_dedents_loop(result, remaining - 1, span);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_ok(src: &str) -> Vec<Token> {
        lex(src)
            .expect("lex should succeed")
            .into_iter()
            .map(|(t, _)| t)
            .collect()
    }

    fn lex_err(src: &str) -> Vec<Error> {
        lex(src).expect_err("lex should fail")
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
        assert_eq!(errs.len(), 1);
        assert!(errs[0].to_string().contains("unterminated"));
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
        let tokens = lex_ok("SET KILL OUTPUT IF ELSE AND OR NOT TRUE FALSE");
        assert_eq!(
            tokens,
            vec![
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
        // Should have Indent after newline due to increased indentation
        assert!(tokens.contains(&Token::Indent));
    }

    #[test]
    fn indentation_dedent() {
        let tokens = lex_ok("IF x\n  OUTPUT y\nSET z = 1");
        // Should have both Indent and Dedent
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
        // Just verify it lexes without error and contains expected tokens
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
        let result = lex("SET x").expect("should lex");
        assert_eq!(result[0], (Token::Set, Span::new(0, 3)));
        assert_eq!(result[1], (Token::Ident("x".into()), Span::new(4, 5)));
    }

    #[test]
    fn number_followed_by_range() {
        // `1..10` should lex as Int(1), DotDot, Int(10)
        let tokens = lex_ok("1..10");
        assert_eq!(
            tokens,
            vec![Token::Int(1), Token::DotDot, Token::Int(10), Token::Eof]
        );
    }

    #[test]
    fn unknown_char_error() {
        let errs = lex_err("SET x = @invalid");
        assert_eq!(errs.len(), 1);
        assert!(errs[0].to_string().contains("unexpected character"));
    }

    #[test]
    fn single_ampersand_error() {
        let errs = lex_err("a & b");
        assert!(!errs.is_empty());
        assert!(errs[0].to_string().contains("expected `&&`"));
    }

    #[test]
    fn single_pipe_error() {
        let errs = lex_err("a | b");
        assert!(!errs.is_empty());
        assert!(errs[0].to_string().contains("expected `||`"));
    }
}
