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
mod instance;
mod ty;
mod unify;

use std::collections::HashMap;

pub(crate) use env::TypeEnv;
pub(crate) use error::{FormattedTypeError, TyPrinter, TypeError};
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use instance::{Instance, InstanceRegistry};
pub(crate) use ty::{
    BuiltinClass, BuiltinClassDef, BuiltinClassDefs, BuiltinClassTag,
    ClassShape, Scheme, Subst, Ty, TyVar,
};

use crate::ast::ExprId;
use crate::TypeId;

/// Output from type checking.
///
/// Contains runtime metadata needed by the interpreter: compiled regex
/// patterns, type information for polymorphic expressions, and instance
/// dispatch tables.
pub(crate) struct TypecheckOutput {
    /// Compiled regex patterns, indexed by `regex_indices`.
    pub(crate) regex_cache: Vec<regex::Regex>,
    /// Mapping from regex expression IDs to cache indices.
    pub(crate) regex_indices: HashMap<ExprId, u32>,
    /// Resolved types for `MEMPTY` expressions (monoid identity values).
    pub(crate) mempty_types: HashMap<ExprId, Ty>,
    /// Resolved types for numeric literals (defaulted to `Int` if ambiguous).
    pub(crate) numeric_types: HashMap<ExprId, Ty>,
    /// Target types for `Into::into` and `TryInto::try_into` conversions.
    pub(crate) convert_targets: HashMap<ExprId, Ty>,
    /// Target types for `?` (wrap) operators on `Fallible` types.
    pub(crate) wrap_types: HashMap<ExprId, Ty>,
    /// Type IDs for class method calls on user-defined types.
    ///
    /// Used to dispatch to user-defined class instances at runtime.
    pub(crate) instance_calls: HashMap<ExprId, TypeId>,
}
