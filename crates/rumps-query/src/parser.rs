//! Parser for the RUMPS query language.
//!
//! # Architecture
//!
//! The parser uses a two-pass approach to decouple parsing from AST construction:
//!
//! ```text
//! Tokens  -->  CST (Concrete Syntax Tree)  -->  AST (arena-allocated)
//!              ^^^^^^^^^^^^^^^^^^^^^^^^^^       ^^^^^^^^^^^^^^^^^^^^^
//!              chumsky produces (pass 1)        lowering produces (pass 2)
//! ```
//!
//! This design eliminates the `Rc<RefCell<Ast>>` pattern previously required
//! by chumsky's `Clone` constraint on parsers. See `cst.rs` for details.
//!
//! # Whitespace Handling
//!
//! The lexer emits `Newline`, `Indent`, and `Dedent` tokens to track source
//! structure. Rather than filtering these in the lexer, the parser handles them
//! explicitly via `opt_newlines()`. This preserves all tokens in the stream for:
//!
//! - **Formatters**: A future formatter needs the original whitespace structure
//! - **Source maps**: Accurate span information for error messages
//! - **Round-tripping**: Parse then re-emit without losing formatting
//!
//! The parser allows optional newlines (and indent/dedent) in these contexts:
//!
//! - **Binary operators**: Before and after operators for expression continuation
//!   (`1\n    + 2` parses as `1 + 2`)
//! - **Delimited constructs**: Inside `[]`, `()`, and `{}` for multi-line arrays,
//!   function calls, objects, and blocks
//!
//! # String Interning in Parsers
//!
//! Parser-building functions receive `&mut StringInterner` and intern contextual
//! keywords (e.g. `"where"`, `"for"`, `"json"`) locally at their point of use.
//! This is necessary because chumsky's `Clone` constraint on parsers means
//! closures cannot capture `&mut StringInterner`; they must capture the
//! resulting `StringId` values (which are `Copy`) instead. Interning happens
//! once during parser construction, not during parsing.
//!
//! # Module Organization
//!
//! - `common`: Whitespace handling, identifiers, subscripts, blocks
//! - `types`: Type expression and pattern parsing
//! - `pattern`: Binding and match patterns
//! - `postfix`: Postfix operations (field access, indexing, calls)
//! - `expr`: Expression parsing with precedence
//! - `stmt`: Statement parsing
//! - `cst`: Concrete syntax tree definitions
//! - `lower`: CST to AST lowering

#![allow(dead_code)]
// NOTE: This is because `ParseErr = Simple<Token, Span>`, which can be quite
// large. Boxing it would infect the entire parser. This is only for errors,
// which are not the happy path, so I'm not too concerned about size here.
// It's also a warning for 136 bytes, which is not _that_ large and anyway
// `Box`ing would add allocation overhead
#![allow(clippy::result_large_err)]

use std::path::Path;

use chumsky::prelude::{end, Simple};
use chumsky::Parser as _;
use nonempty::NonEmpty;

mod common;
mod cst;
mod expr;
mod lower;
mod pattern;
mod postfix;
mod stmt;
mod types;

use crate::lexer::Spanned;
use crate::{Ast, Error, Lexer, Result, Span, StmtId, StringInterner, Token};

/// Parser error type for token-based parsing.
type ParseErr = Simple<Token, Span>;

/// The result of parsing: the AST arena and top-level statement IDs.
#[derive(Debug)]
pub(crate) struct ParseResult {
    pub ast: Ast,
    pub stmts: Vec<StmtId>,
}

/// Parses source code into an AST.
pub(crate) struct Parser;

impl Parser {
    /// Parse source code into an AST.
    pub(crate) fn parse(
        src: &str,
        interner: &mut StringInterner,
    ) -> Result<ParseResult> {
        let raw = Lexer::new(src).lex()?;
        let tokens = Spanned::intern_all(raw, interner);
        Self::parse_interned(&tokens, None, interner)
    }

    /// Parse source code with a source file path for resolving relative imports.
    pub(crate) fn parse_with_path(
        src: &str,
        src_path: &Path,
        interner: &mut StringInterner,
    ) -> Result<ParseResult> {
        let raw = Lexer::new(src).lex()?;
        let tokens = Spanned::intern_all(raw, interner);
        Self::parse_interned(&tokens, Some(src_path), interner)
    }

    /// Parse a raw (pre-interning) token stream into an AST.
    pub(crate) fn parse_tokens(
        tokens: Vec<Spanned<String>>,
        interner: &mut StringInterner,
    ) -> Result<ParseResult> {
        let interned = Spanned::intern_all(tokens, interner);
        Self::parse_interned(&interned, None, interner)
    }

    /// Core parse function: takes interned tokens, produces a `ParseResult`.
    fn parse_interned(
        tokens: &[Spanned],
        src_path: Option<&Path>,
        interner: &mut StringInterner,
    ) -> Result<ParseResult> {
        let parser = Self::program(interner);

        let eof_span = tokens
            .iter()
            .find_map(|s| matches!(s.tok, Token::Eof).then_some(s.span))
            .unwrap_or_default();

        let stream = chumsky::Stream::from_iter(
            eof_span,
            tokens
                .iter()
                .filter(|s| !matches!(s.tok, Token::Eof))
                .map(|s| (s.tok.clone(), s.span)),
        );

        parser
            .parse(stream)
            .map_err(|errs| {
                NonEmpty::collect(
                    errs.into_iter()
                        .map(|e| Error::from_parse_rich(e, interner)),
                )
                .map(Error::multiple)
                .unwrap_or_else(|| {
                    Error::runtime_no_span("unknown parse error")
                })
            })
            .and_then(|cst_stmts| {
                let (ast, stmts) = lower::LowerCtx::program_with_path(
                    cst_stmts, src_path, interner,
                )?;
                Ok(ParseResult { ast, stmts })
            })
    }

    /// Parse a raw token stream into CST (without lowering to AST).
    ///
    /// Used by `lower_module_from_file` to parse imported module files
    /// with the calling module's context for relative path resolution.
    pub(super) fn parse_to_cst(
        tokens: Vec<Spanned<String>>,
        interner: &mut StringInterner,
    ) -> Result<Vec<cst::Stmt>> {
        let interned = Spanned::intern_all(tokens, interner);
        let parser = Self::program(interner);

        let eof_span = interned
            .iter()
            .find_map(|s| matches!(s.tok, Token::Eof).then_some(s.span))
            .unwrap_or_default();

        let stream = chumsky::Stream::from_iter(
            eof_span,
            interned
                .iter()
                .filter(|s| !matches!(s.tok, Token::Eof))
                .map(|s| (s.tok.clone(), s.span)),
        );

        parser.parse(stream).map_err(|errs| {
            NonEmpty::collect(
                errs.into_iter()
                    .map(|e| Error::from_parse_rich(e, interner)),
            )
            .map(Error::multiple)
            .unwrap_or_else(|| Error::runtime_no_span("unknown parse error"))
        })
    }

    /// Program: zero or more statements separated by newlines or commas.
    fn program(
        interner: &mut StringInterner,
    ) -> impl chumsky::Parser<Token, Vec<cst::Stmt>, Error = ParseErr> {
        Self::opt_newlines()
            .ignore_then(
                Self::stmt(interner)
                    .separated_by(Self::item_sep())
                    .allow_trailing(),
            )
            .then_ignore(Self::opt_newlines())
            .then_ignore(end())
    }
}
