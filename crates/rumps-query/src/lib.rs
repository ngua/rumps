//! RUMPS Query Language
//!
//! Lexing, parsing, and interpretation for the RUMPS query DSL.

mod ast;
mod env;
mod error;
mod lexer;
mod parser;
mod span;
mod token;
mod value;

// Future modules (Phase 1 continued):
// mod interpreter;

#[allow(unused_imports)]
pub(crate) use ast::{Ast, BinOp, Expr, ExprId, Literal, Stmt, StmtId, UnOp};
#[allow(unused_imports)]
pub(crate) use env::{Environment, PrimCtx, PrimFn, PrimResult, Scopes};
#[allow(unused_imports)]
pub(crate) use error::{Error, ErrorDisplay, Result};
#[allow(unused_imports)]
pub(crate) use lexer::{Lexer, Spanned};
#[allow(unused_imports)]
pub(crate) use parser::{ParseResult, Parser};
#[allow(unused_imports)]
pub(crate) use span::Span;
#[allow(unused_imports)]
pub(crate) use token::Token;
#[allow(unused_imports)]
pub(crate) use value::{
    StringId, TypeExprArena, TypeExprId, TypeId, TypeRegistry, Value,
    ValueArena, ValueId,
};
