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

// NOTE: We don't really care about the large error types; they are
// only relevant in program termination, not the happy-path interpreter, so
// we silence them here.
#![allow(clippy::result_large_err)]
#![allow(clippy::only_used_in_recursion)]

/// Marks a branch as unreachable due to static type checking.
///
/// Use instead of `unreachable!` when the type checker guarantees a constraint.
/// Provides consistent error messages if the "impossible" case is somehow reached.
///
/// # When to Use
///
/// Use `typechecked!` for code paths that:
/// - Cannot be reached if the static type checker is correct
/// - Previously had runtime type/arity checks that are now redundant
///
/// # When NOT to Use (Keep Runtime Checks)
///
/// Keep runtime checks and do NOT use this macro for:
/// - `AS` casts on `Storable` union (runtime narrowing)
/// - `READ` conversions (parsing can fail)
/// - Database operations returning `Storable` (need `IS`/`AS` for narrowing)
/// - Index bounds checks (not type-level)
/// - Division by zero (not type-level)
macro_rules! typechecked {
    ($op:expr, $constraint:expr) => {
        unreachable!(
            "type checker guarantees `{}` satisfies `{}`",
            $op, $constraint
        )
    };
}

/// Marks a branch as unreachable due to interpreter invariants.
///
/// Use for internal consistency guarantees that aren't type-level constraints.
/// E.g., "if we have a `StringId`, the string exists in the arena."
macro_rules! invariant {
    ($desc:expr) => {
        unreachable!("invariant violated: {}", $desc)
    };
}

mod ast;
mod env;
mod error;
mod intern;
mod interpreter;
mod io;
mod lexer;
mod parser;
mod primitives;
mod resolve;
mod span;
mod token;
mod typecheck;
mod value;

#[allow(unused_imports)]
pub(crate) use ast::{
    Ast, BinOp, Expr, ExprId, Literal, Stmt, StmtId, TypePattern, UnOp,
};
#[allow(unused_imports)]
pub(crate) use env::{Environment, PrimCtx, PrimFn, PrimResult, Scopes};
pub use error::Error;
#[allow(unused_imports)]
pub(crate) use error::ErrorDisplay;
pub(crate) use error::Result;
#[allow(unused_imports)]
pub(crate) use intern::{StringId, StringInterner};
#[allow(unused_imports)]
pub(crate) use interpreter::Interpreter;
pub(crate) use io::{Io, IoContext, TestIo};
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
    TypeExprArena, TypeExprId, TypeId, TypeRegistry, Value, ValueArena, ValueId,
};

/// Run a RUMPS script, outputting to stdout.
pub async fn run(src: &str, db: Database) -> Result<()> {
    run_with_io(src, db, Io).await.map(|_| ())
}

/// Run a RUMPS script with a custom I/O context.
///
/// Parses the source, interprets it against the given database, and uses the
/// provided I/O context for output operations.
async fn run_with_io<I: IoContext>(
    src: &str,
    db: Database,
    io: I,
) -> Result<I> {
    let mut result = Parser::parse(src)?;
    let interp = Interpreter::new(&mut result.ast, &result.stmts, db, io)?;
    let interp = interp.run(&result.stmts).await?;
    Ok(interp.into_io())
}

/// Run a RUMPS script and capture output to a string.
///
/// Useful for testing; returns the captured stdout as a `String`.
pub async fn run_capturing(src: &str, db: Database) -> Result<String> {
    let io = run_with_io(src, db, TestIo::new()).await?;
    Ok(io.stdout_str().to_owned())
}
