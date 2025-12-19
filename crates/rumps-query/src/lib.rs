//! RUMPS Query Language
//!
//! Lexing, parsing, and interpretation for the RUMPS query DSL.
//!
//! # Example
//!
//! ```ignore
//! use rumps_query::run;
//! use rumps_storage::Database;
//!
//! let db = Database::in_memory()?;
//! run("OUTPUT 1 + 2", db).await?;
//! // Prints: 3
//! ```

mod ast;
mod env;
mod error;
mod interpreter;
mod io;
mod lexer;
mod parser;
mod span;
mod token;
mod value;

#[allow(unused_imports)]
pub(crate) use ast::{
    Ast, BinOp, Expr, ExprId, Literal, Stmt, StmtId, TypePattern, UnOp,
};
#[allow(unused_imports)]
pub(crate) use env::{Environment, PrimCtx, PrimFn, PrimResult, Scopes};
#[allow(unused_imports)]
pub(crate) use error::ErrorDisplay;
pub use error::{Error, Result};
#[allow(unused_imports)]
pub(crate) use interpreter::Interpreter;
pub use io::{Io, IoContext, TestIo};
#[allow(unused_imports)]
pub(crate) use lexer::{Lexer, Spanned};
#[allow(unused_imports)]
pub(crate) use parser::{ParseResult, Parser};
use rumps_storage::Database;
pub use span::Span;
#[allow(unused_imports)]
pub(crate) use token::Token;
#[allow(unused_imports)]
pub(crate) use value::{
    StringId, TypeExprArena, TypeExprId, TypeId, TypeRegistry, Value,
    ValueArena, ValueId,
};

/// Run a RUMPS script with a custom I/O context.
///
/// Parses the source, interprets it against the given database, and uses the
/// provided I/O context for output operations.
pub async fn run_with_io<I: IoContext>(
    src: &str,
    db: Database,
    io: I,
) -> Result<I> {
    let result = Parser::parse(src)?;
    let interp = Interpreter::new(&result.ast, db, io)?;
    let interp = interp.run(&result.stmts).await?;
    Ok(interp.into_io())
}

/// Run a RUMPS script, outputting to stdout.
pub async fn run(src: &str, db: Database) -> Result<()> {
    run_with_io(src, db, Io).await.map(|_| ())
}

/// Run a RUMPS script and capture output to a string.
///
/// Useful for testing; returns the captured stdout as a `String`.
pub async fn run_capturing(src: &str, db: Database) -> Result<String> {
    let io = run_with_io(src, db, TestIo::new()).await?;
    Ok(io.stdout_str().to_owned())
}
