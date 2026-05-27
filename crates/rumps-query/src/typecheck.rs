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
mod decl;
mod env;
mod error;
mod infer;
mod instance;
mod runtime_types;
mod ty;
mod uf;
mod unify;

use std::collections::HashMap;
use std::iter;

pub(crate) use env::TypeEnv;
pub(crate) use error::{FormattedTypeError, TyPrinter, TypeError};
pub(crate) use infer::{Constraint, InferCtx};
pub(crate) use instance::{Instance, InstanceRegistry};
pub(crate) use runtime_types::{
    CheckedProgram, ExprAux, ExprInfo, NewtypeEdgeRuntimeInfo, RuntimeTyId,
    RuntimeTypes, TypePatternInfo,
};
pub(crate) use ty::{
    ClassDef, ClassRegistry, ClassShape, Scheme, Ty, TyArena, TyId, TyVar,
    TypeClass,
};

use crate::ast::{ExprId, MatchPatternId};
use crate::intern::StringId;

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
    /// Builtin module function type metadata keyed by full module path.
    pub(crate) module_fn_types: HashMap<Vec<StringId>, TyId>,
    /// Builtin module constant type metadata keyed by full module path.
    pub(crate) module_const_types: HashMap<Vec<StringId>, TyId>,
    /// Class registry; carries class definitions indexed by `ClassId`.
    pub(crate) class_registry: ClassRegistry,
    /// Resolved types for all expressions, populated from `InferCtx.expr_types`.
    pub(crate) expr_types: HashMap<ExprId, TyId>,
    /// Checked target types for `as`, `read`, and annotation expressions.
    pub(crate) expr_targets: HashMap<ExprId, TyId>,
    /// Newtype representation edges approved by static checking.
    pub(crate) approved_newtype_edges:
        HashMap<ExprId, CheckedNewtypeEdgeRuntimeInfo>,
    /// Checked type facts for `expr is Pattern` expression patterns.
    pub(crate) is_patterns: HashMap<ExprId, CheckedTypePatternInfo>,
    /// Checked type annotation targets for `let` bindings, keyed by RHS expr.
    pub(crate) let_targets: HashMap<ExprId, TyId>,
    /// Checked target types for `name IS Type` match patterns.
    pub(crate) match_targets: HashMap<MatchPatternId, TyId>,
    /// Maps solved alias `TyId`s to their expanded underlying `TyId`s.
    pub(crate) alias_type_expansions: HashMap<TyId, TyId>,
}

#[derive(Clone)]
pub(super) enum CheckedTypePatternInfo {
    Type(TyId),
    Object(Vec<(StringId, TyId)>),
}

impl CheckedTypePatternInfo {
    pub(super) fn resolve(
        &mut self,
        uf: &mut uf::UnionFind,
        arena: &mut TyArena,
    ) {
        match self {
            Self::Type(ty) => {
                *ty = uf.resolve(*ty, arena);
            }
            Self::Object(fields) => {
                fields
                    .iter_mut()
                    .for_each(|(_, ty)| *ty = uf.resolve(*ty, arena));
            }
        }
    }

    pub(super) fn ty_ids(&self) -> Box<dyn Iterator<Item = TyId> + '_> {
        match self {
            Self::Type(ty) => Box::new(iter::once(*ty)),
            Self::Object(fields) => Box::new(fields.iter().map(|(_, ty)| *ty)),
        }
    }
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
pub(super) struct CheckedNewtypeEdgeRuntimeInfo {
    pub(super) from: TyId,
    pub(super) to: TyId,
    pub(super) repr: TyId,
}

impl CheckedNewtypeEdgeRuntimeInfo {
    pub(super) fn resolve(
        &mut self,
        uf: &mut uf::UnionFind,
        arena: &mut TyArena,
    ) {
        self.from = uf.resolve(self.from, arena);
        self.to = uf.resolve(self.to, arena);
        self.repr = uf.resolve(self.repr, arena);
    }

    pub(super) fn ty_ids(&self) -> impl Iterator<Item = TyId> {
        [self.from, self.to, self.repr].into_iter()
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
                        recv: recv.map(RuntimeTyId::from),
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

        let expr_targets = self
            .expr_targets
            .iter()
            .map(|(&id, &ty)| (id, RuntimeTyId::from(ty)))
            .collect();
        let approved_newtype_edges = self
            .approved_newtype_edges
            .iter()
            .map(|(&id, info)| {
                (
                    id,
                    NewtypeEdgeRuntimeInfo {
                        from: RuntimeTyId::from(info.from),
                        to: RuntimeTyId::from(info.to),
                        repr: RuntimeTyId::from(info.repr),
                    },
                )
            })
            .collect();
        let is_patterns = self
            .is_patterns
            .iter()
            .map(|(&id, info)| {
                let info = match info {
                    CheckedTypePatternInfo::Type(ty) => {
                        TypePatternInfo::Type(RuntimeTyId::from(*ty))
                    }
                    CheckedTypePatternInfo::Object(fields) => {
                        TypePatternInfo::Object(
                            fields
                                .iter()
                                .map(|(name, ty)| {
                                    (*name, RuntimeTyId::from(*ty))
                                })
                                .collect(),
                        )
                    }
                };
                (id, info)
            })
            .collect();
        let let_targets = self
            .let_targets
            .iter()
            .map(|(&id, &ty)| (id, RuntimeTyId::from(ty)))
            .collect();
        let match_targets = self
            .match_targets
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

        let types =
            RuntimeTypes::new(self.ty_arena.clone(), alias_type_expansions);

        let module_fns = self
            .module_fn_types
            .iter()
            .map(|(path, &ty)| {
                (path.clone(), types.meta(RuntimeTyId::from(ty)))
            })
            .collect();
        let module_consts = self
            .module_const_types
            .iter()
            .map(|(path, &ty)| {
                (path.clone(), types.meta(RuntimeTyId::from(ty)))
            })
            .collect();

        CheckedProgram {
            types,
            exprs,
            regex_cache: self.regex_cache.clone(),
            class_registry: self.class_registry.clone(),
            function_types,
            module_fns,
            module_consts,
            expr_targets,
            approved_newtype_edges,
            is_patterns,
            let_targets,
            match_targets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ast, Expr, Literal, MatchPattern};
    use crate::intern::StringInterner;
    use crate::value::ValueMeta;
    use crate::Span;

    const _: Option<TyId> = match (CheckedExprAux::InstanceCall {
        recv: None,
        fun: None,
        class: None,
    }) {
        CheckedExprAux::InstanceCall { recv, .. } => recv,
        _ => None,
    };
    const _: Option<RuntimeTyId> = match (ExprAux::InstanceCall {
        recv: None,
        fun: None,
        class: None,
    }) {
        ExprAux::InstanceCall { recv, .. } => recv,
        _ => None,
    };
    const _: fn(&CheckedProgram, &[StringId]) -> Option<ValueMeta> =
        CheckedProgram::module_fn_meta;
    const _: fn(&CheckedProgram, &[StringId]) -> Option<ValueMeta> =
        CheckedProgram::module_const_meta;
    const _: fn(&CheckedProgram, ExprId, &'static str) -> RuntimeTyId =
        CheckedProgram::expr_target;

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
            module_fn_types: HashMap::new(),
            module_const_types: HashMap::new(),
            class_registry,
            expr_types: HashMap::new(),
            expr_targets: HashMap::new(),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::new(),
            let_targets: HashMap::new(),
            match_targets: HashMap::new(),
            alias_type_expansions: HashMap::new(),
        };
        let checked = output.to_checked();

        assert_eq!(
            checked.regex_cache.first().map(regex::Regex::as_str),
            Some("abc")
        );
        assert!(checked.class_registry.lookup_by_name(class).is_some());
    }

    #[test]
    fn phase_5_checked_instance_call_keeps_missing_recv() {
        let mut strings = StringInterner::new();
        let mut ast = Ast::new();
        let mut arena = TyArena::new();
        let class_registry =
            ClassRegistry::builtins(&mut |s| strings.intern(s), &mut arena);
        let id = ast
            .add_expr(Expr::Literal(Literal::Unit), Span::new(0, 0))
            .unwrap();
        let fun = strings.intern("instance_fn");
        let output = TypecheckOutput {
            ty_arena: arena,
            regex_cache: Vec::new(),
            expr_metadata: HashMap::from([(
                id,
                CheckedExprInfo {
                    ty: Some(TyArena::UNIT),
                    repr: None,
                    concrete: false,
                    aux: CheckedExprAux::InstanceCall {
                        recv: None,
                        fun: Some(fun),
                        class: None,
                    },
                },
            )]),
            union_value_reprs: HashMap::new(),
            function_types: HashMap::new(),
            module_fn_types: HashMap::new(),
            module_const_types: HashMap::new(),
            class_registry,
            expr_types: HashMap::from([(id, TyArena::UNIT)]),
            expr_targets: HashMap::new(),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::new(),
            let_targets: HashMap::new(),
            match_targets: HashMap::new(),
            alias_type_expansions: HashMap::new(),
        };
        let checked = output.to_checked();

        match checked.expr(id).aux {
            ExprAux::InstanceCall { recv, .. } => {
                assert_eq!(recv, None);
            }
            _ => panic!("expected instance call metadata"),
        }
    }

    #[test]
    fn phase_5_checked_program_owns_builtin_module_metadata() {
        let mut strings = StringInterner::new();
        let mut arena = TyArena::new();
        let class_registry =
            ClassRegistry::builtins(&mut |s| strings.intern(s), &mut arena);
        let module = strings.intern("Math");
        let fun = strings.intern("floor");
        let cst = strings.intern("pi");
        let fun_ty =
            arena.func(smallvec::smallvec![TyArena::FLOAT], TyArena::INT);
        let output = TypecheckOutput {
            ty_arena: arena,
            regex_cache: Vec::new(),
            expr_metadata: HashMap::new(),
            union_value_reprs: HashMap::new(),
            function_types: HashMap::new(),
            module_fn_types: HashMap::from([(vec![module, fun], fun_ty)]),
            module_const_types: HashMap::from([(
                vec![module, cst],
                TyArena::FLOAT,
            )]),
            class_registry,
            expr_types: HashMap::new(),
            expr_targets: HashMap::new(),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::new(),
            let_targets: HashMap::new(),
            match_targets: HashMap::new(),
            alias_type_expansions: HashMap::new(),
        };
        let checked = output.to_checked();

        let fn_meta = checked.module_fn_meta(&[module, fun]);
        assert_eq!(checked.module_fn_arity(&[module, fun]), Some(1));
        assert_eq!(
            fn_meta.map(|meta| meta.ty),
            Some(RuntimeTyId::from(fun_ty))
        );
        assert!(fn_meta
            .map(|meta| checked.types.get(meta.ty))
            .is_some_and(|ty| !matches!(ty, Ty::Unknown)));
        assert_eq!(
            checked
                .module_const_meta(&[module, cst])
                .map(|meta| meta.ty),
            Some(RuntimeTyId::from(TyArena::FLOAT))
        );
    }

    #[test]
    fn phase_5_checked_program_owns_as_and_read_targets() {
        let mut strings = StringInterner::new();
        let mut ast = Ast::new();
        let mut arena = TyArena::new();
        let class_registry =
            ClassRegistry::builtins(&mut |s| strings.intern(s), &mut arena);
        let as_id = ast
            .add_expr(Expr::Literal(Literal::Unit), Span::new(0, 0))
            .unwrap();
        let read_id = ast
            .add_expr(Expr::Literal(Literal::Unit), Span::new(0, 0))
            .unwrap();
        let let_id = ast
            .add_expr(Expr::Literal(Literal::Unit), Span::new(0, 0))
            .unwrap();
        let is_id = ast
            .add_expr(Expr::Literal(Literal::Unit), Span::new(0, 0))
            .unwrap();
        let pat_id = ast.add_pattern(MatchPattern::Wildcard).unwrap();
        let field = strings.intern("name");
        let output = TypecheckOutput {
            ty_arena: arena,
            regex_cache: Vec::new(),
            expr_metadata: HashMap::new(),
            union_value_reprs: HashMap::new(),
            function_types: HashMap::new(),
            module_fn_types: HashMap::new(),
            module_const_types: HashMap::new(),
            class_registry,
            expr_types: HashMap::new(),
            expr_targets: HashMap::from([
                (as_id, TyArena::STRING),
                (read_id, TyArena::INT),
            ]),
            approved_newtype_edges: HashMap::new(),
            is_patterns: HashMap::from([(
                is_id,
                CheckedTypePatternInfo::Object(vec![(field, TyArena::STRING)]),
            )]),
            let_targets: HashMap::from([(let_id, TyArena::INT)]),
            match_targets: HashMap::from([(pat_id, TyArena::FLOAT)]),
            alias_type_expansions: HashMap::new(),
        };
        let checked = output.to_checked();

        assert_eq!(
            checked.expr_target(as_id, "as"),
            RuntimeTyId::from(TyArena::STRING)
        );
        assert_eq!(
            checked.expr_target(read_id, "read"),
            RuntimeTyId::from(TyArena::INT)
        );
        assert_eq!(
            checked.let_target(let_id),
            Some(RuntimeTyId::from(TyArena::INT))
        );
        assert_eq!(
            checked.match_target(pat_id),
            RuntimeTyId::from(TyArena::FLOAT)
        );
        match checked.is_patterns.get(&is_id) {
            Some(TypePatternInfo::Object(fields)) => {
                assert_eq!(
                    fields.first().copied(),
                    Some((field, RuntimeTyId::from(TyArena::STRING)))
                );
            }
            _ => panic!("expected object pattern metadata"),
        }
    }
}
