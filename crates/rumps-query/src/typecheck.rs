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
#![allow(dead_code, unused_imports)]

mod env;
mod error;
mod infer;
mod ty;

pub(crate) use env::TypeEnv;
pub(crate) use error::TypeError;
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use ty::{Scheme, Subst, Ty, TyVar};
