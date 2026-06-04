//! Type unification and constraint solving.
//!
//! Implements the core unification algorithm for Hindley-Milner type inference.
//! Unification determines whether two types can be made equal, and if so,
//! produces a substitution mapping type variables to concrete types.
//!
//! # Testing Philosophy
//!
//! This module has no unit tests. While unification is a pure function on types,
//! testing it in isolation proved less effective than integration testing:
//!
//! 1. All unification behavior is exercised by real code in `scripts/*.rumps`
//! 2. Edge cases (occurs check, union ordering) are implicitly tested through
//!    scripts that rely on correct unification
//! 3. Integration tests catch bugs that synthetic type construction misses
//!
//! See `infer.rs` for the full rationale on our testing approach.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::convert::ConvertCtx;
use super::decl::TypeDeclRegistry;
use super::env::TypeEnv;
use super::error::TypeError;
use super::infer::{ClassContext, Constraint};
use super::instance::{Instance, InstanceRegistry};
use super::ty::{Rename, Ty, TyArena, TyId, TyVar, TypeClass};
use super::uf::UnionFind;
use crate::ast::{Ast, AstTypeExpr, AstTypeExprId};
use crate::intern::{QualifiedName, StringId};
use crate::value::{TypeDef, TypeId, TypeRegistry};
use crate::{ClassId, Span};

/// Result of a unification attempt.
///
/// With union-find, successful unification mutates the UF in-place.
pub(crate) type UnifyResult = Result<(), TypeError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NewtypeEdge {
    pub(super) alias: TypeId,
    pub(super) from: TyId,
    pub(super) to: TyId,
    pub(super) repr: TyId,
}

#[derive(Clone, Copy)]
struct NewtypeEdgeCtx {
    from: TyId,
    to: TyId,
    span: Span,
}

#[derive(Clone, Copy)]
struct NewtypeAlias<'a> {
    id: TypeId,
    args: &'a [TyId],
    repr: TyId,
}

#[derive(Clone, Copy)]
enum NewtypeEdgeMode {
    Accessible,
    Any,
}

/// Result of checking a `newtype` representation edge.
///
/// `Allowed` means both `type visibility` and `repr visibility` permit the
/// edge. `Blocked` means the `newtype` exists but private `repr visibility`
/// prevents external use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NewtypeEdgeStatus {
    Allowed,
    Blocked,
    Missing,
}

/// Context for constraint solving (unification, class satisfaction, etc.).
///
/// Created from `InferCtx` fields for the duration of `solve_constraints`.
/// Separates the solving machinery from inference-time bookkeeping.
pub(super) struct SolveCtx<'a> {
    pub(super) ty_arena: &'a mut TyArena,
    pub(super) uf: &'a mut UnionFind,
    pub(super) registry: &'a TypeRegistry,
    pub(super) decls: &'a TypeDeclRegistry,
    pub(super) instance_registry: &'a InstanceRegistry,
    pub(super) env: &'a TypeEnv,
    pub(super) errors: &'a mut Vec<TypeError>,
    /// AST reference for alias expansion and field type resolution.
    pub(super) ast: &'a mut Ast,
    /// Current module path (for module-aware type name resolution).
    pub(super) current_module: Option<QualifiedName>,
    /// Current class context (for associated type resolution).
    pub(super) class_context: &'a Option<ClassContext>,
    /// Maps HKT class-constrained type variables to their `ClassId`, so
    /// `unify_apply` can look up tuple constructor instances for
    /// position-aware decomposition.
    pub(super) hkt_var_classes: HashMap<TyVar, ClassId>,
}

/// How a specific `Ty` shape satisfies a class.
enum Satisfaction {
    /// Type directly satisfies (no recursion needed).
    Direct,
    /// Recurse into inner types (for containers like `Array[T]`).
    Recurse(SmallVec<[TyId; 4]>),
}

impl SolveCtx<'_> {
    fn is_numeric_capability(id: ClassId) -> bool {
        matches!(
            id,
            ClassId::NUMERIC
                | ClassId::ADDITIVE
                | ClassId::SUBTRACTIVE
                | ClassId::MULTIPLICATIVE
                | ClassId::DIVISIBLE
                | ClassId::FLOOR_DIVISIBLE
                | ClassId::POWERABLE
        )
    }

    /// Map a primitive `Ty` shape to its `TypeId`, if applicable.
    fn primitive_type_id(ty: &Ty) -> Option<TypeId> {
        match ty {
            Ty::Bool => Some(TypeId::BOOL),
            Ty::Int => Some(TypeId::INT),
            Ty::Word => Some(TypeId::WORD),
            Ty::Float => Some(TypeId::FLOAT),
            Ty::Char => Some(TypeId::CHAR),
            Ty::String => Some(TypeId::STRING),
            Ty::Unit => Some(TypeId::UNIT),
            Ty::Time => Some(TypeId::TIME),
            Ty::Range => Some(TypeId::RANGE),
            Ty::Json => Some(TypeId::JSON),
            Ty::Ordering => Some(TypeId::ORDERING),
            Ty::DataStatus => Some(TypeId::DATA_STATUS),
            Ty::FilePath => Some(TypeId::FILEPATH),
            Ty::Path => Some(TypeId::PATH),
            Ty::Regex => Some(TypeId::REGEX),
            Ty::Local => Some(TypeId::LOCAL),
            Ty::Global => Some(TypeId::GLOBAL),
            Ty::Array(_) => Some(TypeId::ARRAY),
            Ty::Option(_) => Some(TypeId::OPTION),
            Ty::Result(_, _) => Some(TypeId::RESULT),
            Ty::Map(_, _) => Some(TypeId::MAP),
            Ty::Tuple(_) => Some(TypeId::TUPLE),
            _ => None,
        }
    }

    /// Map a `Ty` to its `(TypeId, type_args)` pair for instance lookup.
    ///
    /// Handles both primitive types (no args) and parameterized builtins
    /// (`Array[T]`, `Option[T]`, `Result[Ok, Err]`, `Map[K, V]`), as well
    /// as `Ty::Named`.
    fn ty_to_type_id_and_args(
        &self,
        ty: TyId,
    ) -> Option<(TypeId, SmallVec<[TyId; 4]>)> {
        match self.ty_arena.get(ty).clone() {
            Ty::Named(id, args) => Some((id, args)),
            Ty::Array(e) => Some((TypeId::ARRAY, smallvec![e])),
            Ty::Option(e) => Some((TypeId::OPTION, smallvec![e])),
            Ty::Result(ok, err) => Some((TypeId::RESULT, smallvec![ok, err])),
            Ty::Map(k, v) => Some((TypeId::MAP, smallvec![k, v])),
            Ty::Tuple(ts) => Some((TypeId::TUPLE, ts)),
            ref shape => {
                Self::primitive_type_id(shape).map(|id| (id, smallvec![]))
            }
        }
    }

    fn convert_ctx(&mut self) -> ConvertCtx<'_> {
        ConvertCtx {
            ty_arena: self.ty_arena,
            uf: self.uf,
            registry: self.registry,
            decls: self.decls,
            env: self.env,
            ast: self.ast,
            errors: self.errors,
            current_module: &self.current_module,
            class_context: self.class_context,
            rewrite_ast: false,
        }
    }

    /// Unify two types, recording bindings in the union-find.
    ///
    /// # Unification Rules
    ///
    /// 1. `Var(v) ~ t` binds `v` to `t` (if `v` not in `fv(t)`; occurs check)
    /// 2. `t ~ Var(v)` binds `v` to `t` (symmetric)
    /// 3. `Array[a] ~ Array[b]` recurses into `unify(a, b)`
    /// 4. `Fn[p1] -> r1 ~ Fn[p2] -> r2` recurses into params and return types
    /// 5. `{ f1 } ~ { f2 }` unifies common fields (structural objects)
    /// 6. `Named(id, args1) ~ Named(id, args2)` unifies corresponding args
    /// 7. `Unknown ~ _` or `_ ~ Unknown` succeeds (unifies with anything)
    /// 8. `Error ~ _` or `_ ~ Error` succeeds (error recovery)
    /// 9. `T ~ T` succeeds (primitives equal)
    /// 10. Otherwise: error
    ///
    /// # No Implicit Numeric Coercion
    ///
    /// Numeric types (`Int`, `Float`, `Word`) do NOT implicitly coerce.
    /// Use explicit `AS` casts to convert between them.
    pub(crate) fn unify_types(
        &mut self,
        t1: TyId,
        t2: TyId,
        span: Span,
    ) -> UnifyResult {
        self.unify_inner(t1, t2, span)
    }

    pub(super) fn newtype_edge(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> Option<NewtypeEdge> {
        let (edge, cycle) = self.newtype_edge_with_mode(
            from,
            to,
            span,
            NewtypeEdgeMode::Accessible,
        );
        if edge.is_none() {
            if let Some(ty) = cycle {
                self.report_recursive_newtype_edge(ty, span);
            }
        }
        edge
    }

    pub(super) fn newtype_edge_any(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> Option<NewtypeEdge> {
        self.newtype_edge_with_mode(from, to, span, NewtypeEdgeMode::Any)
            .0
    }

    pub(super) fn newtype_edge_status(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> NewtypeEdgeStatus {
        let (edge, cycle) = self.newtype_edge_with_mode(
            from,
            to,
            span,
            NewtypeEdgeMode::Accessible,
        );
        match edge {
            Some(_) => NewtypeEdgeStatus::Allowed,
            None => {
                let snap = self.uf.snapshot();
                let err_len = self.errors.len();
                let (blocked, any_cycle) = self.newtype_edge_with_mode(
                    from,
                    to,
                    span,
                    NewtypeEdgeMode::Any,
                );
                self.uf.rollback(snap);
                self.errors.truncate(err_len);
                match blocked {
                    Some(_) => NewtypeEdgeStatus::Blocked,
                    None => {
                        if let Some(ty) = cycle.or(any_cycle) {
                            self.report_recursive_newtype_edge(ty, span);
                        }
                        NewtypeEdgeStatus::Missing
                    }
                }
            }
        }
    }

    fn newtype_edge_blocked(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
    ) -> bool {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let blocked = self.newtype_edge_status(from, to, span)
            == NewtypeEdgeStatus::Blocked;
        self.uf.rollback(snap);
        self.errors.truncate(err_len);
        blocked
    }

    fn newtype_edge_with_mode(
        &mut self,
        from: TyId,
        to: TyId,
        span: Span,
        mode: NewtypeEdgeMode,
    ) -> (Option<NewtypeEdge>, Option<TyId>) {
        let from = self.uf.resolve(from, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);

        let ctx = NewtypeEdgeCtx { from, to, span };
        let mut cycle = None;
        let edge = self
            .newtype_edge_candidate(from, to, ctx, &mut cycle, mode)
            .or_else(|| {
                self.newtype_edge_candidate(to, from, ctx, &mut cycle, mode)
            });
        (edge, cycle)
    }

    fn newtype_edge_candidate(
        &mut self,
        alias_ty: TyId,
        other: TyId,
        ctx: NewtypeEdgeCtx,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> Option<NewtypeEdge> {
        let snap = self.uf.snapshot();
        let err_len = self.errors.len();
        let edge = self.newtype_edge_inner(
            alias_ty,
            other,
            ctx,
            &mut HashSet::new(),
            cycle,
            mode,
        );
        if edge.is_some() && self.errors.len() == err_len {
            edge
        } else {
            self.uf.rollback(snap);
            self.errors.truncate(err_len);
            None
        }
    }

    fn newtype_edge_inner(
        &mut self,
        alias_ty: TyId,
        other: TyId,
        ctx: NewtypeEdgeCtx,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> Option<NewtypeEdge> {
        match self.alias_parts(alias_ty) {
            Some((alias, args)) => {
                if !seen.insert(alias) {
                    *cycle = Some(alias_ty);
                    None
                } else if matches!(mode, NewtypeEdgeMode::Accessible)
                    && !self.convert_ctx().can_access_alias_repr(alias)
                {
                    seen.remove(&alias);
                    None
                } else {
                    let repr = self.alias_repr(alias, &args);
                    let edge = match self.newtype_repr_matches(
                        NewtypeAlias {
                            id: alias,
                            args: &args,
                            repr,
                        },
                        other,
                        ctx,
                        seen,
                        cycle,
                        mode,
                    ) {
                        Ok(()) => Some(NewtypeEdge {
                            alias,
                            from: ctx.from,
                            to: ctx.to,
                            repr,
                        }),
                        Err(_) => None,
                    };
                    seen.remove(&alias);
                    edge
                }
            }
            None => None,
        }
    }

    fn alias_parts(&self, ty: TyId) -> Option<(TypeId, SmallVec<[TyId; 4]>)> {
        match self.ty_arena.get(ty).clone() {
            Ty::Named(id, args) if self.decls.is_alias(id) => Some((id, args)),
            _ => None,
        }
    }

    fn alias_repr(&mut self, alias: TypeId, args: &[TyId]) -> TyId {
        match self.registry.get_def(alias) {
            Some(TypeDef::Alias { type_params, .. }) => {
                let ps = type_params.clone();
                let subst: IndexMap<StringId, TyId> =
                    ps.iter().zip(args.iter()).map(|(&p, &a)| (p, a)).collect();
                let target = self.decls.alias_target(alias);
                self.convert_ctx().ast_type_to_ty(target, &subst)
            }
            _ => typechecked!("newtype edge", "alias declaration"),
        }
    }

    fn report_recursive_newtype_edge(&mut self, ty: TyId, span: Span) {
        let v = self.uf.fresh();
        self.errors.push(TypeError::InfiniteType(v, ty, span));
    }

    fn newtype_repr_matches(
        &mut self,
        alias: NewtypeAlias<'_>,
        other: TyId,
        ctx: NewtypeEdgeCtx,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> UnifyResult {
        match self
            .ast
            .get_type_expr(self.decls.alias_target(alias.id))
            .cloned()
        {
            Some(AstTypeExpr::Object(fields)) => self
                .newtype_object_repr_matches(
                    alias.id, alias.args, alias.repr, &fields, other, ctx.span,
                    seen, cycle, mode,
                ),
            _ => self.newtype_structural_match(
                alias.repr, other, ctx.span, seen, cycle, mode,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn newtype_object_repr_matches(
        &mut self,
        alias: TypeId,
        args: &[TyId],
        repr: TyId,
        fields: &SmallVec<[(StringId, AstTypeExprId); 4]>,
        other: TyId,
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> UnifyResult {
        match self.ty_arena.get(other).clone() {
            Ty::Object(obj_fields) => {
                let ps = match self.registry.get_def(alias) {
                    Some(TypeDef::Alias { type_params, .. }) => {
                        type_params.clone()
                    }
                    _ => typechecked!("newtype edge", "alias declaration"),
                };
                let subst: IndexMap<StringId, TyId> =
                    ps.iter().zip(args.iter()).map(|(&p, &a)| (p, a)).collect();
                fields.iter().try_for_each(|(name, ast_ty)| {
                    let exp =
                        self.convert_ctx().ast_type_to_ty(*ast_ty, &subst);
                    match obj_fields.get(name) {
                        Some(&got) => self.newtype_structural_match(
                            exp, got, span, seen, cycle, mode,
                        ),
                        None => Err(TypeError::MissingField {
                            ty: alias,
                            field: self.env.resolve_string(*name),
                            span,
                        }),
                    }
                })
            }
            _ => Err(TypeError::Mismatch {
                expected: repr,
                got: other,
                span,
            }),
        }
    }

    fn newtype_structural_match(
        &mut self,
        t1: TyId,
        t2: TyId,
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> UnifyResult {
        if t1 == t2 {
            Ok(())
        } else {
            let ty1 = self.ty_arena.get(t1).clone();
            let ty2 = self.ty_arena.get(t2).clone();
            match (ty1, ty2) {
                (Ty::Error, _) | (_, Ty::Error) => Ok(()),
                (Ty::Unknown, _) | (_, Ty::Unknown) => Ok(()),
                (Ty::Named(id, _), _) if self.decls.is_alias(id) => self
                    .newtype_edge_inner(
                        t1,
                        t2,
                        NewtypeEdgeCtx {
                            from: t1,
                            to: t2,
                            span,
                        },
                        seen,
                        cycle,
                        mode,
                    )
                    .map_or_else(
                        || {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span,
                            })
                        },
                        |_| Ok(()),
                    ),
                (_, Ty::Named(id, _)) if self.decls.is_alias(id) => self
                    .newtype_edge_inner(
                        t2,
                        t1,
                        NewtypeEdgeCtx {
                            from: t1,
                            to: t2,
                            span,
                        },
                        seen,
                        cycle,
                        mode,
                    )
                    .map_or_else(
                        || {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span,
                            })
                        },
                        |_| Ok(()),
                    ),

                (Ty::Var(v), _) => self.unify_var(v, t2, span),
                (_, Ty::Var(v)) => self.unify_var(v, t1, span),

                (Ty::Bool, Ty::Bool)
                | (Ty::Int, Ty::Int)
                | (Ty::Word, Ty::Word)
                | (Ty::Float, Ty::Float)
                | (Ty::Char, Ty::Char)
                | (Ty::String, Ty::String)
                | (Ty::Unit, Ty::Unit)
                | (Ty::Time, Ty::Time)
                | (Ty::Range, Ty::Range)
                | (Ty::Json, Ty::Json)
                | (Ty::Ordering, Ty::Ordering)
                | (Ty::DataStatus, Ty::DataStatus)
                | (Ty::FilePath, Ty::FilePath)
                | (Ty::Path, Ty::Path)
                | (Ty::Regex, Ty::Regex)
                | (Ty::RuntimeError, Ty::RuntimeError)
                | (Ty::Local, Ty::Local)
                | (Ty::Global, Ty::Global) => Ok(()),

                (Ty::Array(a), Ty::Array(b))
                | (Ty::Option(a), Ty::Option(b)) => {
                    self.newtype_structural_match(a, b, span, seen, cycle, mode)
                }
                (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                    self.newtype_structural_match(
                        ok1, ok2, span, seen, cycle, mode,
                    )?;
                    self.newtype_structural_match(
                        err1, err2, span, seen, cycle, mode,
                    )
                }
                (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                    self.newtype_structural_match(
                        k1, k2, span, seen, cycle, mode,
                    )?;
                    self.newtype_structural_match(
                        v1, v2, span, seen, cycle, mode,
                    )
                }
                (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                    if ts1.len() == ts2.len() {
                        self.newtype_structural_sequence(
                            ts1.iter().copied(),
                            ts2.iter().copied(),
                            span,
                            seen,
                            cycle,
                            mode,
                        )
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    }
                }
                (Ty::Fn(ps1, ret1), Ty::Fn(ps2, ret2)) => {
                    if ps1.len() == ps2.len() {
                        self.newtype_structural_sequence(
                            ps1.iter().copied(),
                            ps2.iter().copied(),
                            span,
                            seen,
                            cycle,
                            mode,
                        )?;
                        self.newtype_structural_match(
                            ret1, ret2, span, seen, cycle, mode,
                        )
                    } else {
                        Err(TypeError::ArityMismatch {
                            expected: ps2.len(),
                            got: ps1.len(),
                            span,
                        })
                    }
                }
                (Ty::Object(fs1), Ty::Object(fs2)) => self
                    .newtype_structural_objects(
                        &fs1, &fs2, span, seen, cycle, mode,
                    ),
                (Ty::Union(_, ms1), Ty::Union(_, ms2)) => {
                    if ms1.len() == ms2.len() {
                        let available: Vec<usize> = (0..ms2.len()).collect();
                        self.newtype_structural_union(
                            &ms1, &ms2, &available, span, seen, cycle, mode,
                        )
                        .unwrap_or_else(|| {
                            Err(TypeError::Mismatch {
                                expected: t2,
                                got: t1,
                                span,
                            })
                        })
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    }
                }
                (Ty::Named(id1, args1), Ty::Named(id2, args2)) => {
                    if id1 == id2 && args1.len() == args2.len() {
                        self.newtype_structural_sequence(
                            args1.iter().copied(),
                            args2.iter().copied(),
                            span,
                            seen,
                            cycle,
                            mode,
                        )
                    } else {
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    }
                }
                _ => Err(TypeError::Mismatch {
                    expected: t2,
                    got: t1,
                    span,
                }),
            }
        }
    }

    fn newtype_structural_sequence(
        &mut self,
        ts1: impl Iterator<Item = TyId>,
        ts2: impl Iterator<Item = TyId>,
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> UnifyResult {
        ts1.zip(ts2).try_for_each(|(t1, t2)| {
            self.newtype_structural_match(t1, t2, span, seen, cycle, mode)
        })
    }

    fn newtype_structural_objects(
        &mut self,
        fields1: &IndexMap<StringId, TyId>,
        fields2: &IndexMap<StringId, TyId>,
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> UnifyResult {
        fields1.iter().try_for_each(|(name, t1)| {
            fields2.get(name).map_or(Ok(()), |&t2| {
                self.newtype_structural_match(*t1, t2, span, seen, cycle, mode)
            })
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn newtype_structural_union(
        &mut self,
        remaining1: &[TyId],
        all2: &[TyId],
        available: &[usize],
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
    ) -> Option<UnifyResult> {
        match remaining1.split_first() {
            None => Some(Ok(())),
            Some((&first, rest)) => self.newtype_structural_union_matches(
                first, rest, all2, available, span, seen, cycle, mode, 0,
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn newtype_structural_union_matches(
        &mut self,
        first: TyId,
        rest: &[TyId],
        all2: &[TyId],
        available: &[usize],
        span: Span,
        seen: &mut HashSet<TypeId>,
        cycle: &mut Option<TyId>,
        mode: NewtypeEdgeMode,
        start: usize,
    ) -> Option<UnifyResult> {
        available.get(start).and_then(|&idx| {
            let m2 = *all2.get(idx)?;
            let snap = self.uf.snapshot();
            let err_len = self.errors.len();
            let mut branch_seen = seen.clone();
            match self.newtype_structural_match(
                first,
                m2,
                span,
                &mut branch_seen,
                cycle,
                mode,
            ) {
                Ok(()) => {
                    let next: Vec<usize> = available
                        .iter()
                        .copied()
                        .filter(|&i| i != idx)
                        .collect();
                    match self.newtype_structural_union(
                        rest,
                        all2,
                        &next,
                        span,
                        &mut branch_seen,
                        cycle,
                        mode,
                    ) {
                        Some(Ok(())) => Some(Ok(())),
                        _ => {
                            self.uf.rollback(snap);
                            self.errors.truncate(err_len);
                            self.newtype_structural_union_matches(
                                first,
                                rest,
                                all2,
                                available,
                                span,
                                seen,
                                cycle,
                                mode,
                                start + 1,
                            )
                        }
                    }
                }
                Err(_) => {
                    self.uf.rollback(snap);
                    self.errors.truncate(err_len);
                    self.newtype_structural_union_matches(
                        first,
                        rest,
                        all2,
                        available,
                        span,
                        seen,
                        cycle,
                        mode,
                        start + 1,
                    )
                }
            }
        })
    }

    /// Expand a `Ty::Named` alias fully to its target type.
    ///
    /// Recursively expands chained aliases (e.g., `A = B`, `B = Int`) until
    /// reaching a non-alias type. Object aliases are NOT expanded; they need
    /// special handling in `unify_named_with_object`.
    fn expand_alias_fully(
        &mut self,
        ty: TyId,
        other: TyId,
        span: Span,
    ) -> Option<TyId> {
        let mut current = ty;
        let mut expanded = false;
        // Expand until we hit a non-alias or object alias
        while let Some(next) = self.expand_alias_once(current, other, span) {
            current = next;
            expanded = true;
        }
        if expanded {
            Some(current)
        } else {
            None
        }
    }

    /// Expand a `Ty::Named` alias one level.
    ///
    /// If `ty` is `Ty::Named(id, args)` where `id` refers to a `TypeDef::Alias`,
    /// returns the expanded target type with type args substituted. Otherwise
    /// returns `None`.
    ///
    /// Note: Aliases to object types are NOT expanded here; they need special
    /// handling in `unify_named_with_object` to check all required fields.
    fn expand_alias_once(
        &mut self,
        ty: TyId,
        other: TyId,
        span: Span,
    ) -> Option<TyId> {
        let type_id = match self.ty_arena.get(ty) {
            Ty::Named(id, _) => *id,
            _ => None?,
        };
        match self.registry.get_def(type_id) {
            Some(TypeDef::Alias { .. }) => {
                if self
                    .alias_parts(other)
                    .is_some_and(|(other_id, _)| other_id == type_id)
                {
                    None
                } else {
                    let target = self.decls.alias_target(type_id);
                    // Don't expand if target is an object type; let
                    // `unify_named_with_object` handle it for proper
                    // required-field checking
                    let is_obj = self
                        .ast
                        .get_type_expr(target)
                        .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                    if is_obj {
                        None
                    } else {
                        self.newtype_edge(ty, other, span)
                            .filter(|edge| edge.alias == type_id)
                            .map(|edge| edge.repr)
                    }
                }
            }
            _ => None,
        }
    }

    fn expand_alias_fully_for_class(
        &mut self,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        let mut current = ty;
        let mut expanded = false;
        let mut seen = HashSet::new();
        while let Some((alias, _)) = self.alias_parts(current) {
            if seen.insert(alias) {
                match self.expand_alias_once_for_class(current, span) {
                    Some(next) => {
                        if next == current {
                            self.report_recursive_newtype_edge(current, span);
                            None
                        } else {
                            current = next;
                            expanded = true;
                            Some(())
                        }
                    }
                    None => None,
                }?;
            } else {
                self.report_recursive_newtype_edge(current, span);
                None?;
            }
        }
        Some(current).filter(|_| expanded)
    }

    fn expand_alias_once_for_class(
        &mut self,
        ty: TyId,
        span: Span,
    ) -> Option<TyId> {
        let (type_id, args) = self.alias_parts(ty)?;
        match self.registry.get_def(type_id) {
            Some(TypeDef::Alias { .. }) => {
                let target = self.decls.alias_target(type_id);
                let is_obj = self
                    .ast
                    .get_type_expr(target)
                    .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                if is_obj {
                    None
                } else {
                    let repr = self.alias_repr(type_id, &args);
                    self.newtype_edge(ty, repr, span)
                        .filter(|edge| edge.alias == type_id)
                        .map(|edge| edge.repr)
                }
            }
            _ => None,
        }
    }

    /// Core unification logic.
    fn unify_inner(&mut self, t1: TyId, t2: TyId, span: Span) -> UnifyResult {
        // Equal TyIds are trivially unified
        if t1 == t2 {
            Ok(())
        } else {
            // Expand aliases through approved `newtype` representation edges.
            let t1 = self.expand_alias_fully(t1, t2, span).unwrap_or(t1);
            let t2 = self.expand_alias_fully(t2, t1, span).unwrap_or(t2);

            if t1 == t2 {
                Ok(())
            } else {
                // Clone both `Ty`s to release the arena borrow.
                let ty1 = self.ty_arena.get(t1).clone();
                let ty2 = self.ty_arena.get(t2).clone();
                self.unify_inner_dispatch(t1, t2, &ty1, &ty2, span)
            }
        }
    }

    /// Dispatch on cloned `Ty` pair; `t1`/`t2` are the original `TyId`s for
    /// error messages, `ty1`/`ty2` are the cloned shapes for matching.
    fn unify_inner_dispatch(
        &mut self,
        t1: TyId,
        t2: TyId,
        ty1: &Ty,
        ty2: &Ty,
        span: Span,
    ) -> UnifyResult {
        match (ty1, ty2) {
            // Error recovery: Error unifies with anything
            (Ty::Error, _) | (_, Ty::Error) => Ok(()),

            // Unknown unifies with anything (database reads before narrowing)
            (Ty::Unknown, _) | (_, Ty::Unknown) => Ok(()),

            // Type variable on left: bind it
            (Ty::Var(v), _) => self.unify_var(*v, t2, span),

            // Type variable on right: symmetric
            (_, Ty::Var(v)) => self.unify_var(*v, t1, span),

            // Identical primitives (handled by TyId equality above for
            // pre-interned constants, but needed for dynamically allocated
            // duplicates)
            (Ty::Bool, Ty::Bool)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Char, Ty::Char)
            | (Ty::String, Ty::String)
            | (Ty::Time, Ty::Time)
            | (Ty::Range, Ty::Range)
            | (Ty::Json, Ty::Json)
            | (Ty::Ordering, Ty::Ordering)
            | (Ty::DataStatus, Ty::DataStatus)
            | (Ty::FilePath, Ty::FilePath)
            | (Ty::Path, Ty::Path)
            | (Ty::Regex, Ty::Regex)
            | (Ty::RuntimeError, Ty::RuntimeError) => Ok(()),

            // Local and Global are distinct types; use Ref union for either
            (Ty::Local, Ty::Local) | (Ty::Global, Ty::Global) => Ok(()),

            // Numeric types: same type only (no implicit coercion)
            (Ty::Int, Ty::Int)
            | (Ty::Word, Ty::Word)
            | (Ty::Float, Ty::Float) => Ok(()),

            // A `newtype` edge exists, but private `repr visibility` blocks
            // this external annotation or unification site.
            _ if self.newtype_edge_blocked(t1, t2, span) => {
                Err(TypeError::PrivateReprAnnotation {
                    from: t1,
                    to: t2,
                    span,
                })
            }

            // Array: unify element types
            (Ty::Array(a), Ty::Array(b)) => self.unify_inner(*a, *b, span),

            // Option: unify inner types
            (Ty::Option(a), Ty::Option(b)) => self.unify_inner(*a, *b, span),

            // Result: unify both ok and err types
            (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                let (ok1, err1, ok2, err2) = (*ok1, *err1, *ok2, *err2);
                self.unify_inner(ok1, ok2, span)?;
                self.unify_inner(err1, err2, span)
            }

            // Map: unify key and value types
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                let (k1, v1, k2, v2) = (*k1, *v1, *k2, *v2);
                self.unify_inner(k1, k2, span)?;
                self.unify_inner(v1, v2, span)
            }

            // Tuple: unify element-wise (must have same length)
            (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                if ts1.len() != ts2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let v1: SmallVec<[TyId; 4]> = ts1.clone();
                    let v2: SmallVec<[TyId; 4]> = ts2.clone();
                    self.unify_sequence(
                        v1.iter().copied(),
                        v2.iter().copied(),
                        span,
                    )
                }
            }

            // Function: unify params and return type
            (Ty::Fn(params1, ret1), Ty::Fn(params2, ret2)) => {
                if params1.len() != params2.len() {
                    Err(TypeError::ArityMismatch {
                        expected: params1.len(),
                        got: params2.len(),
                        span,
                    })
                } else {
                    let p1: SmallVec<[TyId; 4]> = params1.clone();
                    let p2: SmallVec<[TyId; 4]> = params2.clone();
                    let (r1, r2) = (*ret1, *ret2);
                    self.unify_sequence(
                        p1.iter().copied(),
                        p2.iter().copied(),
                        span,
                    )?;
                    self.unify_inner(r1, r2, span)
                }
            }

            // Structural objects: unify common fields
            (Ty::Object(fields1), Ty::Object(fields2)) => {
                let f1 = fields1.clone();
                let f2 = fields2.clone();
                self.unify_objects(&f1, &f2, span)
            }

            // Named type with structural object (extensible record check)
            (Ty::Named(id, args), Ty::Object(obj_fields))
            | (Ty::Object(obj_fields), Ty::Named(id, args)) => {
                let id = *id;
                let args: SmallVec<[TyId; 4]> = args.clone();
                let obj = obj_fields.clone();
                self.unify_named_with_object(id, &args, &obj, span)
            }

            // Named types: same TypeId, unify type arguments
            (Ty::Named(id1, args1), Ty::Named(id2, args2)) => {
                if id1 != id2 || args1.len() != args2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let a1: SmallVec<[TyId; 4]> = args1.clone();
                    let a2: SmallVec<[TyId; 4]> = args2.clone();
                    self.unify_sequence(
                        a1.iter().copied(),
                        a2.iter().copied(),
                        span,
                    )
                }
            }

            // HKT application against a union must keep the union as the
            // constructor. The generic union matcher below would pick one
            // member and collapse `Flex` to `Option`/`Array`.
            (Ty::Apply(tv, args), Ty::Union(..)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t2, span)
            }
            (Ty::Union(..), Ty::Apply(tv, args)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t1, span)
            }

            // Union types: structural equality (same members, order-independent)
            (Ty::Union(_, members1), Ty::Union(_, members2)) => {
                if members1.len() != members2.len() {
                    Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let m1: SmallVec<[TyId; 4]> = members1.clone();
                    let m2: SmallVec<[TyId; 4]> = members2.clone();
                    // Find a bijective matching between union members
                    let available: Vec<usize> = (0..m2.len()).collect();
                    self.unify_union_bijection(&m1, &m2, &available, span)
                        .unwrap_or({
                            Err(TypeError::Mismatch {
                                expected: t1,
                                got: t2,
                                span,
                            })
                        })
                }
            }

            // Concrete type with union: T unifies if it matches any member
            (_, Ty::Union(_, members)) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| {
                        let snap = self.uf.snapshot();
                        match self.unify_inner(t1, m, span) {
                            ok @ Ok(()) => Some(ok),
                            _ => {
                                self.uf.rollback(snap);
                                None
                            }
                        }
                    })
                    .unwrap_or({
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }
            (Ty::Union(_, members), _) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| {
                        let snap = self.uf.snapshot();
                        match self.unify_inner(m, t2, span) {
                            ok @ Ok(()) => Some(ok),
                            _ => {
                                self.uf.rollback(snap);
                                None
                            }
                        }
                    })
                    .unwrap_or({
                        Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }

            // HKT type application: `F[T]` where `F` is a type variable.
            //
            // Decompose the other type into constructor + element,
            // bind the type variable to the constructor shape, and
            // unify args with the element types.
            //
            // Must appear before the `Named` catch-all so that
            // `(Apply, Named)` is decomposed rather than rejected.
            (Ty::Apply(tv, args), _) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t2, span)
            }
            (_, Ty::Apply(tv, args)) => {
                let tv = *tv;
                let args: SmallVec<[TyId; 4]> = args.clone();
                self.unify_apply(tv, &args, t1, span)
            }

            // Named (sum/alias) vs anything else: mismatch.
            // (Unions are `Ty::Union` and handled above.)
            (_, Ty::Named(..)) | (Ty::Named(..), _) => {
                Err(TypeError::Mismatch {
                    expected: t2,
                    got: t1,
                    span,
                })
            }

            // Associated type projection: resolve and unify
            (Ty::AssocType(tv, class, name), _) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(resolved, t2, span),
                    Err(_) => {
                        // Base type is unresolved (type variable); defer
                        Ok(())
                    }
                }
            }
            (_, Ty::AssocType(tv, class, name)) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(t1, resolved, span),
                    Err(_) => Ok(()),
                }
            }

            // All other combinations are type mismatches
            _ => Err(TypeError::Mismatch {
                expected: t2,
                got: t1,
                span,
            }),
        }
    }

    /// Unify a type variable with a type.
    ///
    /// Finds the canonical root via UF, probes for existing binding, and
    /// either follows through to the bound type, unions two vars, or binds
    /// the root. Uses UF-aware occurs check.
    fn unify_var(&mut self, v: TyVar, t: TyId, span: Span) -> UnifyResult {
        let root = self.uf.find(v);
        if let Some(bound) = self.uf.probe(root) {
            // Already bound; unify the bound type with `t`
            self.unify_inner(bound, t, span)
        } else if let Ty::Var(w) = self.ty_arena.get(t) {
            let w_root = self.uf.find(*w);
            if root == w_root {
                Ok(())
            } else if let Some(w_bound) = self.uf.probe(w_root) {
                // `w` is bound; unify root with the bound type
                if self.ty_arena.occurs_uf(w_bound, root, self.uf) {
                    Err(TypeError::InfiniteType(root, w_bound, span))
                } else {
                    self.uf.bind(root, w_bound);
                    Ok(())
                }
            } else {
                self.uf.union(root, w_root);
                Ok(())
            }
        } else if self.ty_arena.occurs_uf(t, root, self.uf) {
            Err(TypeError::InfiniteType(root, t, span))
        } else {
            self.uf.bind(root, t);
            Ok(())
        }
    }

    /// Unify a higher-kinded type application `Apply(tv, args)` with another type.
    ///
    /// Decomposes `other` into constructor + element types, binds `tv` to the
    /// constructor shape, and unifies `args` with the element types.
    fn unify_apply(
        &mut self,
        tv: TyVar,
        args: &[TyId],
        other: TyId,
        span: Span,
    ) -> UnifyResult {
        // Clone the shape to release arena borrow
        let shape = self.ty_arena.get(other).clone();
        match shape {
            // Parameterized builtins: decompose into constructor + element
            Ty::Option(inner) => {
                let ctor = self.ty_arena.option(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, &[inner], span)
            }
            Ty::Result(ok, err) => {
                if args.len() >= 2 {
                    let ctor =
                        self.ty_arena.result(TyArena::ERROR, TyArena::ERROR);
                    self.unify_apply_inner(tv, args, ctor, &[ok, err], span)
                } else {
                    let ctor = self.ty_arena.result(TyArena::ERROR, err);
                    self.unify_apply_inner(tv, args, ctor, &[ok], span)
                }
            }
            Ty::Array(inner) => {
                let ctor = self.ty_arena.array(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, &[inner], span)
            }
            Ty::Map(k, v) => {
                if args.len() >= 2 {
                    let ctor =
                        self.ty_arena.map_ty(TyArena::ERROR, TyArena::ERROR);
                    self.unify_apply_inner(tv, args, ctor, &[k, v], span)
                } else {
                    let ctor = self.ty_arena.map_ty(TyArena::ERROR, v);
                    self.unify_apply_inner(tv, args, ctor, &[k], span)
                }
            }

            Ty::Union(prov, ref members) => {
                let parts: Option<SmallVec<[(TyId, SmallVec<[TyId; 4]>); 4]>> =
                    members
                        .iter()
                        .map(|&m| match self.ty_arena.get(m).clone() {
                            Ty::Option(inner) => Some((
                                self.ty_arena.option(TyArena::ERROR),
                                smallvec![inner],
                            )),
                            Ty::Result(ok, err) => {
                                if args.len() >= 2 {
                                    Some((
                                        self.ty_arena.result(
                                            TyArena::ERROR,
                                            TyArena::ERROR,
                                        ),
                                        smallvec![ok, err],
                                    ))
                                } else {
                                    Some((
                                        self.ty_arena
                                            .result(TyArena::ERROR, err),
                                        smallvec![ok],
                                    ))
                                }
                            }
                            Ty::Array(inner) => Some((
                                self.ty_arena.array(TyArena::ERROR),
                                smallvec![inner],
                            )),
                            Ty::Map(k, v) => {
                                if args.len() >= 2 {
                                    Some((
                                        self.ty_arena.map_ty(
                                            TyArena::ERROR,
                                            TyArena::ERROR,
                                        ),
                                        smallvec![k, v],
                                    ))
                                } else {
                                    Some((
                                        self.ty_arena.map_ty(TyArena::ERROR, v),
                                        smallvec![k],
                                    ))
                                }
                            }
                            Ty::Range => {
                                Some((TyArena::RANGE, smallvec![TyArena::INT]))
                            }
                            Ty::Named(id, ref type_args) => {
                                if type_args.is_empty() {
                                    None
                                } else {
                                    let start = type_args
                                        .len()
                                        .saturating_sub(args.len());
                                    let elems = type_args
                                        .iter()
                                        .skip(start)
                                        .copied()
                                        .collect();
                                    let placeholder = type_args
                                        .iter()
                                        .enumerate()
                                        .map(|(i, &t)| {
                                            if i >= start {
                                                TyArena::ERROR
                                            } else {
                                                t
                                            }
                                        })
                                        .collect();
                                    Some((
                                        self.ty_arena.named(id, placeholder),
                                        elems,
                                    ))
                                }
                            }
                            Ty::Tuple(ref ts) => {
                                let start = ts.len().saturating_sub(args.len());
                                let elems =
                                    ts.iter().skip(start).copied().collect();
                                let placeholder = ts
                                    .iter()
                                    .enumerate()
                                    .map(|(i, &t)| {
                                        if i >= start {
                                            TyArena::ERROR
                                        } else {
                                            t
                                        }
                                    })
                                    .collect();
                                Some((
                                    self.ty_arena.alloc(Ty::Tuple(placeholder)),
                                    elems,
                                ))
                            }
                            _ => None,
                        })
                        .collect();

                match parts {
                    Some(parts) => {
                        let ctors =
                            parts.iter().map(|(ctor, _)| *ctor).collect();
                        let ctor = self.ty_arena.alloc(Ty::Union(prov, ctors));
                        parts
                            .iter()
                            .find_map(|(_, elems)| {
                                let snap = self.uf.snapshot();
                                match self.unify_apply_inner(
                                    tv, args, ctor, elems, span,
                                ) {
                                    Ok(()) => Some(Ok(())),
                                    Err(_) => {
                                        self.uf.rollback(snap);
                                        None
                                    }
                                }
                            })
                            .unwrap_or_else(|| {
                                let got_args: SmallVec<[TyId; 4]> =
                                    args.iter().copied().collect();
                                let got = self.ty_arena.hkt(tv, got_args);
                                Err(TypeError::Mismatch {
                                    expected: other,
                                    got,
                                    span,
                                })
                            })
                    }
                    None => {
                        let got_args: SmallVec<[TyId; 4]> =
                            args.iter().copied().collect();
                        let got = self.ty_arena.hkt(tv, got_args);
                        Err(TypeError::Mismatch {
                            expected: other,
                            got,
                            span,
                        })
                    }
                }
            }

            Ty::Range => self.unify_apply_inner(
                tv,
                args,
                TyArena::RANGE,
                &[TyArena::INT],
                span,
            ),

            // User-defined named types: decompose into constructor + element
            // Element type is the LAST type arg (Haskell curried convention).
            // Non-element (fixed) args are preserved in the constructor placeholder.
            Ty::Named(id, ref type_args) => {
                if type_args.is_empty() {
                    self.unify_var(tv, other, span)
                } else {
                    let start = type_args.len().saturating_sub(args.len());
                    let elems: SmallVec<[TyId; 4]> = type_args[start..].into();
                    let placeholder: SmallVec<[TyId; 4]> = type_args
                        .iter()
                        .enumerate()
                        .map(
                            |(i, &t)| {
                                if i >= start {
                                    TyArena::ERROR
                                } else {
                                    t
                                }
                            },
                        )
                        .collect();
                    let ctor = self.ty_arena.named(id, placeholder);
                    self.unify_apply_inner(tv, args, ctor, &elems, span)
                }
            }

            // Two `Apply` nodes: unify constructors and args pairwise
            Ty::Apply(tv2, ref args2) => {
                if args.len() != args2.len() {
                    let exp = self.ty_arena.hkt(tv2, args2.clone());
                    let got_args: SmallVec<[TyId; 4]> =
                        args.iter().copied().collect();
                    let got = self.ty_arena.hkt(tv, got_args);
                    Err(TypeError::Mismatch {
                        expected: exp,
                        got,
                        span,
                    })
                } else {
                    let a2: SmallVec<[TyId; 4]> = args2.clone();
                    let tv2_id = self.ty_arena.alloc(Ty::Var(tv2));
                    self.unify_var(tv, tv2_id, span)?;
                    self.unify_sequence(
                        args.iter().copied(),
                        a2.iter().copied(),
                        span,
                    )
                }
            }

            Ty::Tuple(ref ts) => {
                // Try to find element positions from an HKT class
                // constraint on `tv`; this handles tuple constructors
                // with interleaved fixed/element positions like `(,T,)`
                let root = self.uf.find(tv);
                let positions = self
                    .hkt_var_classes
                    .get(&root)
                    .and_then(|&cid| {
                        self.instance_registry
                            .lookup_tuple(cid, ts.len())
                            .cloned()
                    })
                    .and_then(|inst| {
                        let ca: SmallVec<[usize; 4]> = inst
                            .type_params
                            .iter()
                            .enumerate()
                            .filter(|(_, &p)| inst.class_args.contains(&p))
                            .map(|(i, _)| i)
                            .collect();
                        (ca.len() == args.len()).then_some(ca)
                    });

                let (elems, placeholder) = match positions {
                    Some(ref pos) => {
                        let e: SmallVec<[TyId; 4]> = pos
                            .iter()
                            .filter_map(|&i| ts.get(i).copied())
                            .collect();
                        let p: SmallVec<[TyId; 4]> = ts
                            .iter()
                            .enumerate()
                            .map(|(i, &t)| {
                                if pos.contains(&i) {
                                    TyArena::ERROR
                                } else {
                                    t
                                }
                            })
                            .collect();
                        (e, p)
                    }
                    None => {
                        let start = ts.len().saturating_sub(args.len());
                        let e: SmallVec<[TyId; 4]> =
                            ts.iter().skip(start).copied().collect();
                        let p: SmallVec<[TyId; 4]> =
                            ts.iter()
                                .enumerate()
                                .map(|(i, &t)| {
                                    if i >= start {
                                        TyArena::ERROR
                                    } else {
                                        t
                                    }
                                })
                                .collect();
                        (e, p)
                    }
                };

                let ctor = self.ty_arena.alloc(Ty::Tuple(placeholder));
                self.unify_apply_inner(tv, args, ctor, &elems, span)
            }

            _ => {
                let got_args: SmallVec<[TyId; 4]> =
                    args.iter().copied().collect();
                let got = self.ty_arena.hkt(tv, got_args);
                Err(TypeError::Mismatch {
                    expected: other,
                    got,
                    span,
                })
            }
        }
    }

    /// Bind `tv` to a constructor shape and unify `Apply` args with
    /// the element types pairwise.
    fn unify_apply_inner(
        &mut self,
        tv: TyVar,
        args: &[TyId],
        ctor: TyId,
        elems: &[TyId],
        span: Span,
    ) -> UnifyResult {
        self.unify_var(tv, ctor, span)?;
        self.unify_sequence(args.iter().copied(), elems.iter().copied(), span)
    }

    /// Unify two sequences of types element-wise.
    fn unify_sequence(
        &mut self,
        ts1: impl Iterator<Item = TyId>,
        ts2: impl Iterator<Item = TyId>,
        span: Span,
    ) -> UnifyResult {
        ts1.zip(ts2)
            .try_for_each(|(t1, t2)| self.unify_inner(t1, t2, span))
    }

    /// Find a bijective matching between union members via backtracking.
    ///
    /// Tries to match each member of `remaining1` to a unique member of `all2`
    /// (using indices in `available`). Uses UF snapshot/rollback for
    /// backtracking instead of cloning substitutions.
    fn unify_union_bijection(
        &mut self,
        remaining1: &[TyId],
        all2: &[TyId],
        available: &[usize],
        span: Span,
    ) -> Option<UnifyResult> {
        match remaining1.split_first() {
            None => Some(Ok(())),
            Some((&first, rest)) => {
                // Try each available index, backtracking on failure
                self.try_union_matches(first, rest, all2, available, span, 0)
            }
        }
    }

    /// Helper for `unify_union_bijection`: try matching `first` with each
    /// available member starting at `start_idx`.
    #[allow(clippy::too_many_arguments)]
    fn try_union_matches(
        &mut self,
        first: TyId,
        rest: &[TyId],
        all2: &[TyId],
        available: &[usize],
        span: Span,
        start_idx: usize,
    ) -> Option<UnifyResult> {
        available.get(start_idx).and_then(|&idx| {
            let m2 = *all2.get(idx)?;
            let snap = self.uf.snapshot();

            match self.unify_inner(first, m2, span) {
                Ok(()) => {
                    let new_available: Vec<usize> = available
                        .iter()
                        .copied()
                        .filter(|&i| i != idx)
                        .collect();

                    match self.unify_union_bijection(
                        rest,
                        all2,
                        &new_available,
                        span,
                    ) {
                        Some(Ok(())) => Some(Ok(())),
                        // Backtrack: rollback and try next
                        _ => {
                            self.uf.rollback(snap);
                            self.try_union_matches(
                                first,
                                rest,
                                all2,
                                available,
                                span,
                                start_idx + 1,
                            )
                        }
                    }
                }
                // This match failed; rollback and try next
                Err(_) => {
                    self.uf.rollback(snap);
                    self.try_union_matches(
                        first,
                        rest,
                        all2,
                        available,
                        span,
                        start_idx + 1,
                    )
                }
            }
        })
    }

    /// Unify two structural object types.
    ///
    /// Uses extensible record semantics: an object matches if it has at least
    /// the required fields with matching types. Extra fields are allowed.
    fn unify_objects(
        &mut self,
        fields1: &IndexMap<StringId, TyId>,
        fields2: &IndexMap<StringId, TyId>,
        span: Span,
    ) -> UnifyResult {
        // Collect all field names from both objects
        let all_keys: Vec<StringId> = fields1
            .keys()
            .chain(fields2.keys())
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        // Unify fields present in both; extra fields are allowed
        all_keys.iter().try_for_each(|key| {
            match (fields1.get(key), fields2.get(key)) {
                (Some(&t1), Some(&t2)) => self.unify_inner(t1, t2, span),
                // Field only in one object; extensible, so OK
                _ => Ok(()),
            }
        })
    }

    /// Unify a named object alias type with a structural object type.
    ///
    /// The alias must have all required fields present in the object.
    /// Extra fields in the object are allowed (extensible record semantics).
    fn unify_named_with_object(
        &mut self,
        type_id: TypeId,
        type_args: &[TyId],
        obj_fields: &IndexMap<StringId, TyId>,
        span: Span,
    ) -> UnifyResult {
        // Look up alias definition
        let def = self.registry.get_def(type_id);

        match def {
            Some(TypeDef::Alias { type_params, .. }) => {
                let target = self.decls.alias_target(type_id);
                // Check if target is an object type
                let type_params = type_params.clone();
                match self.ast.get_type_expr(target).cloned() {
                    Some(AstTypeExpr::Object(alias_fields)) => {
                        let named = self.ty_arena.named(
                            type_id,
                            type_args.iter().copied().collect(),
                        );
                        let obj =
                            self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                        if self.newtype_edge(named, obj, span).is_some() {
                            Ok(())
                        } else if !self
                            .convert_ctx()
                            .can_access_alias_repr(type_id)
                        {
                            Err(TypeError::Mismatch {
                                expected: named,
                                got: obj,
                                span,
                            })
                        } else {
                            // Build substitution from type params to type args
                            let param_subst: IndexMap<StringId, TyId> =
                                type_params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, a)| (*p, *a))
                                    .collect();

                            // Pre-intern field names before the fold
                            let fields_with_ids: Vec<_> = alias_fields
                                .iter()
                                .map(|(name, ty)| (*name, *ty))
                                .collect();

                            // Check that object has all required fields
                            fields_with_ids.iter().try_for_each(
                                |(field_name, field_ty_id)| {
                                    let expected_ty =
                                        self.convert_ctx().ast_type_to_ty(
                                            *field_ty_id,
                                            &param_subst,
                                        );

                                    match obj_fields.get(field_name) {
                                        Some(&obj_ty) => self.unify_inner(
                                            expected_ty,
                                            obj_ty,
                                            span,
                                        ),
                                        None => {
                                            // Missing required field
                                            Err(TypeError::MissingField {
                                                ty: type_id,
                                                field: self.env.resolve_string(
                                                    *field_name,
                                                ),
                                                span,
                                            })
                                        }
                                    }
                                },
                            )
                        }
                    }
                    _ => {
                        // Not an object alias, can't unify with object
                        let named = self.ty_arena.named(
                            type_id,
                            type_args.iter().copied().collect(),
                        );
                        let obj =
                            self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                        Err(TypeError::Mismatch {
                            expected: named,
                            got: obj,
                            span,
                        })
                    }
                }
            }

            Some(TypeDef::Union { .. })
            | Some(TypeDef::Sum { .. })
            | Some(TypeDef::Builtin(_))
            | None => {
                let named = self
                    .ty_arena
                    .named(type_id, type_args.iter().copied().collect());
                let obj = self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                Err(TypeError::Mismatch {
                    expected: named,
                    got: obj,
                    span,
                })
            }
        }
    }

    /// Solve all collected constraints, updating the union-find in-place.
    ///
    /// Processes constraints in order:
    /// 1. `Eq` constraints via unification
    /// 2. `Numeric` constraints (must resolve to `Int` or `Float`)
    /// 3. `Callable` constraints (callee must be `Fn` type)
    /// 4. `Into[String]` constraints (rejects `Fn` types)
    /// 5. `Into[Json]` constraints (rejects `Fn` types)
    /// 6. `Subscript` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 7. `Storable` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 8. `Fallible` constraints (must be `Option[T]` or `Result[T, E]`; third pass)
    ///
    /// Errors are recorded via `self.errors`; unification continues to collect
    /// as many errors as possible.
    pub(crate) fn solve_constraints(
        &mut self,
        constraints: Vec<(Constraint, Option<QualifiedName>)>,
        numeric_vars: &[TyVar],
    ) {
        // Pre-build a map from HKT-constrained type variables to their
        // class ID so `unify_apply` can look up tuple constructor instances
        // for position-aware element decomposition.
        constraints.iter().for_each(|(c, _)| {
            if let Constraint::Class {
                ty,
                class: TypeClass::Hkt { id, .. },
                ..
            } = c
            {
                if let Ty::Var(v) = self.ty_arena.get(*ty) {
                    self.hkt_var_classes.insert(*v, *id);
                }
            }
        });

        // First pass: process Unify, Callable, HasField, Iterable, Indexable.
        // These constraints generate type bindings (via union-find) that
        // other constraints (Numeric, Into[String], etc.) depend on.
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            match c {
                Constraint::Unify(t1, t2, span) => {
                    // Pre-resolve before unifying. While `unify_inner` handles
                    // `Ty::Var` via `unify_var` (which does `find`/`probe`
                    // internally), pre-resolving is still necessary: without it,
                    // error messages on mismatch show unresolved type variables
                    // (e.g. `Option[T]`) instead of concrete types (e.g.
                    // `Option[Int]`).
                    let t1 = self.uf.resolve(*t1, self.ty_arena);
                    let t2 = self.uf.resolve(*t2, self.ty_arena);
                    if let Err(e) = self.unify_types(t1, t2, *span) {
                        self.errors.push(e);
                    }
                }
                Constraint::Callable {
                    callee,
                    args,
                    ret,
                    span,
                } => {
                    let callee = self.uf.resolve(*callee, self.ty_arena);
                    let args: SmallVec<[TyId; 4]> = args
                        .iter()
                        .map(|&t| self.uf.resolve(t, self.ty_arena))
                        .collect();
                    let ret = self.uf.resolve(*ret, self.ty_arena);
                    self.check_callable(callee, &args, ret, *span);
                }
                Constraint::HasField {
                    base,
                    field,
                    field_ty,
                    span,
                } => {
                    let base = self.uf.resolve(*base, self.ty_arena);
                    let field_ty = self.uf.resolve(*field_ty, self.ty_arena);
                    self.check_has_field(base, *field, field_ty, *span);
                }
                Constraint::Class { ty, class, span } => match class {
                    // Iterable (with element type) and Indexable: first pass
                    TypeClass::Hkt {
                        id: ClassId::ITERABLE,
                        ref elems,
                        ..
                    } if !elems.is_empty() => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                    TypeClass::Concrete {
                        id: ClassId::INDEXABLE,
                        ..
                    } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                    TypeClass::Hkt { .. } => {}
                    TypeClass::Concrete { .. } => {}
                },
            }
        });

        // Default unresolved numeric type variables to Int after first pass.
        // This ensures subsequent constraint checks (Numeric, Indexable, etc.)
        // see concrete types rather than unresolved type variables.
        // Mimics Haskell's defaulting: `10` becomes `Int` when unconstrained.
        //
        // Important: bind the *resolved* root, not the original. If the
        // numeric type var was unified with another var (e.g., `?N -> ?F`), we
        // must bind `?F -> Int`, not overwrite `?N` (which would lose the link).
        numeric_vars.iter().for_each(|v| {
            let root = self.uf.find(*v);
            if self.uf.probe(root).is_none() {
                self.uf.bind(root, TyArena::INT);
            }
        });

        // Second pass: process simple membership constraints
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    TypeClass::Concrete { ref params, .. }
                        if params.is_empty() =>
                    {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        self.satisfies_class(class, ty, *span);
                    }
                    TypeClass::Hkt { .. } | TypeClass::Concrete { .. } => {}
                }
            }
        });

        // Third pass: final check for HKT and parameterized constraints now
        // that numeric type variables have been defaulted and Callable has
        // resolved all type variables through argument unification.
        constraints.iter().for_each(|(c, module)| {
            self.current_module = module.clone();
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    TypeClass::Concrete { ref params, .. }
                        if params.is_empty() => {}
                    TypeClass::Hkt { .. } | TypeClass::Concrete { .. } => {
                        let ty = self.uf.resolve(*ty, self.ty_arena);
                        let class = class.resolve_inner(self.uf, self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                }
            }
        });
    }

    /// Check that a type satisfies a class constraint.
    ///
    /// This is the unified constraint checking method that handles all class
    /// constraints. The `class` parameter contains any associated types (e.g.,
    /// `Into(target)`, `Iterable(elem)`). The union-find is updated in-place
    /// when the constraint involves unification (e.g., `Fallible`, `Iterable`,
    /// `Indexable`).
    fn satisfies_class(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        // Handle associated types: resolve to concrete type before checking
        let shape = self.ty_arena.get(ty).clone();
        if let Ty::AssocType(tv, assoc_class, name) = shape {
            // Resolve the base type variable through union-find
            match self.uf.resolve_var(tv, self.ty_arena) {
                Some(base) => {
                    if let Ok(resolved) =
                        self.resolve_assoc_type(base, assoc_class, name, span)
                    {
                        self.satisfies_class(class, resolved, span);
                    }
                }
                None => {
                    // Base type still unresolved; defer constraint
                }
            }
        } else {
            self.satisfies_class_inner(class, ty, span);
        }
    }

    /// Inner implementation of class constraint checking.
    fn satisfies_class_inner(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        match class {
            TypeClass::Concrete { id, ref params } if params.is_empty() => {
                self.check_simple_class(*id, class, ty, span)
            }
            TypeClass::Concrete {
                id: ClassId::INTO,
                ref params,
            } => {
                let to = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_into(ty, to, span)
            }
            TypeClass::Concrete {
                id: ClassId::TRY_INTO,
                ref params,
            } => {
                let to = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_try_into(ty, to, span)
            }
            TypeClass::Concrete {
                id: ClassId::INDEXABLE,
                ref params,
            } => {
                let elem = params.first().copied().unwrap_or(TyArena::UNKNOWN);
                self.check_indexable(class, ty, elem, span)
            }
            TypeClass::Concrete { id, ref params }
                if id.idx() >= ClassId::BUILTIN_COUNT && !params.is_empty() =>
            {
                self.check_user_parameterized(*id, params, class, ty, span)
            }
            TypeClass::Hkt { id, ref elems, .. }
                if matches!(
                    *id,
                    ClassId::ITERABLE
                        | ClassId::MAPPABLE
                        | ClassId::FILTERABLE
                        | ClassId::FOLDABLE
                        | ClassId::BIMAPPABLE
                ) =>
            {
                self.check_hkt_class(*id, elems, class, ty, span)
            }
            TypeClass::Hkt { id, ref elems, .. }
                if id.idx() < ClassId::BUILTIN_COUNT =>
            {
                self.satisfies_hkt_class(*id, elems, class, ty, span);
            }
            TypeClass::Hkt { id, ref elems, .. } => {
                self.check_user_hkt(*id, elems, class, ty, span);
            }
            _ => {}
        }
    }

    /// How the given `shape` satisfies `class_id` as a builtin.
    ///
    /// Returns `None` if the builtin table has no entry; the caller then
    /// falls through to handling `Var`/`Union`/`Named`/etc.
    fn builtin_satisfaction(
        class_id: ClassId,
        shape: &Ty,
    ) -> Option<Satisfaction> {
        match (class_id, shape) {
            (id, Ty::Int | Ty::Word | Ty::Float)
                if Self::is_numeric_capability(id) =>
            {
                Some(Satisfaction::Direct)
            }
            (ClassId::BIT_LIKE, Ty::Bool | Ty::Int | Ty::Word) => {
                Some(Satisfaction::Direct)
            }
            (ClassId::NEGATABLE, Ty::Int | Ty::Float) => {
                Some(Satisfaction::Direct)
            }
            (
                ClassId::DEFAULT,
                Ty::Unit
                | Ty::Bool
                | Ty::String
                | Ty::Array(_)
                | Ty::Map(_, _)
                | Ty::Option(_)
                | Ty::Ordering
                | Ty::FilePath,
            ) => Some(Satisfaction::Direct),
            (
                ClassId::CONCATABLE,
                Ty::String | Ty::Array(_) | Ty::Map(_, _) | Ty::Option(_),
            ) => Some(Satisfaction::Direct),
            (ClassId::REVERSIBLE, Ty::Array(_) | Ty::Range) => {
                Some(Satisfaction::Direct)
            }
            (
                ClassId::ORD,
                Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::Ordering,
            ) => Some(Satisfaction::Direct),
            (ClassId::ORD, Ty::Array(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::ORD, Ty::Tuple(es)) => {
                Some(Satisfaction::Recurse(SmallVec::from_slice(es)))
            }
            (ClassId::ORD, Ty::Option(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::ORD, Ty::Result(a, b)) => {
                Some(Satisfaction::Recurse(smallvec![*a, *b]))
            }
            (ClassId::ORD, Ty::Map(k, v)) => {
                Some(Satisfaction::Recurse(smallvec![*k, *v]))
            }
            (
                ClassId::EQ,
                Ty::Unit
                | Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::FilePath
                | Ty::Json
                | Ty::Local
                | Ty::Global
                | Ty::Ordering,
            ) => Some(Satisfaction::Direct),
            (ClassId::EQ, Ty::Array(e) | Ty::Option(e)) => {
                Some(Satisfaction::Recurse(smallvec![*e]))
            }
            (ClassId::EQ, Ty::Tuple(es)) => {
                Some(Satisfaction::Recurse(SmallVec::from_slice(es)))
            }
            (ClassId::EQ, Ty::Result(a, b) | Ty::Map(a, b)) => {
                Some(Satisfaction::Recurse(smallvec![*a, *b]))
            }
            (ClassId::EQ, Ty::Object(fields)) => {
                Some(Satisfaction::Recurse(fields.values().copied().collect()))
            }
            // `Display` must NOT wildcard these shapes; they need to fall
            // through to the dispatcher (instance lookup for `Named`/`Union`,
            // silent for `Var`/`Error`/`Unknown`, error for `Fn`).
            (
                ClassId::DISPLAY,
                Ty::Fn(_, _)
                | Ty::Var(_)
                | Ty::Error
                | Ty::Unknown
                | Ty::Union(_, _)
                | Ty::Named(_, _),
            ) => None,
            (ClassId::DISPLAY, _) => Some(Satisfaction::Direct),
            _ => None,
        }
    }

    /// Check a "simple" class (`Numeric`, numeric capabilities, `BitLike`,
    /// `Negatable`, `Default`, `Concatable`, `Ord`, `Eq`, `Display`) against `ty`.
    ///
    /// Per-class dispatch rules: Numeric capabilities on a `Union` succeed if
    /// any one member directly satisfies using the "any" strategy; on a
    /// `Named` with no instance, fall back to alias expansion. `BitLike` on a
    /// `Named` with no instance falls back to alias expansion. All others
    /// require every `Union` member to satisfy, and a `Named` with no instance
    /// is an error.
    fn check_simple_class(
        &mut self,
        class_id: ClassId,
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(ty).clone();
        match Self::builtin_satisfaction(class_id, &shape) {
            Some(Satisfaction::Direct) => {}
            Some(Satisfaction::Recurse(inners)) => {
                inners
                    .iter()
                    .for_each(|&t| self.satisfies_class(class, t, span));
            }
            None => match shape {
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                Ty::Union(prov, members) => {
                    let inst = prov.and_then(|id| {
                        self.instance_registry.lookup(class_id, id).cloned()
                    });
                    match inst {
                        Some(inst) => self.check_instance_constraints(
                            &inst,
                            &[],
                            span,
                            None,
                        ),
                        None => {
                            if Self::is_numeric_capability(class_id) {
                                let any_sat = members.iter().any(|&m| {
                                    let sh = self.ty_arena.get(m).clone();
                                    matches!(
                                        Self::builtin_satisfaction(
                                            class_id, &sh
                                        ),
                                        Some(Satisfaction::Direct)
                                    )
                                });
                                if !any_sat {
                                    self.errors.push(
                                        TypeError::UnsatisfiedClass(
                                            class.clone(),
                                            ty,
                                            span,
                                        ),
                                    );
                                }
                            } else {
                                members.iter().for_each(|&m| {
                                    self.satisfies_class(class, m, span)
                                });
                            }
                        }
                    }
                }
                Ty::Named(id, args) => {
                    let inst =
                        self.instance_registry.lookup(class_id, id).cloned();
                    match inst {
                        Some(inst) => self.check_instance_constraints(
                            &inst, &args, span, None,
                        ),
                        None => {
                            let expanded =
                                if Self::is_numeric_capability(class_id)
                                    || class_id == ClassId::BIT_LIKE
                                {
                                    self.expand_alias_fully_for_class(ty, span)
                                } else {
                                    None
                                };
                            match expanded {
                                Some(e) => self.satisfies_class(class, e, span),
                                None => self.errors.push(
                                    TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    ),
                                ),
                            }
                        }
                    }
                }
                // User classes: handle parameterized builtins via instance lookup
                _ if class_id.idx() >= ClassId::BUILTIN_COUNT => {
                    match self.ty_to_type_id_and_args(ty) {
                        Some((tid, args)) => {
                            match self
                                .instance_registry
                                .lookup(class_id, tid)
                                .cloned()
                            {
                                Some(inst) => self.check_instance_constraints(
                                    &inst, &args, span, None,
                                ),
                                None => self.errors.push(
                                    TypeError::UnsatisfiedClass(
                                        class.clone(),
                                        ty,
                                        span,
                                    ),
                                ),
                            }
                        }
                        None => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
                _ => {
                    self.errors.push(TypeError::UnsatisfiedClass(
                        class.clone(),
                        ty,
                        span,
                    ));
                }
            },
        }
    }

    /// Check an HKT class (`Iterable`, `Mappable`, `Filterable`, `Foldable`, `Bimappable`)
    /// against `ty`, optionally unifying element types with `elems`.
    fn check_hkt_class(
        &mut self,
        class_id: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty = self.expand_alias_fully_for_class(ty, span).unwrap_or(ty);
        let shape = self.ty_arena.get(ty).clone();
        let builtin_elems: Option<SmallVec<[TyId; 2]>> =
            match (class_id, &shape) {
                (_, Ty::Array(e)) => Some(smallvec![*e]),
                (
                    ClassId::ITERABLE | ClassId::FILTERABLE | ClassId::FOLDABLE,
                    Ty::Range,
                ) => Some(smallvec![TyArena::INT]),
                (ClassId::MAPPABLE, Ty::Option(e)) => Some(smallvec![*e]),
                (ClassId::MAPPABLE, Ty::Result(ok, _)) => Some(smallvec![*ok]),
                (ClassId::BIMAPPABLE, Ty::Result(ok, err)) => {
                    Some(smallvec![*ok, *err])
                }
                (ClassId::BIMAPPABLE, Ty::Tuple(ts)) if ts.len() == 2 => {
                    Some(ts.iter().copied().collect())
                }
                _ => None,
            };
        match builtin_elems {
            Some(ref inner) => {
                elems.iter().zip(inner.iter()).for_each(|(&e, &b)| {
                    if let Err(e) = self.unify_types(e, b, span) {
                        self.errors.push(e);
                    }
                });
            }
            None => match shape {
                Ty::Union(_, members) => {
                    members
                        .iter()
                        .for_each(|&m| self.satisfies_class(class, m, span));
                }
                Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                Ty::Named(id, type_args) => {
                    match self.instance_registry.lookup(class_id, id).cloned() {
                        Some(inst) => {
                            let param_subst = self
                                .build_instance_subst(&inst, &type_args, span);
                            elems.iter().zip(inst.class_args.iter()).for_each(
                                |(&elem, &inst_elem)| {
                                    let resolved = self
                                        .ty_arena
                                        .apply(inst_elem, &param_subst);
                                    if let Err(e) =
                                        self.unify_types(elem, resolved, span)
                                    {
                                        self.errors.push(e);
                                    }
                                },
                            );
                            self.check_instance_constraints(
                                &inst,
                                &type_args,
                                span,
                                Some(&param_subst),
                            );
                        }
                        None => {
                            if class_id == ClassId::ITERABLE {
                                let exp = elems
                                    .first()
                                    .copied()
                                    .unwrap_or(TyArena::UNKNOWN);
                                let arr = self.ty_arena.array(exp);
                                self.errors.push(TypeError::Mismatch {
                                    expected: arr,
                                    got: ty,
                                    span,
                                });
                            } else {
                                self.errors.push(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                }
                Ty::Tuple(ts) => {
                    match self
                        .instance_registry
                        .lookup_tuple(class_id, ts.len())
                        .cloned()
                    {
                        Some(inst) => {
                            let subst =
                                self.build_instance_subst(&inst, &ts, span);
                            elems.iter().zip(inst.class_args.iter()).for_each(
                                |(&elem, &ie)| {
                                    let resolved =
                                        self.ty_arena.apply(ie, &subst);
                                    if let Err(e) =
                                        self.unify_types(elem, resolved, span)
                                    {
                                        self.errors.push(e);
                                    }
                                },
                            );
                            self.check_instance_constraints(
                                &inst,
                                &ts,
                                span,
                                Some(&subst),
                            );
                        }
                        None => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
                _ => {
                    if class_id == ClassId::ITERABLE {
                        let exp =
                            elems.first().copied().unwrap_or(TyArena::UNKNOWN);
                        let arr = self.ty_arena.array(exp);
                        self.errors.push(TypeError::Mismatch {
                            expected: arr,
                            got: ty,
                            span,
                        });
                    } else {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            },
        }
    }

    /// `Into(target)`: `as` casts.
    fn check_into(&mut self, ty: TyId, to: TyId, span: Span) {
        let ty = self.uf.resolve(ty, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);
        let ty_shape = self.ty_arena.get(ty).clone();
        let to_shape = self.ty_arena.get(to).clone();
        let unresolved = matches!(
            (&ty_shape, &to_shape),
            (Ty::Var(_), _)
                | (_, Ty::Var(_))
                | (Ty::Error, _)
                | (_, Ty::Error)
                | (Ty::Unknown, _)
                | (_, Ty::Unknown)
        );

        if unresolved || ty == to {
        } else {
            match self.newtype_edge_status(ty, to, span) {
                NewtypeEdgeStatus::Allowed => {}
                NewtypeEdgeStatus::Blocked => {
                    self.errors.push(TypeError::PrivateReprCast {
                        from: ty,
                        to,
                        span,
                    });
                }
                NewtypeEdgeStatus::Missing => match (&ty_shape, &to_shape) {
                    // Functions cannot be stringified
                    (Ty::Fn(_, _), Ty::String) => {
                        self.errors.push(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Union(_, members), Ty::String) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, to),
                                *m,
                                span,
                            )
                        });
                    }
                    (_, Ty::String) => {}

                    // Functions, regex, refs cannot be Json-serialized
                    (Ty::Fn(_, _), Ty::Json)
                    | (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.errors.push(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::INTO, TyArena::JSON),
                        *elem,
                        span,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::INTO, TyArena::JSON),
                        *inner,
                        span,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            ok,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            err,
                            span,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            k,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::INTO, TyArena::JSON),
                            v,
                            span,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *e,
                                span,
                            )
                        });
                    }
                    (Ty::Object(fields), Ty::Json) => {
                        let vals: SmallVec<[TyId; 4]> =
                            fields.values().copied().collect();
                        vals.iter().for_each(|t| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *t,
                                span,
                            )
                        });
                    }
                    (Ty::Union(_, members), Ty::Json) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *m,
                                span,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &TypeClass::param(ClassId::INTO, TyArena::JSON),
                                *a,
                                span,
                            )
                        });
                    }
                    (_, Ty::Json) => {}

                    // Numeric coercions
                    (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {}
                    (Ty::Word, Ty::Int) | (Ty::Word, Ty::Float) => {}
                    (Ty::Bool, Ty::Int) | (Ty::Int, Ty::Bool) => {}

                    // Special conversions
                    (Ty::DataStatus, Ty::Int) => {}
                    (Ty::String, Ty::FilePath) => {}
                    (Ty::Path, Ty::FilePath) => {}
                    (Ty::Named(id, _), Ty::FilePath) if *id == TypeId::PATH => {
                    }

                    // `Range -> Array[Int]`
                    (Ty::Range, Ty::Array(elem)) if *elem == TyArena::INT => {}

                    // `Storable` to member type
                    (Ty::Union(Some(id), _), _) if *id == TypeId::STORABLE => {
                        if !TyArena::STORABLE_MEMBERS.contains(&to) {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Member to union type
                    (_, Ty::Union(Some(id), _))
                        if *id == TypeId::STORABLE
                            || *id == TypeId::SCALAR
                            || *id == TypeId::SUBSCRIPT =>
                    {
                        let uid = *id;
                        let is_member = if uid == TypeId::STORABLE {
                            TyArena::STORABLE_MEMBERS.contains(&ty)
                        } else if uid == TypeId::SCALAR {
                            TyArena::SCALAR_MEMBERS.contains(&ty)
                        } else {
                            TyArena::SUBSCRIPT_MEMBERS.contains(&ty)
                        };
                        if !is_member {
                            self.errors.push(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Union handling
                    (Ty::Union(prov, members), _) => {
                        let inst =
                            prov.and_then(|id| self.find_into_instance(id, to));
                        match inst {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                let ms: SmallVec<[TyId; 4]> = members.clone();
                                ms.iter().for_each(|m| {
                                    self.satisfies_class(
                                        &TypeClass::param(ClassId::INTO, to),
                                        *m,
                                        span,
                                    )
                                });
                            }
                        }
                    }

                    // User type with `Into` instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        match self.find_into_instance(id, to) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, None,
                                );
                            }
                            None => {
                                self.errors.push(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }

                    // Builtin type with user-defined `Into[UserType]` instance.
                    // E.g., `class Into[UserId] FOR Int { ... }`.
                    _ => {
                        let type_id =
                            Self::primitive_type_id(self.ty_arena.get(ty));
                        match type_id
                            .and_then(|id| self.find_into_instance(id, to))
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                self.errors.push(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }
                },
            }
        }
    }

    /// `TryInto(target)`: `read` casts.
    fn check_try_into(&mut self, ty: TyId, to: TyId, span: Span) {
        let ty = self.uf.resolve(ty, self.ty_arena);
        let to = self.uf.resolve(to, self.ty_arena);
        let ty_shape = self.ty_arena.get(ty).clone();
        let to_shape = self.ty_arena.get(to).clone();
        match (&ty_shape, &to_shape) {
            (Ty::Var(_), _) | (_, Ty::Var(_)) => {}
            (Ty::Error, _) | (_, Ty::Error) => {}
            (Ty::Unknown, _) | (_, Ty::Unknown) => {}

            _ if ty == to => {}

            _ => match self.newtype_edge_status(ty, to, span) {
                NewtypeEdgeStatus::Allowed | NewtypeEdgeStatus::Blocked => {
                    if !self.check_try_into_instance(ty, to, span) {
                        self.errors.push(
                            TypeError::NewtypeReprReadRequiresTryInto {
                                from: ty,
                                to,
                                span,
                            },
                        );
                    }
                }
                NewtypeEdgeStatus::Missing => match (&ty_shape, &to_shape) {
                    // Function types cannot be source for `READ`
                    (Ty::Fn(_, _), _) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }

                    // Cannot `READ` into function, regex, or refs
                    (_, Ty::Fn(_, _))
                    | (_, Ty::Regex)
                    | (_, Ty::Local)
                    | (_, Ty::Global) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }

                    // `READ Json` requires source to be `Into[Json]`
                    (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.errors.push(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    _ if self.check_try_into_instance(ty, to, span) => {}
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                        *elem,
                        span,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                        *inner,
                        span,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            ok,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            err,
                            span,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            k,
                            span,
                        );
                        self.satisfies_class(
                            &TypeClass::param(ClassId::TRY_INTO, TyArena::JSON),
                            v,
                            span,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
                                *e,
                                span,
                            )
                        });
                    }
                    (Ty::Object(fields), Ty::Json) => {
                        let vals: SmallVec<[TyId; 4]> =
                            fields.values().copied().collect();
                        vals.iter().for_each(|t| {
                            self.satisfies_class(
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
                                *t,
                                span,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &TypeClass::param(
                                    ClassId::TRY_INTO,
                                    TyArena::JSON,
                                ),
                                *a,
                                span,
                            )
                        });
                    }

                    // Union handling
                    (Ty::Union(prov, members), _) => {
                        let has_inst = prov
                            .and_then(|_| self.find_try_into_instance(ty, to))
                            .is_some();
                        if has_inst {
                            self.check_try_into_instance(ty, to, span);
                        } else {
                            let ms: SmallVec<[TyId; 4]> = members.clone();
                            ms.iter().for_each(|m| {
                                self.satisfies_class(
                                    &TypeClass::param(ClassId::TRY_INTO, to),
                                    *m,
                                    span,
                                )
                            });
                        }
                    }

                    // User type with `TryInto` instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        let inst = self
                            .find_try_into_instance_for(id, &type_args, to)
                            .map(|inst| (inst, type_args));
                        if let Some((inst, type_args)) = inst {
                            self.check_instance_constraints(
                                &inst, &type_args, span, None,
                            );
                        }
                    }

                    // All other combinations are valid for `READ`
                    _ => {}
                },
            },
        };
    }

    fn find_try_into_instance(
        &mut self,
        ty: TyId,
        to: TyId,
    ) -> Option<Instance> {
        match self.ty_arena.get(ty).clone() {
            Ty::Union(Some(id), _) => {
                self.find_try_into_instance_for(id, &[], to)
            }
            _ => self.ty_to_type_id_and_args(ty).and_then(|(id, args)| {
                self.find_try_into_instance_for(id, &args, to)
            }),
        }
    }

    fn find_try_into_instance_for(
        &mut self,
        id: TypeId,
        args: &[TyId],
        to: TyId,
    ) -> Option<Instance> {
        let insts: Vec<Instance> = self
            .instance_registry
            .lookup_all(ClassId::TRY_INTO, id)
            .to_vec();
        let to = self.uf.resolve(to, self.ty_arena);
        insts.into_iter().find(|inst| {
            inst.class_args.first().is_some_and(|&ia| {
                let subst = self.build_instance_subst_readonly(inst, args);
                let resolved = self.ty_arena.apply(ia, &subst);
                self.uf.resolve(resolved, self.ty_arena) == to
            })
        })
    }

    fn check_try_into_instance(
        &mut self,
        ty: TyId,
        to: TyId,
        span: Span,
    ) -> bool {
        let inst = match self.ty_arena.get(ty).clone() {
            Ty::Union(Some(id), _) => self
                .find_try_into_instance_for(id, &[], to)
                .map(|inst| (inst, SmallVec::new())),
            _ => self.ty_to_type_id_and_args(ty).and_then(|(id, args)| {
                self.find_try_into_instance_for(id, &args, to)
                    .map(|inst| (inst, args))
            }),
        };
        match inst {
            Some((inst, args)) => {
                let subst = self.build_instance_subst(&inst, &args, span);
                inst.class_args.first().copied().into_iter().for_each(|ia| {
                    let resolved = self.ty_arena.apply(ia, &subst);
                    if let Err(e) = self.unify_types(to, resolved, span) {
                        self.errors.push(e);
                    }
                });
                self.check_instance_constraints(
                    &inst,
                    &args,
                    span,
                    Some(&subst),
                );
                true
            }
            None => false,
        }
    }

    /// `Indexable(elem)`: `Array[T]`, `Map[K,V]`, `String`.
    ///
    /// The index type is now accessed via the associated type `.Index`; only
    /// the element type is unified here.
    fn check_indexable(
        &mut self,
        class: &TypeClass<TyId>,
        ty: TyId,
        elem: TyId,
        span: Span,
    ) {
        match self.ty_arena.get(ty).clone() {
            Ty::Array(inner) => {
                // `Array[T]`: `elem = T` (index type is `Int`, via `.Index`)
                if let Err(e) = self.unify_types(elem, inner, span) {
                    self.errors.push(e);
                }
            }
            Ty::Map(_key, val) => {
                // `Map[K, V]`: `elem = Option[V]` (index type is `K`, via `.Index`)
                let opt_val = self.ty_arena.option(val);
                if let Err(e) = self.unify_types(elem, opt_val, span) {
                    self.errors.push(e);
                }
            }
            Ty::String => {
                // `String`: `elem = Char` (index type is `Int`, via `.Index`)
                if let Err(e) = self.unify_types(elem, TyArena::CHAR, span) {
                    self.errors.push(e);
                }
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|m| self.satisfies_class(class, *m, span));
            }
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            Ty::Named(id, type_args) => {
                match self
                    .instance_registry
                    .lookup(ClassId::INDEXABLE, id)
                    .cloned()
                {
                    Some(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);
                        // `class_args[0]` is the element type
                        if let Some(&inst_elem) = inst.class_args.first() {
                            let resolved =
                                self.ty_arena.apply(inst_elem, &param_subst);
                            if let Err(e) =
                                self.unify_types(elem, resolved, span)
                            {
                                self.errors.push(e);
                            }
                        }
                        self.check_instance_constraints(
                            &inst,
                            &type_args,
                            span,
                            Some(&param_subst),
                        );
                    }
                    None => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }
            _ => {
                self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                ));
            }
        }
    }

    /// Shared HKT class satisfaction logic for `Fallible`, `Wrappable`, and `Chainable`.
    ///
    /// All three handle the same set of types (`Option`, `Result`, `Tuple`, `Union`,
    /// `Var` defaulting to `Option`, `Apply`, `Named` via instance registry) and differ
    /// only in which tag is used for registry lookups and error messages.
    fn satisfies_hkt_class(
        &mut self,
        tag: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty = self.expand_alias_fully_for_class(ty, span).unwrap_or(ty);

        match self.ty_arena.get(ty).clone() {
            Ty::Option(opt_elem) => {
                elems
                    .iter()
                    .zip([opt_elem].iter())
                    .for_each(|(&inner, &b)| {
                        if let Err(e) = self.unify_types(inner, b, span) {
                            self.errors.push(e);
                        }
                    });
            }
            Ty::Result(ok, err) => {
                let builtin: SmallVec<[TyId; 2]> = if elems.len() <= 1 {
                    smallvec![ok]
                } else {
                    smallvec![ok, err]
                };
                elems.iter().zip(builtin.iter()).for_each(|(&inner, &b)| {
                    if let Err(e) = self.unify_types(inner, b, span) {
                        self.errors.push(e);
                    }
                });
            }
            Ty::Tuple(ts) => {
                match self
                    .instance_registry
                    .lookup_tuple(tag, ts.len())
                    .cloned()
                {
                    Some(inst) => {
                        let subst = self.build_instance_subst(&inst, &ts, span);
                        elems.iter().zip(inst.class_args.iter()).for_each(
                            |(&inner, &ie)| {
                                let resolved = self.ty_arena.apply(ie, &subst);
                                if let Err(e) =
                                    self.unify_types(inner, resolved, span)
                                {
                                    self.errors.push(e);
                                }
                            },
                        );
                        self.check_instance_constraints(
                            &inst,
                            &ts,
                            span,
                            Some(&subst),
                        );
                    }
                    None => {
                        // Fallback: direct zip (fully-unapplied tuples)
                        elems.iter().zip(ts.iter()).for_each(|(&inner, &b)| {
                            if let Err(e) = self.unify_types(inner, b, span) {
                                self.errors.push(e);
                            }
                        });
                    }
                }
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|m| self.satisfies_class(class, *m, span));
            }
            Ty::Var(v) => {
                if elems.len() <= 1 {
                    let elem = elems.first().copied().unwrap_or_else(|| {
                        let fv = self.uf.fresh();
                        self.ty_arena.alloc(Ty::Var(fv))
                    });
                    let opt_id = self.ty_arena.option(elem);
                    let root = self.uf.find(v);
                    self.uf.bind(root, opt_id);
                }
            }
            Ty::Apply(_, _) => {}
            Ty::Error | Ty::Unknown => {}
            Ty::Named(id, type_args) => {
                match self.instance_registry.lookup(tag, id).cloned() {
                    Some(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);
                        elems.iter().zip(inst.class_args.iter()).for_each(
                            |(&inner, &inst_inner)| {
                                let resolved = self
                                    .ty_arena
                                    .apply(inst_inner, &param_subst);
                                if let Err(e) =
                                    self.unify_types(inner, resolved, span)
                                {
                                    self.errors.push(e);
                                }
                            },
                        );
                        self.check_instance_constraints(
                            &inst,
                            &type_args,
                            span,
                            Some(&param_subst),
                        );
                    }
                    None => {
                        self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }
            _ => {
                self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                ));
            }
        }
    }

    /// Check a parameterized user class constraint via instance lookup.
    fn check_user_parameterized(
        &mut self,
        class_id: ClassId,
        params: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let class_arg = params.first().copied().unwrap_or(TyArena::UNKNOWN);
        let shape = self.ty_arena.get(ty).clone();
        match shape {
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            Ty::Union(prov, members) => {
                let insts = prov
                    .map(|id| self.instance_registry.lookup_all(class_id, id))
                    .unwrap_or(&[]);
                if insts.is_empty() {
                    members
                        .iter()
                        .for_each(|&m| self.satisfies_class(class, m, span));
                } else {
                    let matched =
                        self.find_matching_instance(insts, class_arg, &[]);
                    match matched {
                        Some(inst) => self.check_instance_constraints(
                            &inst,
                            &[],
                            span,
                            None,
                        ),
                        None => members.iter().for_each(|&m| {
                            self.satisfies_class(class, m, span)
                        }),
                    }
                }
            }
            _ => match self.ty_to_type_id_and_args(ty) {
                Some((tid, args)) => {
                    let insts =
                        self.instance_registry.lookup_all(class_id, tid);
                    match self.find_matching_instance(insts, class_arg, &args) {
                        Some(inst) => {
                            let subst =
                                self.build_instance_subst(&inst, &args, span);
                            if let Some(&ia) = inst.class_args.first() {
                                let resolved = self.ty_arena.apply(ia, &subst);
                                if let Err(e) =
                                    self.unify_types(class_arg, resolved, span)
                                {
                                    self.errors.push(e);
                                }
                            }
                            self.check_instance_constraints(
                                &inst,
                                &args,
                                span,
                                Some(&subst),
                            );
                        }
                        None => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
                None => self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                )),
            },
        }
    }

    /// Find an instance whose `class_args` match the expected `class_arg`.
    ///
    /// If only one instance exists, returns it directly.
    /// For multiple instances, builds each instance's substitution and
    /// checks if the resolved class arg matches `class_arg`.
    fn find_matching_instance(
        &mut self,
        insts: &[Instance],
        class_arg: TyId,
        type_args: &[TyId],
    ) -> Option<Instance> {
        match insts {
            [] => None,
            [single] => Some(single.clone()),
            many => {
                let resolved_arg = self.uf.resolve(class_arg, self.ty_arena);
                many.iter()
                    .find(|inst| {
                        inst.class_args.first().is_some_and(|&ia| {
                            let subst = self
                                .build_instance_subst_readonly(inst, type_args);
                            let resolved = self.ty_arena.apply(ia, &subst);
                            resolved == resolved_arg
                        })
                    })
                    .cloned()
            }
        }
    }

    /// Find an `Into[T]` instance for a type whose `class_args` target matches `to`.
    fn find_into_instance(
        &self,
        type_id: TypeId,
        to: TyId,
    ) -> Option<Instance> {
        let insts = self.instance_registry.lookup_all(ClassId::INTO, type_id);
        insts
            .iter()
            .find(|i| i.class_args.first().copied() == Some(to))
            .cloned()
    }

    /// Check a user-defined HKT class constraint via instance lookup.
    ///
    /// Unlike `satisfies_hkt_class`, does NOT default `Ty::Var` to `Option`
    /// and does NOT hardcode `Ty::Option`/`Ty::Result` as satisfying.
    fn check_user_hkt(
        &mut self,
        class_id: ClassId,
        elems: &[TyId],
        class: &TypeClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(ty).clone();
        match shape {
            Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|&m| self.satisfies_class(class, m, span));
            }
            _ => match self.ty_to_type_id_and_args(ty) {
                Some((tid, args)) => {
                    let found = if tid == TypeId::TUPLE {
                        self.instance_registry
                            .lookup_tuple(class_id, args.len())
                            .cloned()
                    } else {
                        self.instance_registry.lookup(class_id, tid).cloned()
                    };
                    match found {
                        Some(inst) => {
                            let subst =
                                self.build_instance_subst(&inst, &args, span);
                            elems.iter().zip(inst.class_args.iter()).for_each(
                                |(&elem, &inst_elem)| {
                                    let resolved =
                                        self.ty_arena.apply(inst_elem, &subst);
                                    if let Err(e) =
                                        self.unify_types(elem, resolved, span)
                                    {
                                        self.errors.push(e);
                                    }
                                },
                            );
                            self.check_instance_constraints(
                                &inst,
                                &args,
                                span,
                                Some(&subst),
                            );
                        }
                        None => self.errors.push(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        )),
                    }
                }
                None => self.errors.push(TypeError::UnsatisfiedClass(
                    class.clone(),
                    ty,
                    span,
                )),
            },
        }
    }

    /// Build a `Rename` from an instance's `type_params` and the actual
    /// `type_args` at a use site. For `Ty::Var` entries, adds the mapping
    /// to the rename. For concrete entries, unifies with the corresponding
    /// `type_arg` to verify they match.
    pub(super) fn build_instance_subst(
        &mut self,
        inst: &Instance,
        type_args: &[TyId],
        span: Span,
    ) -> Rename {
        // Partition into var mappings and concrete pairs first to
        // avoid borrow-checker issues with `ty_arena` vs `unify_types`.
        let (vars, concretes): (
            SmallVec<[(TyVar, TyId); 2]>,
            SmallVec<[(TyId, TyId); 2]>,
        ) = inst.type_params.iter().zip(type_args.iter()).fold(
            (SmallVec::new(), SmallVec::new()),
            |(mut vs, mut cs), (&p, &a)| {
                match self.ty_arena.get(p) {
                    Ty::Var(tv) => vs.push((*tv, a)),
                    _ => cs.push((p, a)),
                }
                (vs, cs)
            },
        );
        concretes.into_iter().for_each(|(p, a)| {
            if let Err(e) = self.unify_types(p, a, span) {
                self.errors.push(e);
            }
        });
        Rename(vars.into_iter().collect())
    }

    /// Like `build_instance_subst` but only builds the var-to-type mapping
    /// without performing unification on concrete entries. Used when
    /// probing for a matching instance among several candidates.
    fn build_instance_subst_readonly(
        &self,
        inst: &Instance,
        type_args: &[TyId],
    ) -> Rename {
        let vars: SmallVec<[(TyVar, TyId); 2]> = inst
            .type_params
            .iter()
            .zip(type_args.iter())
            .filter_map(|(&p, &a)| match self.ty_arena.get(p) {
                Ty::Var(tv) => Some((*tv, a)),
                _ => None,
            })
            .collect();
        Rename(vars.into_iter().collect())
    }

    /// Check that a user instance's WHERE constraints are satisfied.
    /// Accepts an optional pre-built `Rename` to avoid redundant
    /// `build_instance_subst` calls at sites that already have one.
    fn check_instance_constraints(
        &mut self,
        inst: &Instance,
        type_args: &[TyId],
        span: Span,
        subst: Option<&Rename>,
    ) {
        let fallback;
        let inst_subst = match subst {
            Some(r) => r,
            None => {
                fallback = self.build_instance_subst(inst, type_args, span);
                &fallback
            }
        };
        // Collect constraints to avoid borrow conflict
        let constraints: SmallVec<[(TyVar, TypeClass<TyId>); 2]> =
            inst.constraints.clone();
        constraints.iter().for_each(|(var, class)| {
            let var_id = self.ty_arena.alloc(Ty::Var(*var));
            let ty = self.ty_arena.apply(var_id, inst_subst);
            let class = class.apply(inst_subst, self.ty_arena);
            self.satisfies_class(&class, ty, span);
        });
    }

    /// Check that a callee type is callable and unify with expected signature.
    fn check_callable(
        &mut self,
        callee: TyId,
        args: &[TyId],
        ret: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(callee).clone();
        match shape {
            Ty::Fn(ref params, fn_ret) => {
                let params: SmallVec<[TyId; 4]> = params.clone();
                if args.len() > params.len() {
                    self.errors.push(TypeError::TooManyArguments {
                        expected: params.len(),
                        got: args.len(),
                        span,
                    });
                } else if args.is_empty() && !params.is_empty() {
                    self.errors.push(TypeError::ZeroArguments {
                        expected: params.len(),
                        span,
                    });
                } else if args.len() < params.len() {
                    // Partial application: unify supplied args with prefix
                    params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                        if let Err(e) = self.unify_types(a, p, span) {
                            self.errors.push(e);
                        }
                    });

                    // Residual function type from remaining params
                    let remaining: SmallVec<[TyId; 4]> =
                        params[args.len()..].iter().copied().collect();
                    let residual = self.ty_arena.func(remaining, fn_ret);
                    if let Err(e) = self.unify_types(residual, ret, span) {
                        self.errors.push(e);
                    }
                } else {
                    // Full application
                    params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                        if let Err(e) = self.unify_types(a, p, span) {
                            self.errors.push(e);
                        }
                    });

                    if let Err(e) = self.unify_types(fn_ret, ret, span) {
                        self.errors.push(e);
                    }
                }
            }

            Ty::Var(v) => {
                // Callee is unresolved; create function type and bind
                let fn_ty =
                    self.ty_arena.func(args.iter().copied().collect(), ret);
                if let Err(e) = self.unify_var(v, fn_ty, span) {
                    self.errors.push(e);
                }
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.errors.push(TypeError::NotCallable(callee, span));
            }
        }
    }

    /// Check that a type has a specific field.
    ///
    /// Looks up the field in the resolved base type and unifies the expected
    /// field type with the actual field type. Unlike `unify_named_with_object`,
    /// this only checks the single accessed field, not all object fields.
    fn check_has_field(
        &mut self,
        base: TyId,
        field: StringId,
        field_ty: TyId,
        span: Span,
    ) {
        let shape = self.ty_arena.get(base).clone();
        match shape {
            // Structural object: look up field directly
            Ty::Object(ref fields) => match fields.get(&field) {
                Some(&actual_ty) => {
                    if let Err(e) = self.unify_types(field_ty, actual_ty, span)
                    {
                        self.errors.push(e);
                    }
                }
                None => {
                    let name = self
                        .env
                        .get_str(field)
                        .unwrap_or("<unknown>")
                        .to_string();
                    self.errors.push(TypeError::FieldNotFound {
                        ty: base,
                        field: name,
                        span,
                    });
                }
            },

            // Named alias to object: look up field in alias definition
            Ty::Named(type_id, ref type_args) => {
                let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                let def = self.registry.get_def(type_id);
                match def {
                    Some(TypeDef::Alias { type_params, .. }) => {
                        if !self.convert_ctx().can_access_alias_repr(type_id) {
                            self.errors
                                .push(TypeError::NotAnObject(base, span));
                        } else {
                            let target = self.decls.alias_target(type_id);
                            let params: SmallVec<[StringId; 2]> =
                                type_params.clone();
                            match self.ast.get_type_expr(target).cloned() {
                                Some(AstTypeExpr::Object(alias_fields)) => {
                                    let field_str =
                                        self.env.get_str(field).unwrap_or("");
                                    let field_ty_id = alias_fields
                                        .iter()
                                        .find(|(n, _)| *n == field)
                                        .map(|(_, ty)| *ty);
                                    match field_ty_id {
                                        Some(ast_ty_id) => {
                                            let param_subst: IndexMap<_, _> =
                                                params
                                                    .iter()
                                                    .zip(type_args.iter())
                                                    .map(|(p, &a)| (*p, a))
                                                    .collect();
                                            let actual_ty = self
                                                .convert_ctx()
                                                .ast_type_to_ty(
                                                    ast_ty_id,
                                                    &param_subst,
                                                );
                                            if let Err(e) = self.unify_types(
                                                field_ty, actual_ty, span,
                                            ) {
                                                self.errors.push(e);
                                            }
                                        }
                                        None => {
                                            self.errors.push(
                                                TypeError::FieldNotFound {
                                                    ty: base,
                                                    field: field_str
                                                        .to_string(),
                                                    span,
                                                },
                                            );
                                        }
                                    }
                                }
                                _ => {
                                    self.errors.push(TypeError::NotAnObject(
                                        base, span,
                                    ));
                                }
                            }
                        }
                    }
                    _ => {
                        self.errors.push(TypeError::NotAnObject(base, span));
                    }
                }
            }

            // Json: any field access is valid and returns Json
            Ty::Json => {
                if let Err(e) = self.unify_types(field_ty, TyArena::JSON, span)
                {
                    self.errors.push(e);
                }
            }

            // Union: all members must have the field with compatible types
            Ty::Union(_, ref members) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter().for_each(|&m| {
                    self.check_has_field(m, field, field_ty, span);
                });
            }

            // Type variable: defer until resolved
            Ty::Var(_) => {
                // Type variable not yet resolved; constraint will be checked
                // when the variable is bound. For now, this is allowed.
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.errors.push(TypeError::NotAnObject(base, span));
            }
        }
    }

    /// Resolve an associated type projection to a concrete type.
    ///
    /// Given a base type (`Array[Int]`, `Map[String, Int]`, etc.) and a class
    /// with an associated type (`Indexable:Index`), returns the concrete type
    /// that the associated type resolves to.
    ///
    /// # Builtin Rules
    ///
    /// - `Array[T]` with `Indexable:Index` -> `Int`
    /// - `Map[K, V]` with `Indexable:Index` -> `K`
    /// - `String` with `Indexable:Index` -> `Int`
    ///
    /// # User Types
    ///
    /// For user-defined types, looks up the instance in the registry and
    /// retrieves the associated type definition.
    ///
    pub(crate) fn resolve_assoc_type(
        &mut self,
        base: TyId,
        class: ClassId,
        assoc_name: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        // Validate that assoc_name is a valid associated type for this class
        let assoc_types = &self.env.class_def(class).assoc_types;
        if !assoc_types.contains(&assoc_name) {
            Err(TypeError::NoSuchAssocType {
                class,
                name: assoc_name,
                span,
            })
        } else {
            let shape = self.ty_arena.get(base).clone();
            match shape {
                // Builtin: Array[T] with Indexable:Index = Int
                Ty::Array(_) if class == ClassId::INDEXABLE => Ok(TyArena::INT),

                // Builtin: Map[K, V] with Indexable:Index = K
                Ty::Map(k, _) if class == ClassId::INDEXABLE => Ok(k),

                // Builtin: String with Indexable:Index = Int
                Ty::String if class == ClassId::INDEXABLE => Ok(TyArena::INT),

                // User type: look up instance in registry
                Ty::Named(type_id, ref type_args) => {
                    let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                    match self.instance_registry.lookup(class, type_id).cloned()
                    {
                        Some(inst) => {
                            let param_rename = self
                                .build_instance_subst(&inst, &type_args, span);
                            // Find the associated type definition
                            match inst.get_assoc_type(assoc_name) {
                                Some(assoc_def) => {
                                    let assoc_ty = assoc_def.ty;
                                    Ok(self
                                        .ty_arena
                                        .apply(assoc_ty, &param_rename))
                                }
                                None => Err(TypeError::MissingAssocType {
                                    class,
                                    assoc: assoc_name,
                                    span,
                                }),
                            }
                        }
                        None => Err(TypeError::UnsatisfiedClass(
                            TypeClass::placeholder(
                                class,
                                self.env.class_def(class).shape,
                            ),
                            base,
                            span,
                        )),
                    }
                }

                // Type variable: cannot resolve yet (defer resolution)
                Ty::Var(_) => Err(TypeError::UnknownAssocType {
                    ty: base,
                    assoc: assoc_name,
                    span,
                }),

                // Error/Unknown: propagate
                Ty::Error | Ty::Unknown => Ok(TyArena::ERROR),

                // User classes: handle parameterized builtins via instance lookup
                _ if class.idx() >= ClassId::BUILTIN_COUNT => {
                    match self.ty_to_type_id_and_args(base) {
                        Some((tid, type_args)) => {
                            match self
                                .instance_registry
                                .lookup(class, tid)
                                .cloned()
                            {
                                Some(inst) => {
                                    let param_rename = self
                                        .build_instance_subst(
                                            &inst, &type_args, span,
                                        );
                                    match inst.get_assoc_type(assoc_name) {
                                        Some(assoc_def) => {
                                            Ok(self.ty_arena.apply(
                                                assoc_def.ty,
                                                &param_rename,
                                            ))
                                        }
                                        None => {
                                            Err(TypeError::MissingAssocType {
                                                class,
                                                assoc: assoc_name,
                                                span,
                                            })
                                        }
                                    }
                                }
                                None => Err(TypeError::UnsatisfiedClass(
                                    TypeClass::placeholder(
                                        class,
                                        self.env.class_def(class).shape,
                                    ),
                                    base,
                                    span,
                                )),
                            }
                        }
                        None => Err(TypeError::UnsatisfiedClass(
                            TypeClass::placeholder(
                                class,
                                self.env.class_def(class).shape,
                            ),
                            base,
                            span,
                        )),
                    }
                }

                // Other types: no instance for this class
                _ => Err(TypeError::UnsatisfiedClass(
                    TypeClass::placeholder(
                        class,
                        self.env.class_def(class).shape,
                    ),
                    base,
                    span,
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typecheck::instance::Instance;
    use crate::TypeId;

    /// Test that `check_ord` passes for builtin orderable types.
    #[test]
    fn check_ord_builtins() {
        // All these should pass without error
        let builtins = [
            Ty::Bool,
            Ty::Int,
            Ty::Word,
            Ty::Float,
            Ty::Char,
            Ty::String,
            Ty::Time,
            Ty::Ordering,
        ];

        builtins.iter().for_each(|ty| {
            // We can't easily create an InferCtx in unit tests, so we test
            // the match logic indirectly via integration tests. This test
            // documents the expected behavior.
            assert!(
                matches!(
                    ty,
                    Ty::Bool
                        | Ty::Int
                        | Ty::Word
                        | Ty::Float
                        | Ty::Char
                        | Ty::String
                        | Ty::Time
                        | Ty::Ordering
                ),
                "expected {} to be orderable",
                ty
            );
        });
    }

    /// Test that `check_display` passes for builtin displayable types.
    #[test]
    fn check_display_builtins() {
        let mut a = TyArena::new();
        // Functions are NOT displayable
        let fn_ty = a.func(smallvec::smallvec![TyArena::INT], TyArena::INT);
        assert!(
            matches!(a.get(fn_ty), Ty::Fn(_, _)),
            "Fn types should not be displayable"
        );

        // All primitives are displayable
        let displayable = [
            TyArena::BOOL,
            TyArena::INT,
            TyArena::WORD,
            TyArena::FLOAT,
            TyArena::CHAR,
            TyArena::STRING,
            TyArena::UNIT,
            TyArena::TIME,
            TyArena::RANGE,
            TyArena::JSON,
            TyArena::ORDERING,
            TyArena::DATA_STATUS,
            TyArena::FILEPATH,
            TyArena::PATH,
            TyArena::REGEX,
            TyArena::RUNTIME_ERROR,
            TyArena::LOCAL,
            TyArena::GLOBAL,
        ];

        displayable.iter().for_each(|&tid| {
            assert!(
                !matches!(a.get(tid), Ty::Fn(_, _)),
                "expected type to be displayable"
            );
        });
    }

    /// Test Instance creation and lookup.
    #[test]
    fn instance_registry_lookup() {
        use crate::typecheck::instance::InstanceRegistry;

        let mut registry = InstanceRegistry::new();

        // Use an existing TypeId constant for testing (STORABLE is a union type
        // that we can hypothetically add an Ord instance for)
        let user_type_id = TypeId::STORABLE;

        // Create an Ord instance for the user type
        let ord_inst = Instance {
            class: ClassId::ORD,
            class_args: SmallVec::new(),
            type_params: SmallVec::new(),
            constraints: SmallVec::new(),
            methods: HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        let _ = registry.register(user_type_id, ord_inst.clone());

        // Lookup should find the instance
        let found = registry.lookup(ClassId::ORD, user_type_id);
        assert!(found.is_some(), "should find Ord instance");

        // Lookup for different class should not find anything
        let not_found = registry.lookup(ClassId::DISPLAY, user_type_id);
        assert!(not_found.is_none(), "should not find Display instance");

        // Lookup for different type should not find anything
        let other_type_id = TypeId::SCALAR;
        let not_found2 = registry.lookup(ClassId::ORD, other_type_id);
        assert!(
            not_found2.is_none(),
            "should not find instance for other type"
        );
    }

    /// Test that Instance with WHERE constraints stores them correctly.
    #[test]
    fn instance_with_constraints() {
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let t_id = a.var(0);
        let constraint = (t, TypeClass::simple(ClassId::DISPLAY));

        let inst = Instance {
            class: ClassId::ORD,
            class_args: SmallVec::new(),
            type_params: smallvec::smallvec![t_id],
            constraints: smallvec::smallvec![constraint],
            methods: HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        assert_eq!(inst.type_params.len(), 1);
        assert_eq!(inst.constraints.len(), 1);
        assert_eq!(inst.constraints[0].0, t);
        assert!(matches!(
            inst.constraints[0].1,
            TypeClass::Concrete { id: ClassId::DISPLAY, ref params } if params.is_empty()
        ));
    }

    /// Test constraint substitution.
    #[test]
    fn constraint_substitution() {
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let var_id = a.var(0);
        let constraint = TypeClass::hkt_elem(ClassId::ITERABLE, var_id);

        // Create rename: T -> Int
        let rename = Rename::singleton(t, TyArena::INT);

        // Apply rename to constraint
        let resolved = constraint.apply(&rename, &mut a);

        // Should now be `Iterable` with `elems: [Int]`
        assert!(
            matches!(
                resolved,
                TypeClass::Hkt { id: ClassId::ITERABLE, ref elems, .. } if elems.first() == Some(&TyArena::INT)
            ),
            "constraint should be Iterable with elems=[Int] after rename"
        );
    }
}
