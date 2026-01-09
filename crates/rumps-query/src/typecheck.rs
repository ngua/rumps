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

use std::collections::HashMap;

pub(crate) use env::TypeEnv;
pub(crate) use error::{FormattedTypeError, TyPrinter, TypeError};
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use ty::{Scheme, Subst, Ty, TyVar};

use crate::ast::{Ast, ExprId, StmtId};
use crate::env::Environment;
use crate::intern::StringInterner;
use crate::value::{TypeExprArena, TypeRegistry, ValueArena};

/// Run the type checker on an AST.
///
/// Performs type inference and constraint solving on all statements. Returns
/// a cache of compiled regex patterns and mempty types on success, or `Err`
/// with collected type errors.
///
/// # Arguments
///
/// * `ast` - The AST after name resolution (mutable for `TxnId` population)
/// * `stmts` - Top-level statement IDs to type-check
/// * `registry` - Type registry with builtin and user-defined types
/// * `type_exprs` - Type expression arena for union member lookups
/// * `runtime_env` - Runtime environment for module function type lookups
/// * `arena` - Value arena for string lookups in error messages
/// * `strings` - String interner shared with the registry
pub(crate) fn check(
    ast: &mut Ast,
    stmts: &[StmtId],
    registry: &TypeRegistry,
    type_exprs: &TypeExprArena,
    runtime_env: &Environment,
    arena: &ValueArena,
    strings: StringInterner,
) -> crate::Result<(
    Vec<regex::Regex>,
    HashMap<ExprId, u32>,
    HashMap<ExprId, Ty>,
    HashMap<ExprId, Ty>,
)> {
    let mut ctx =
        InferCtx::new(ast, registry, type_exprs, runtime_env, strings);

    // Pass 1: Hoist function and module declarations for forward references
    ctx.hoist_declarations(stmts);

    // Pass 2: Infer types for all statement bodies
    stmts.iter().for_each(|id| ctx.stmt(*id));

    // Solve collected constraints
    let subst = ctx.solve_constraints();

    // Apply substitution to all inferred types
    ctx.apply_subst(&subst);

    // Check for remaining unresolved type variables
    ctx.check_remaining_unknowns();

    ctx.into_result_formatted(registry, arena)
}
