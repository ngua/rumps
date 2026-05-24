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

mod convert;
mod env;
mod error;
mod infer;
mod instance;
mod runtime_types;
mod ty;
mod uf;
mod unify;

use std::collections::HashMap;

pub(crate) use env::TypeEnv;
pub(crate) use error::{FormattedTypeError, TyPrinter, TypeError};
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use instance::{Instance, InstanceRegistry};
pub(crate) use runtime_types::{
    CheckedProgram, ExprAux, ExprInfo, RuntimeTyId, RuntimeTypes,
};
use smallvec::SmallVec;
pub(crate) use ty::{
    ClassDef, ClassRegistry, ClassShape, Scheme, Ty, TyArena, TyId, TyVar,
    TypeClass,
};

use crate::ast::{AstTypeExprId, ExprId};
use crate::intern::StringId;
use crate::TypeId;

/// Output from type checking.
///
/// Contains runtime metadata needed by the interpreter: compiled regex
/// patterns, type information for polymorphic expressions, and instance
/// dispatch tables.
pub(crate) struct TypecheckOutput {
    /// Type arena; owns all interned types referenced by `TyId` handles.
    pub(crate) ty_arena: TyArena,
    /// Compiled regex patterns, indexed by `regex_indices`.
    pub(crate) regex_cache: Vec<regex::Regex>,
    /// Mapping from regex expression IDs to cache indices.
    pub(crate) regex_indices: HashMap<ExprId, u32>,
    /// Resolved types for `MEMPTY` expressions (monoid identity values).
    pub(crate) mempty_types: HashMap<ExprId, TyId>,
    /// Resolved types for numeric literals (defaulted to `Int` if ambiguous).
    pub(crate) numeric_types: HashMap<ExprId, TyId>,
    /// Target types for `Into::into` and `TryInto::try_into` conversions.
    pub(crate) convert_targets: HashMap<ExprId, TyId>,
    /// Target types for `?` (wrap) operators on `Wrappable` types.
    pub(crate) wrap_types: HashMap<ExprId, TyId>,
    /// Resolved output types for `Bimappable:bimap` calls.
    pub(crate) bimap_output_types: HashMap<ExprId, TyId>,
    /// Type IDs for class method calls on user-defined types.
    ///
    /// Used to dispatch to user-defined class instances at runtime.
    pub(crate) instance_calls: HashMap<ExprId, TypeId>,
    /// Resolved function names for ambiguous parameterized class method calls.
    pub(crate) resolved_instance_fns: HashMap<ExprId, StringId>,
    /// Class registry; carries class definitions indexed by `ClassId`.
    pub(crate) class_registry: ClassRegistry,
    /// Resolved class names for naked (`:method`) class method expressions.
    pub(crate) naked_method_classes: HashMap<ExprId, StringId>,
    /// Resolved types for all expressions, populated from `InferCtx.expr_types`.
    pub(crate) expr_types: HashMap<ExprId, TyId>,
    /// Mapping from AST type expression IDs to their resolved `TyId`s.
    ///
    /// Populated for `IS` type patterns, `AS` casts, `READ` conversions, and
    /// match `IS` arms so the interpreter can look up the target type as a
    /// `RuntimeTyId` without going through the `TypeExprArena`.
    pub(crate) ast_type_map: HashMap<AstTypeExprId, TyId>,
    /// Maps `read` target `AstTypeExprId`s to their expanded underlying `TyId`
    /// when the target is an alias type.
    pub(crate) alias_expansions: HashMap<AstTypeExprId, TyId>,
}

impl TypecheckOutput {
    /// Build a `CheckedProgram` from this output.
    ///
    /// Clones the type arena (since `TypecheckOutput` still owns it for the
    /// current interpreter pipeline) and collects per-expression metadata
    /// into a unified `HashMap<ExprId, ExprInfo>`.
    ///
    /// This is a transitional method; a later phase will switch the
    /// interpreter to consume `CheckedProgram` directly.
    pub(crate) fn to_checked(&self) -> CheckedProgram {
        let mut arena = self.ty_arena.clone();

        // Base layer: all expression types from `expr_types`
        let mut exprs: HashMap<ExprId, ExprInfo> = self
            .expr_types
            .iter()
            .map(|(&id, &ty)| {
                (
                    id,
                    ExprInfo {
                        ty: RuntimeTyId::from(ty),
                        aux: ExprAux::None,
                    },
                )
            })
            .collect();

        // Overlay side-map entries (which carry more specific `ty` values
        // for numeric literals, mempty, wrap, convert, bimap).
        self.numeric_types.iter().for_each(|(&id, &ty)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        self.mempty_types.iter().for_each(|(&id, &ty)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        self.wrap_types.iter().for_each(|(&id, &ty)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        self.convert_targets.iter().for_each(|(&id, &ty)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::None,
                },
            );
        });

        self.bimap_output_types.iter().for_each(|(&id, &ty)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(ty),
                    aux: ExprAux::HofCall {
                        out: RuntimeTyId::from(ty),
                    },
                },
            );
        });

        self.regex_indices.iter().for_each(|(&id, &idx)| {
            exprs.insert(
                id,
                ExprInfo {
                    ty: RuntimeTyId::from(TyArena::REGEX),
                    aux: ExprAux::RegexIndex(idx),
                },
            );
        });

        self.instance_calls.iter().for_each(|(&id, &tid)| {
            let recv = RuntimeTyId::from(arena.named(tid, SmallVec::new()));
            let fun = self.resolved_instance_fns.get(&id).copied();
            let ty = exprs
                .get(&id)
                .map_or(RuntimeTyId::from(TyArena::UNKNOWN), |e| e.ty);
            exprs.insert(
                id,
                ExprInfo {
                    ty,
                    aux: ExprAux::InstanceCall { recv, fun },
                },
            );
        });

        self.naked_method_classes.iter().for_each(|(&id, &class)| {
            let ty = exprs
                .get(&id)
                .map_or(RuntimeTyId::from(TyArena::UNKNOWN), |e| e.ty);
            exprs.insert(
                id,
                ExprInfo {
                    ty,
                    aux: ExprAux::NakedMethod { class },
                },
            );
        });

        CheckedProgram {
            types: RuntimeTypes::new(arena),
            exprs,
            regex_cache: Vec::new(),
            class_registry: ClassRegistry::empty(),
        }
    }
}
