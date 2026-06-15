//! RUMPS Query Language
//!
//! Lexing, parsing, and interpretation for the RUMPS query DSL.
//!
//! # Example
//!
//! ```ignore
//! use std::path::Path;
//! use rumps_query::run;
//! use rumps_storage::Database;
//!
//! let db = Database::in_memory()?;
//! run("OUTPUT 1 + 2", Path::new("/dev/stdin"), db).await?;
//! // Prints: 3
//! ```

// NOTE: We don't really care about the large error types; they are
// only relevant in program termination, not the happy-path interpreter, so
// we silence them here.
#![allow(clippy::result_large_err)]
#![allow(clippy::only_used_in_recursion)]
#![cfg_attr(
    test,
    allow(clippy::approx_constant, clippy::expect_used, clippy::unwrap_used)
)]

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
/// - `as` casts on `Storable` union (runtime narrowing)
/// - `read` conversions (parsing can fail)
/// - Database operations returning `Storable` (need `is`/`as` for narrowing)
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

use std::path::Path;
use std::time::Duration;

use ast::pragma;
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
use rumps_storage::{Database, SyncMode};
pub use span::Span;
#[allow(unused_imports)]
pub(crate) use token::Token;
#[allow(unused_imports)]
pub(crate) use value::{
    ClassId, Payload, TypeId, TypeRegistry, Value, ValueArena, ValueId,
    ValueMeta,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DbSyncMode {
    Immediate,
    OnCommit,
    Periodic { interval_ms: u64 },
    Relaxed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DbOptions {
    cache_size: Option<usize>,
    sync_mode: Option<DbSyncMode>,
    wal_max_file_size: Option<u64>,
}

impl From<&pragma::Program> for DbOptions {
    fn from(p: &pragma::Program) -> Self {
        p.db_options.iter().fold(Self::default(), |mut opts, opt| {
            match opt {
                pragma::DbOption::CacheSize { value, .. } => {
                    opts.cache_size = Some(*value);
                }
                pragma::DbOption::SyncMode { value, .. } => {
                    opts.sync_mode = Some(DbSyncMode::from(*value));
                }
                pragma::DbOption::WalMaxFileSize { value, .. } => {
                    opts.wal_max_file_size = Some(*value);
                }
            }

            opts
        })
    }
}

impl From<pragma::Program> for DbOptions {
    fn from(p: pragma::Program) -> Self {
        Self::from(&p)
    }
}

impl From<pragma::SyncMode> for DbSyncMode {
    fn from(m: pragma::SyncMode) -> Self {
        match m {
            pragma::SyncMode::Immediate => Self::Immediate,
            pragma::SyncMode::OnCommit => Self::OnCommit,
            pragma::SyncMode::Periodic { interval_ms } => {
                Self::Periodic { interval_ms }
            }
            pragma::SyncMode::Relaxed => Self::Relaxed,
        }
    }
}

impl DbSyncMode {
    fn storage(self) -> SyncMode {
        match self {
            Self::Immediate => SyncMode::Immediate,
            Self::OnCommit => SyncMode::OnCommit,
            Self::Periodic { interval_ms } => {
                SyncMode::Periodic(Duration::from_millis(interval_ms))
            }
            Self::Relaxed => SyncMode::Relaxed,
        }
    }
}

impl DbOptions {
    pub(crate) async fn open(self, path: impl AsRef<Path>) -> Result<Database> {
        let db = Database::open_override(path);
        let db = match self.cache_size {
            Some(size) => db.cache_size(size),
            None => db,
        };
        let db = match self.sync_mode {
            Some(mode) => db.sync_mode(mode.storage()),
            None => db,
        };
        let db = match self.wal_max_file_size {
            Some(size) => db.wal_max_file_size(size),
            None => db,
        };

        // Convert storage errors at the query boundary so callers only handle
        // `rumps-query` errors.
        db.open()
            .await
            .map_err(|e| Error::runtime_no_span(e.to_string()))
    }

    pub(crate) async fn open_requested(
        self,
        db: Option<&Path>,
    ) -> Result<Database> {
        match db {
            Some(path) => self.open(path).await,
            None => Database::in_memory()
                .map_err(|e| Error::runtime_no_span(e.to_string())),
        }
    }
}

/// Prepared parsed, typechecked, etc... RUMPS program
struct Program {
    ast: Ast,
    stmts: Vec<StmtId>,
    interner: StringInterner,
    pragmas: pragma::Program,
}

impl Program {
    fn prepare(src: &str, src_path: &Path) -> Result<Self> {
        let mut interner = StringInterner::new();
        let result = Parser::parse_with_path(src, src_path, &mut interner)?;

        Ok(Self {
            ast: result.ast,
            stmts: result.stmts,
            interner,
            pragmas: result.pragmas,
        })
    }

    fn db_options(&self) -> DbOptions {
        DbOptions::from(&self.pragmas)
    }

    async fn run_with_io<I: IoContext>(self, db: Database, io: I) -> Result<I> {
        let Self {
            mut ast,
            stmts,
            interner,
            pragmas,
        } = self;
        let interactive = false;
        let interp = Interpreter::new(
            &mut ast,
            &stmts,
            db,
            io,
            interactive,
            interner,
            pragmas,
        )?;
        let interp = interp.run(&stmts, interactive).await?;

        Ok(interp.into_io())
    }
}

/// Run a RUMPS script, outputting to stdout.
///
/// The `src_path` is used to resolve relative module imports.
pub async fn run(src: &str, src_path: &Path, db: Database) -> Result<()> {
    Program::prepare(src, src_path)?
        .run_with_io(db, Io)
        .await
        .map(|_| ())
}

/// Run a RUMPS script in interactive mode, outputting to stdout.
///
/// Interactive mode executes top-level expressions sequentially without
/// requiring a `main` function. The `src_path` is used to resolve relative
/// module imports.
#[allow(unused_variables)]
pub async fn run_interactive(
    src: &str,
    src_path: &Path,
    db: Database,
) -> Result<()> {
    todo!("REPL/interactive mode is not yet supported")
}

/// Run a RUMPS script and capture output to a string.
///
/// The `src_path` is used to resolve relative module imports.
pub async fn run_capturing(
    src: &str,
    src_path: &Path,
    db: Database,
) -> Result<String> {
    let io = Program::prepare(src, src_path)?
        .run_with_io(db, TestIo::new())
        .await?;
    Ok(io.stdout_str().to_owned())
}

/// Open a database and run a RUMPS script, outputting to stdout. This is the
/// version used by the CLI and supports top-level pragmas for database options,
/// e.g. cache size, in the form of `#(options: ...)`.
///
/// The `src_path` is used to resolve relative module imports.
pub async fn run_opening_db(
    src: &str,
    src_path: &Path,
    db: Option<&Path>,
) -> Result<()> {
    let program = Program::prepare(src, src_path)?;
    let open_db = program.db_options().open_requested(db).await?;
    program.run_with_io(open_db, Io).await.map(|_| ())
}

/// Open a database, run a RUMPS script, and capture output to a string.
///
/// The `src_path` is used to resolve relative module imports.
pub async fn run_opening_db_capturing(
    src: &str,
    src_path: &Path,
    db: Option<&Path>,
) -> Result<String> {
    let program = Program::prepare(src, src_path)?;
    let open_db = program.db_options().open_requested(db).await?;
    let io = program.run_with_io(open_db, TestIo::new()).await?;

    Ok(io.stdout_str().to_owned())
}
