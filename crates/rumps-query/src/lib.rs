//! RUMPS Query Language
//!
//! Lexing, parsing, and interpretation for the RUMPS query DSL.

mod ast;
mod error;
mod lexer;
mod span;
mod token;

// Future modules (Phase 1 continued):
// mod parser;
// mod value;
// mod env;
// mod interpreter;

#[allow(unused_imports)]
pub(crate) use ast::{Ast, BinOp, Expr, ExprId, Literal, Stmt, StmtId, UnOp};
#[allow(unused_imports)]
pub(crate) use error::{Error, ErrorDisplay};
#[allow(unused_imports)]
pub(crate) use lexer::{Lexer, Spanned};
#[allow(unused_imports)]
pub(crate) use span::Span;
#[allow(unused_imports)]
pub(crate) use token::Token;
