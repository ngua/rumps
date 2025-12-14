//! RUMPS Query Language
//!
//! Lexing, parsing, and interpretation for the RUMPS query DSL.

mod error;
mod span;

// Future modules (Phase 1 continued):
// mod token;
// mod lexer;
// mod ast;
// mod parser;
// mod value;
// mod env;
// mod interpreter;

#[allow(unused_imports)]
pub(crate) use error::{Error, ErrorDisplay};
#[allow(unused_imports)]
pub(crate) use span::Span;
