//! Static type checking for RUMPS.
//!
//! Implements Hindley-Milner style type inference. Runs after name resolution
//! but before interpretation, rejecting programs with type errors at compile
//! time.
//!
//! # Pipeline
//!
//! ```text
//! Lexer -> CST -> AST -> Name Resolution -> [TYPE CHECK] -> Interpreter
//! ```

// Foundation types; will be used in later phases.
#![allow(dead_code, unused_imports, unused_assignments)]

mod env;
mod error;
mod infer;
mod ty;
mod unify;

pub(crate) use env::TypeEnv;
pub(crate) use error::TypeError;
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use ty::{Scheme, Subst, Ty, TyVar};

use crate::ast::{Ast, StmtId};
use crate::env::Environment;
use crate::intern::StringInterner;
use crate::value::{TypeExprArena, TypeRegistry};

/// Run the type checker on an AST.
///
/// Performs type inference and constraint solving on all statements. Returns
/// `Ok(())` if the program is well-typed, or `Err` with collected type errors.
///
/// # Arguments
///
/// * `ast` - The AST after name resolution
/// * `stmts` - Top-level statement IDs to type-check
/// * `registry` - Type registry with builtin and user-defined types
/// * `type_exprs` - Type expression arena for union member lookups
/// * `runtime_env` - Runtime environment for module function type lookups
/// * `strings` - String interner shared with the registry
pub(crate) fn check(
    ast: &Ast,
    stmts: &[StmtId],
    registry: &TypeRegistry,
    type_exprs: &TypeExprArena,
    runtime_env: &Environment,
    strings: StringInterner,
) -> crate::Result<()> {
    let mut ctx =
        InferCtx::new(ast, registry, type_exprs, runtime_env, strings);

    // Infer types for all statements
    stmts.iter().for_each(|id| ctx.stmt(*id));

    // Solve collected constraints
    let subst = ctx.solve_constraints();

    // Apply substitution to all inferred types
    ctx.apply_subst(&subst);

    // Check for remaining unresolved type variables
    ctx.check_remaining_unknowns();

    ctx.into_result()
}
