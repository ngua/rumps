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
pub(crate) use ty::{
    ClassDef, ClassRegistry, ClassShape, Scheme, Ty, TyArena, TyId, TyVar,
    TypeClass,
};

use crate::ast::{AstTypeExprId, ExprId};
use crate::intern::StringId;

/// Capability required to read AST-backed declaration metadata from `TypeRegistry`.
#[derive(Clone, Copy)]
pub(crate) struct TypeDeclAccess(());

impl TypeDeclAccess {
    fn new() -> Self {
        Self(())
    }
}

/// Output from type checking.
///
/// Contains runtime metadata needed by the interpreter.
pub(crate) struct TypecheckOutput {
    /// Type arena; owns all interned types referenced by `TyId` handles.
    pub(crate) ty_arena: TyArena,
    /// Compiled regex patterns, indexed by `ExprAux::RegexIndex`.
    pub(crate) regex_cache: Vec<regex::Regex>,
    /// Per-expression runtime metadata overrides produced during typecheck.
    pub(crate) expr_metadata: HashMap<ExprId, CheckedExprInfo>,
    /// Expressions widened into a union with their concrete member type.
    pub(crate) union_value_reprs: HashMap<ExprId, TyId>,
    /// Function and closure types keyed by body expression.
    pub(crate) function_types: HashMap<ExprId, TyId>,
    /// Class registry; carries class definitions indexed by `ClassId`.
    pub(crate) class_registry: ClassRegistry,
    /// Resolved types for all expressions, populated from `InferCtx.expr_types`.
    pub(crate) expr_types: HashMap<ExprId, TyId>,
    /// Mapping from AST type expression IDs to their resolved `TyId`s.
    ///
    /// Populated for `IS` type patterns, `AS` casts, `READ` conversions, and
    /// match `IS` arms so the interpreter can look up the target runtime type.
    pub(crate) ast_type_map: HashMap<AstTypeExprId, TyId>,
    /// Maps solved alias `TyId`s to their expanded underlying `TyId`s.
    pub(crate) alias_type_expansions: HashMap<TyId, TyId>,
}

#[derive(Clone, Copy)]
pub(super) struct CheckedExprInfo {
    pub(super) ty: Option<TyId>,
    pub(super) repr: Option<TyId>,
    pub(super) concrete: bool,
    pub(super) aux: CheckedExprAux,
}

impl Default for CheckedExprInfo {
    fn default() -> Self {
        Self {
            ty: None,
            repr: None,
            concrete: false,
            aux: CheckedExprAux::None,
        }
    }
}

impl CheckedExprInfo {
    pub(super) fn resolve(
        &mut self,
        uf: &mut uf::UnionFind,
        arena: &mut TyArena,
    ) {
        self.ty = self.ty.map(|ty| uf.resolve(ty, arena));
        self.repr = self.repr.map(|ty| uf.resolve(ty, arena));
        self.aux.resolve(uf, arena);
    }

    pub(super) fn ty_ids(&self) -> impl Iterator<Item = TyId> {
        self.ty.into_iter().chain(self.repr)
    }
}

#[derive(Clone, Copy)]
pub(super) enum CheckedExprAux {
    None,
    RegexIndex(u32),
    HofCall {
        out: TyId,
        class: Option<StringId>,
    },
    InstanceCall {
        recv: Option<TyId>,
        fun: Option<StringId>,
        class: Option<StringId>,
    },
    NakedMethod {
        class: StringId,
    },
}

impl CheckedExprAux {
    fn resolve(&mut self, uf: &mut uf::UnionFind, arena: &mut TyArena) {
        match self {
            Self::HofCall { out, .. } => {
                *out = uf.resolve(*out, arena);
            }
            Self::InstanceCall { recv, .. } => {
                *recv = recv.map(|ty| uf.resolve(ty, arena));
            }
            Self::None | Self::RegexIndex(_) | Self::NakedMethod { .. } => {}
        }
    }
}

impl TypecheckOutput {
    /// Build a `CheckedProgram` from this output.
    ///
    /// Clones the type arena into `RuntimeTypes`, then collects
    /// per-expression metadata into a unified `HashMap<ExprId, ExprInfo>`.
    pub(crate) fn to_checked(&self) -> CheckedProgram {
        let mut exprs: HashMap<ExprId, ExprInfo> = self
            .expr_types
            .iter()
            .map(|(&id, &ty)| {
                (
                    id,
                    ExprInfo {
                        ty: RuntimeTyId::from(ty),
                        repr: None,
                        aux: ExprAux::None,
                    },
                )
            })
            .collect();

        self.expr_metadata.iter().for_each(|(&id, meta)| {
            let info = exprs.entry(id).or_insert_with(|| ExprInfo {
                ty: RuntimeTyId::from(meta.ty.unwrap_or_else(|| {
                    typechecked!(
                        "expression metadata",
                        "resolved expression type"
                    )
                })),
                repr: None,
                aux: ExprAux::None,
            });
            if let Some(ty) = meta.ty {
                info.ty = RuntimeTyId::from(ty);
            }
            if let Some(repr) = meta.repr {
                info.repr = Some(RuntimeTyId::from(repr));
            }
            info.aux = match meta.aux {
                CheckedExprAux::None => ExprAux::None,
                CheckedExprAux::RegexIndex(idx) => ExprAux::RegexIndex(idx),
                CheckedExprAux::HofCall { out, class } => ExprAux::HofCall {
                    out: RuntimeTyId::from(out),
                    class,
                },
                CheckedExprAux::InstanceCall { recv, fun, class } => {
                    ExprAux::InstanceCall {
                        recv: RuntimeTyId::from(
                            recv.unwrap_or(TyArena::UNKNOWN),
                        ),
                        fun,
                        class,
                    }
                }
                CheckedExprAux::NakedMethod { class } => {
                    ExprAux::NakedMethod { class }
                }
            };
        });

        self.union_value_reprs.iter().for_each(|(&id, &repr)| {
            let info = exprs.get_mut(&id).unwrap_or_else(|| {
                typechecked!("union value", "resolved expression type")
            });
            info.repr = Some(RuntimeTyId::from(repr));
        });

        let function_types = self
            .function_types
            .iter()
            .map(|(&id, &ty)| (id, RuntimeTyId::from(ty)))
            .collect();

        let ast_type_map = self
            .ast_type_map
            .iter()
            .map(|(&id, &ty)| (id, RuntimeTyId::from(ty)))
            .collect();
        let alias_type_expansions: HashMap<RuntimeTyId, RuntimeTyId> = self
            .alias_type_expansions
            .iter()
            .map(|(&alias, &expanded)| {
                (RuntimeTyId::from(alias), RuntimeTyId::from(expanded))
            })
            .collect();

        CheckedProgram {
            types: RuntimeTypes::new(
                self.ty_arena.clone(),
                alias_type_expansions.clone(),
            ),
            exprs,
            regex_cache: self.regex_cache.clone(),
            class_registry: self.class_registry.clone(),
            function_types,
            ast_type_map,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intern::StringInterner;

    #[test]
    fn phase_11_checked_program_preserves_regex_cache_and_class_registry() {
        let mut strings = StringInterner::new();
        let mut arena = TyArena::new();
        let class_registry =
            ClassRegistry::builtins(&mut |s| strings.intern(s), &mut arena);
        let class = strings.intern("Numeric");
        let output = TypecheckOutput {
            ty_arena: arena,
            regex_cache: vec![regex::Regex::new("abc").unwrap()],
            expr_metadata: HashMap::new(),
            union_value_reprs: HashMap::new(),
            function_types: HashMap::new(),
            class_registry,
            expr_types: HashMap::new(),
            ast_type_map: HashMap::new(),
            alias_type_expansions: HashMap::new(),
        };
        let checked = output.to_checked();

        assert_eq!(
            checked.regex_cache.first().map(regex::Regex::as_str),
            Some("abc")
        );
        assert!(checked.class_registry.lookup_by_name(class).is_some());
    }
}
