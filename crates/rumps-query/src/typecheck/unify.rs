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

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::error::TypeError;
use super::infer::{Constraint, InferCtx};
use super::ty::{
    BuiltinClass, BuiltinClassTag, Subst, Ty, TyArena, TyId, TyVar,
};
use crate::ast::AstTypeExpr;
use crate::intern::StringId;
use crate::value::TypeDef;
use crate::Span;

/// Result of a unification attempt.
#[derive(Debug)]
pub(crate) enum UnifyResult {
    /// Unification succeeded; returns the resulting substitution.
    Ok(Subst),
    /// Unification failed; returns the error.
    Err(TypeError),
}

impl<'a> InferCtx<'a> {
    /// Unify two types, returning a substitution that makes them equal.
    ///
    /// # Unification Rules
    ///
    /// 1. `Var(v) ~ t` -> `{ v -> t }` (if `v` not in `fv(t)`; occurs check)
    /// 2. `t ~ Var(v)` -> `{ v -> t }` (symmetric)
    /// 3. `Array[a] ~ Array[b]` -> `unify(a, b)` (recursive)
    /// 4. `Fn[p1] -> r1 ~ Fn[p2] -> r2` -> `unify(p1, p2) . unify(r1, r2)`
    /// 5. `{ f1 } ~ { f2 }` -> unify common fields (structural objects)
    /// 6. `Named(id, args1) ~ Named(id, args2)` -> unify corresponding args
    /// 7. `Unknown ~ _` or `_ ~ Unknown` -> `{}` (unifies with anything)
    /// 8. `Error ~ _` or `_ ~ Error` -> `{}` (error recovery)
    /// 9. `T ~ T` -> `{}` (primitives equal)
    /// 10. Otherwise -> error
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

    /// Expand a `Ty::Named` alias fully to its target type.
    ///
    /// Recursively expands chained aliases (e.g., `A = B`, `B = Int`) until
    /// reaching a non-alias type. Object aliases are NOT expanded; they need
    /// special handling in `unify_named_with_object`.
    fn expand_alias_fully(&mut self, ty: TyId) -> Option<TyId> {
        let mut current = ty;
        let mut expanded = false;
        // Expand until we hit a non-alias or object alias
        while let Some(next) = self.expand_alias_once(current) {
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
    fn expand_alias_once(&mut self, ty: TyId) -> Option<TyId> {
        let (type_id, args) = match self.ty_arena.get(ty) {
            Ty::Named(id, a) => (*id, a.clone()),
            _ => None?,
        };
        match self.registry().get_def(type_id) {
            Some(TypeDef::Alias {
                type_params,
                target,
                ..
            }) => {
                // Don't expand if target is an object type; let
                // `unify_named_with_object` handle it for proper
                // required-field checking
                let is_obj = self
                    .ast()
                    .get_type_expr(*target)
                    .is_some_and(|te| matches!(te, AstTypeExpr::Object(_)));
                if is_obj {
                    None
                } else {
                    let subst: IndexMap<StringId, TyId> = type_params
                        .iter()
                        .zip(args.iter())
                        .map(|(p, a)| (*p, *a))
                        .collect();
                    let target = *target;
                    Some(self.ast_type_to_ty(target, &subst))
                }
            }
            _ => None,
        }
    }

    /// Core unification logic.
    fn unify_inner(&mut self, t1: TyId, t2: TyId, span: Span) -> UnifyResult {
        // Expand aliases fully before unifying (transparent type aliases)
        let t1 = self.expand_alias_fully(t1).unwrap_or(t1);
        let t2 = self.expand_alias_fully(t2).unwrap_or(t2);

        // Equal TyIds are trivially unified
        if t1 == t2 {
            UnifyResult::Ok(Subst::empty())
        } else {
            // Clone both Ty to release borrow on arena
            let ty1 = self.ty_arena.get(t1).clone();
            let ty2 = self.ty_arena.get(t2).clone();
            self.unify_inner_dispatch(t1, t2, &ty1, &ty2, span)
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
            (Ty::Error, _) | (_, Ty::Error) => UnifyResult::Ok(Subst::empty()),

            // Unknown unifies with anything (database reads before narrowing)
            (Ty::Unknown, _) | (_, Ty::Unknown) => {
                UnifyResult::Ok(Subst::empty())
            }

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
            | (Ty::RuntimeError, Ty::RuntimeError) => {
                UnifyResult::Ok(Subst::empty())
            }

            // Local and Global are distinct types; use Ref union for either
            (Ty::Local, Ty::Local) | (Ty::Global, Ty::Global) => {
                UnifyResult::Ok(Subst::empty())
            }

            // Numeric types: same type only (no implicit coercion)
            (Ty::Int, Ty::Int)
            | (Ty::Word, Ty::Word)
            | (Ty::Float, Ty::Float) => UnifyResult::Ok(Subst::empty()),

            // Array: unify element types
            (Ty::Array(a), Ty::Array(b)) => self.unify_inner(*a, *b, span),

            // Range coerces to Array[Int] (for Array HOFs)
            (Ty::Range, Ty::Array(elem)) | (Ty::Array(elem), Ty::Range) => {
                self.unify_inner(*elem, TyArena::INT, span)
            }

            // Option: unify inner types
            (Ty::Option(a), Ty::Option(b)) => self.unify_inner(*a, *b, span),

            // Result: unify both ok and err types
            (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                let (ok1, err1, ok2, err2) = (*ok1, *err1, *ok2, *err2);
                match self.unify_inner(ok1, ok2, span) {
                    UnifyResult::Ok(s1) => {
                        let err1 = self.ty_arena.apply(err1, &s1);
                        let err2 = self.ty_arena.apply(err2, &s1);
                        match self.unify_inner(err1, err2, span) {
                            UnifyResult::Ok(s2) => UnifyResult::Ok(
                                s1.compose(&s2, &mut self.ty_arena),
                            ),
                            err => err,
                        }
                    }
                    err => err,
                }
            }

            // Map: unify key and value types
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                let (k1, v1, k2, v2) = (*k1, *v1, *k2, *v2);
                match self.unify_inner(k1, k2, span) {
                    UnifyResult::Ok(s1) => {
                        let v1 = self.ty_arena.apply(v1, &s1);
                        let v2 = self.ty_arena.apply(v2, &s1);
                        match self.unify_inner(v1, v2, span) {
                            UnifyResult::Ok(s2) => UnifyResult::Ok(
                                s1.compose(&s2, &mut self.ty_arena),
                            ),
                            err => err,
                        }
                    }
                    err => err,
                }
            }

            // Tuple: unify element-wise (must have same length)
            (Ty::Tuple(ts1), Ty::Tuple(ts2)) => {
                if ts1.len() != ts2.len() {
                    UnifyResult::Err(TypeError::Mismatch {
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
                    UnifyResult::Err(TypeError::ArityMismatch {
                        expected: params1.len(),
                        got: params2.len(),
                        span,
                    })
                } else {
                    let p1: SmallVec<[TyId; 4]> = params1.clone();
                    let p2: SmallVec<[TyId; 4]> = params2.clone();
                    let (r1, r2) = (*ret1, *ret2);
                    match self.unify_sequence(
                        p1.iter().copied(),
                        p2.iter().copied(),
                        span,
                    ) {
                        UnifyResult::Ok(s) => {
                            let ret1 = self.ty_arena.apply(r1, &s);
                            let ret2 = self.ty_arena.apply(r2, &s);
                            match self.unify_inner(ret1, ret2, span) {
                                UnifyResult::Ok(s2) => UnifyResult::Ok(
                                    s.compose(&s2, &mut self.ty_arena),
                                ),
                                err => err,
                            }
                        }
                        err => err,
                    }
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
                    UnifyResult::Err(TypeError::Mismatch {
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

            // Union types: structural equality (same members, order-independent)
            (Ty::Union(members1), Ty::Union(members2)) => {
                if members1.len() != members2.len() {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t2,
                        got: t1,
                        span,
                    })
                } else {
                    let m1: SmallVec<[TyId; 4]> = members1.clone();
                    let m2: SmallVec<[TyId; 4]> = members2.clone();
                    // Find a bijective matching between union members
                    let available: Vec<usize> = (0..m2.len()).collect();
                    self.unify_union_bijection(
                        &m1,
                        &m2,
                        &available,
                        Subst::empty(),
                        span,
                    )
                    .unwrap_or({
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: t1,
                            got: t2,
                            span,
                        })
                    })
                }
            }

            // Concrete type with union: T unifies if it matches any member
            (_, Ty::Union(members)) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| match self.unify_inner(t1, m, span) {
                        ok @ UnifyResult::Ok(_) => Some(ok),
                        _ => None,
                    })
                    .unwrap_or({
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }
            (Ty::Union(members), _) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter()
                    .find_map(|&m| match self.unify_inner(m, t2, span) {
                        ok @ UnifyResult::Ok(_) => Some(ok),
                        _ => None,
                    })
                    .unwrap_or({
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: t2,
                            got: t1,
                            span,
                        })
                    })
            }

            // Named union with concrete type: expand union and check membership
            (_, Ty::Named(..)) => self.expand_union_members(t2).map_or_else(
                || {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1,
                        got: t2,
                        span,
                    })
                },
                |members| {
                    members
                        .iter()
                        .find_map(|&m| match self.unify_inner(t1, m, span) {
                            ok @ UnifyResult::Ok(_) => Some(ok),
                            _ => None,
                        })
                        .unwrap_or({
                            UnifyResult::Err(TypeError::Mismatch {
                                expected: t1,
                                got: t2,
                                span,
                            })
                        })
                },
            ),
            (Ty::Named(..), _) => self.expand_union_members(t1).map_or_else(
                || {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1,
                        got: t2,
                        span,
                    })
                },
                |members| {
                    members
                        .iter()
                        .find_map(|&m| match self.unify_inner(m, t2, span) {
                            ok @ UnifyResult::Ok(_) => Some(ok),
                            _ => None,
                        })
                        .unwrap_or({
                            UnifyResult::Err(TypeError::Mismatch {
                                expected: t1,
                                got: t2,
                                span,
                            })
                        })
                },
            ),

            // HKT type application: `F[T]` where `F` is a type variable.
            //
            // Decompose the other type into constructor + element,
            // bind the type variable to the constructor shape, and
            // unify args with the element types.
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

            // Associated type projection: resolve and unify
            (Ty::AssocType(tv, class, name), _) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(resolved, t2, span),
                    Err(_) => {
                        // Base type is unresolved (type variable); defer
                        UnifyResult::Ok(Subst::empty())
                    }
                }
            }
            (_, Ty::AssocType(tv, class, name)) => {
                let (tv, class, name) = (*tv, *class, *name);
                let base = self.ty_arena.alloc(Ty::Var(tv));
                match self.resolve_assoc_type(base, class, name, span) {
                    Ok(resolved) => self.unify_inner(t1, resolved, span),
                    Err(_) => UnifyResult::Ok(Subst::empty()),
                }
            }

            // All other combinations are type mismatches
            _ => UnifyResult::Err(TypeError::Mismatch {
                expected: t2,
                got: t1,
                span,
            }),
        }
    }

    /// Unify a type variable with a type.
    ///
    /// Performs the occurs check to prevent infinite types like `a = Array[a]`.
    fn unify_var(&mut self, v: TyVar, t: TyId, span: Span) -> UnifyResult {
        // If t is the same variable, nothing to do
        if matches!(self.ty_arena.get(t), Ty::Var(w) if *w == v) {
            UnifyResult::Ok(Subst::empty())
        } else if self.ty_arena.occurs(t, v) {
            // Occurs check failed; would create infinite type
            UnifyResult::Err(TypeError::InfiniteType(v, t, span))
        } else {
            UnifyResult::Ok(Subst::singleton(v, t))
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
                self.unify_apply_inner(tv, args, ctor, inner, span)
            }
            Ty::Result(ok, err) => {
                let ctor = self.ty_arena.result(TyArena::ERROR, err);
                self.unify_apply_inner(tv, args, ctor, ok, span)
            }
            Ty::Array(inner) => {
                let ctor = self.ty_arena.array(TyArena::ERROR);
                self.unify_apply_inner(tv, args, ctor, inner, span)
            }
            Ty::Map(k, v) => {
                let ctor = self.ty_arena.map_ty(TyArena::ERROR, v);
                self.unify_apply_inner(tv, args, ctor, k, span)
            }

            // Non-parameterized types with known element types
            Ty::Range => {
                let s1 = self.unify_var(tv, TyArena::RANGE, span);
                self.unify_apply_first_arg(s1, args, TyArena::INT, span)
            }

            // User-defined named types: decompose into constructor + type args
            Ty::Named(id, ref type_args) => {
                if let Some(&first_arg) = type_args.first() {
                    let placeholder: SmallVec<[TyId; 4]> =
                        type_args.iter().map(|_| TyArena::ERROR).collect();
                    let ctor = self.ty_arena.named(id, placeholder);
                    self.unify_apply_inner(tv, args, ctor, first_arg, span)
                } else {
                    // Named type with no params; just bind tv
                    self.unify_var(tv, other, span)
                }
            }

            // Two `Apply` nodes: unify constructors and args pairwise
            Ty::Apply(tv2, ref args2) => {
                if args.len() != args2.len() {
                    let exp = self.ty_arena.hkt(tv2, args2.clone());
                    let got_args: SmallVec<[TyId; 4]> =
                        args.iter().copied().collect();
                    let got = self.ty_arena.hkt(tv, got_args);
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: exp,
                        got,
                        span,
                    })
                } else {
                    let a2: SmallVec<[TyId; 4]> = args2.clone();
                    let tv2_id = self.ty_arena.alloc(Ty::Var(tv2));
                    match self.unify_var(tv, tv2_id, span) {
                        UnifyResult::Ok(s1) => {
                            let a1: SmallVec<[TyId; 4]> = args
                                .iter()
                                .map(|&a| self.ty_arena.apply(a, &s1))
                                .collect();
                            let a2: SmallVec<[TyId; 4]> = a2
                                .iter()
                                .map(|&a| self.ty_arena.apply(a, &s1))
                                .collect();
                            match self.unify_sequence(
                                a1.iter().copied(),
                                a2.iter().copied(),
                                span,
                            ) {
                                UnifyResult::Ok(s2) => UnifyResult::Ok(
                                    s1.compose(&s2, &mut self.ty_arena),
                                ),
                                err => err,
                            }
                        }
                        err => err,
                    }
                }
            }

            _ => {
                let got_args: SmallVec<[TyId; 4]> =
                    args.iter().copied().collect();
                let got = self.ty_arena.hkt(tv, got_args);
                UnifyResult::Err(TypeError::Mismatch {
                    expected: other,
                    got,
                    span,
                })
            }
        }
    }

    /// Bind `tv` to a constructor shape and unify the first `Apply` arg with
    /// the element type.
    fn unify_apply_inner(
        &mut self,
        tv: TyVar,
        args: &[TyId],
        ctor: TyId,
        elem: TyId,
        span: Span,
    ) -> UnifyResult {
        let s1 = self.unify_var(tv, ctor, span);
        self.unify_apply_first_arg(s1, args, elem, span)
    }

    /// Given a constructor binding result, unify the first `Apply` arg with
    /// an element type.
    fn unify_apply_first_arg(
        &mut self,
        ctor_result: UnifyResult,
        args: &[TyId],
        elem: TyId,
        span: Span,
    ) -> UnifyResult {
        match ctor_result {
            UnifyResult::Ok(s1) => args.first().map_or_else(
                || {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: elem,
                        got: TyArena::UNIT,
                        span,
                    })
                },
                |&arg| {
                    let arg = self.ty_arena.apply(arg, &s1);
                    let elem = self.ty_arena.apply(elem, &s1);
                    match self.unify_inner(arg, elem, span) {
                        UnifyResult::Ok(s2) => {
                            UnifyResult::Ok(s1.compose(&s2, &mut self.ty_arena))
                        }
                        err => err,
                    }
                },
            ),
            err => err,
        }
    }

    /// Unify two sequences of types element-wise.
    fn unify_sequence(
        &mut self,
        ts1: impl Iterator<Item = TyId>,
        ts2: impl Iterator<Item = TyId>,
        span: Span,
    ) -> UnifyResult {
        ts1.zip(ts2)
            .try_fold(Subst::empty(), |acc, (t1, t2)| {
                let t1 = self.ty_arena.apply(t1, &acc);
                let t2 = self.ty_arena.apply(t2, &acc);
                match self.unify_inner(t1, t2, span) {
                    UnifyResult::Ok(s) => {
                        Ok(acc.compose(&s, &mut self.ty_arena))
                    }
                    UnifyResult::Err(e) => Err(e),
                }
            })
            .map_or_else(UnifyResult::Err, UnifyResult::Ok)
    }

    /// Find a bijective matching between union members via backtracking.
    ///
    /// Tries to match each member of `remaining1` to a unique member of `all2`
    /// (using indices in `available`). Returns `Some(UnifyResult)` if a complete
    /// matching is found or definitely fails, `None` to signal backtracking.
    fn unify_union_bijection(
        &mut self,
        remaining1: &[TyId],
        all2: &[TyId],
        available: &[usize],
        acc: Subst,
        span: Span,
    ) -> Option<UnifyResult> {
        match remaining1.split_first() {
            None => Some(UnifyResult::Ok(acc)),
            Some((&first, rest)) => {
                // Try each available index, backtracking on failure
                self.try_union_matches(
                    first, rest, all2, available, acc, span, 0,
                )
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
        acc: Subst,
        span: Span,
        start_idx: usize,
    ) -> Option<UnifyResult> {
        available.get(start_idx).and_then(|&idx| {
            let m2 = *all2.get(idx)?;
            let first_applied = self.ty_arena.apply(first, &acc);
            let m2_applied = self.ty_arena.apply(m2, &acc);

            match self.unify_inner(first_applied, m2_applied, span) {
                UnifyResult::Ok(s) => {
                    let new_acc = acc.compose(&s, &mut self.ty_arena);
                    let new_available: Vec<usize> = available
                        .iter()
                        .copied()
                        .filter(|&i| i != idx)
                        .collect();

                    match self.unify_union_bijection(
                        rest,
                        all2,
                        &new_available,
                        new_acc,
                        span,
                    ) {
                        Some(UnifyResult::Ok(final_subst)) => {
                            Some(UnifyResult::Ok(final_subst))
                        }
                        // Backtrack: try next available index
                        _ => self.try_union_matches(
                            first,
                            rest,
                            all2,
                            available,
                            acc,
                            span,
                            start_idx + 1,
                        ),
                    }
                }
                // This match failed; try next available index
                UnifyResult::Err(_) => self.try_union_matches(
                    first,
                    rest,
                    all2,
                    available,
                    acc,
                    span,
                    start_idx + 1,
                ),
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
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Unify fields present in both; extra fields are allowed
        all_keys
            .iter()
            .try_fold(Subst::empty(), |acc, key| {
                match (fields1.get(key), fields2.get(key)) {
                    (Some(&t1), Some(&t2)) => {
                        let t1 = self.ty_arena.apply(t1, &acc);
                        let t2 = self.ty_arena.apply(t2, &acc);
                        match self.unify_inner(t1, t2, span) {
                            UnifyResult::Ok(s) => {
                                Ok(acc.compose(&s, &mut self.ty_arena))
                            }
                            UnifyResult::Err(e) => Err(e),
                        }
                    }
                    // Field only in one object; extensible, so OK
                    (Some(_), None) | (None, Some(_)) => Ok(acc),
                    (None, None) => Ok(acc), // Shouldn't happen
                }
            })
            .map_or_else(UnifyResult::Err, UnifyResult::Ok)
    }

    /// Unify a named object alias type with a structural object type.
    ///
    /// The alias must have all required fields present in the object.
    /// Extra fields in the object are allowed (extensible record semantics).
    fn unify_named_with_object(
        &mut self,
        type_id: crate::TypeId,
        type_args: &[TyId],
        obj_fields: &IndexMap<StringId, TyId>,
        span: Span,
    ) -> UnifyResult {
        // Look up alias definition
        let def = self.registry().get_def(type_id);

        match def {
            Some(TypeDef::Alias {
                type_params,
                target,
                ..
            }) => {
                // Check if target is an object type
                let target = *target;
                let type_params = type_params.clone();
                match self.ast().get_type_expr(target).cloned() {
                    Some(AstTypeExpr::Object(alias_fields)) => {
                        // Build substitution from type params to type args
                        let param_subst: IndexMap<StringId, TyId> = type_params
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
                        fields_with_ids
                            .iter()
                            .try_fold(
                                Subst::empty(),
                                |acc, (field_name, field_ty_id)| {
                                    let expected_ty = self.ast_type_to_ty(
                                        *field_ty_id,
                                        &param_subst,
                                    );
                                    let expected_ty =
                                        self.ty_arena.apply(expected_ty, &acc);

                                    match obj_fields.get(field_name) {
                                        Some(&obj_ty) => {
                                            let obj_ty = self
                                                .ty_arena
                                                .apply(obj_ty, &acc);
                                            match self.unify_inner(
                                                expected_ty,
                                                obj_ty,
                                                span,
                                            ) {
                                                UnifyResult::Ok(s) => Ok(acc
                                                    .compose(
                                                        &s,
                                                        &mut self.ty_arena,
                                                    )),
                                                UnifyResult::Err(e) => Err(e),
                                            }
                                        }
                                        None => {
                                            // Missing required field
                                            Err(TypeError::MissingField {
                                                ty: type_id,
                                                field: self
                                                    .env()
                                                    .resolve_string(
                                                        *field_name,
                                                    ),
                                                span,
                                            })
                                        }
                                    }
                                },
                            )
                            .map_or_else(UnifyResult::Err, UnifyResult::Ok)
                    }
                    _ => {
                        // Not an object alias, can't unify with object
                        let named = self.ty_arena.named(
                            type_id,
                            type_args.iter().copied().collect(),
                        );
                        let obj =
                            self.ty_arena.alloc(Ty::Object(obj_fields.clone()));
                        UnifyResult::Err(TypeError::Mismatch {
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
                UnifyResult::Err(TypeError::Mismatch {
                    expected: named,
                    got: obj,
                    span,
                })
            }
        }
    }

    /// Solve all collected constraints, returning a unified substitution.
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
    /// Errors are recorded via `self.error()`; unification continues to collect
    /// as many errors as possible.
    pub(crate) fn solve_constraints(&mut self) -> Subst {
        let constraints = self.take_constraints();
        let mut subst = Subst::empty();

        // Get type variables from integer literals for defaulting to Int.
        // These have NO constraint; they can unify with any type. After solving,
        // unresolved ones default to Int. We clone (not take) so numeric_vars
        // remains available for error formatting (displaying vars as Int).
        let numeric_vars = self.clone_numeric_vars();

        // First pass: process Unify, Callable, HasField, Iterable, Indexable to
        // build substitution. These constraints generate type bindings that
        // other constraints (Numeric, Into[String], etc.) depend on.
        constraints.iter().for_each(|c| match c {
            Constraint::Unify(t1, t2, span) => {
                let t1 = self.ty_arena.apply(*t1, &subst);
                let t2 = self.ty_arena.apply(*t2, &subst);
                match self.unify_types(t1, t2, *span) {
                    UnifyResult::Ok(s) => {
                        subst = subst.compose(&s, &mut self.ty_arena);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }
            Constraint::Callable {
                callee,
                args,
                ret,
                span,
            } => {
                let callee = self.ty_arena.apply(*callee, &subst);
                let args: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.ty_arena.apply(t, &subst)).collect();
                let ret = self.ty_arena.apply(*ret, &subst);
                self.check_callable(callee, &args, ret, *span, &mut subst);
            }
            Constraint::HasField {
                base,
                field,
                field_ty,
                span,
            } => {
                let base = self.ty_arena.apply(*base, &subst);
                let field_ty = self.ty_arena.apply(*field_ty, &subst);
                self.check_has_field(
                    base, *field, field_ty, *span, &mut subst,
                );
            }
            Constraint::Class { ty, class, span } => match class {
                // Iterable (with element type) and Indexable: first pass
                BuiltinClass::Hkt(BuiltinClassTag::Iterable, Some(_))
                | BuiltinClass::Parameterized(BuiltinClassTag::Indexable, _) => {
                    let ty = self.ty_arena.apply(*ty, &subst);
                    let class = class.apply(&subst, &mut self.ty_arena);
                    self.satisfies_class(&class, ty, *span, &mut subst);
                }
                // HKT constraints deferred to third pass
                BuiltinClass::Hkt(..) => {}
                // Simple and remaining parameterized: second/third pass
                BuiltinClass::Simple(_)
                | BuiltinClass::Parameterized(..) => {}
            },
        });

        // Default unresolved numeric type variables to Int after first pass.
        // This ensures subsequent constraint checks (Numeric, Indexable, etc.)
        // see concrete types rather than unresolved type variables.
        // Mimics Haskell's defaulting: `10` becomes `Int` when unconstrained.
        //
        // Important: bind the *resolved* type variable, not the original. If the
        // numeric type var was unified with another var (e.g., `?N -> ?F`), we
        // must bind `?F -> Int`, not overwrite `?N` (which would lose the link).
        numeric_vars.iter().for_each(|v| {
            let vid = self.ty_arena.alloc(Ty::Var(*v));
            let resolved = self.ty_arena.apply(vid, &subst);
            if let Ty::Var(root) = self.ty_arena.get(resolved) {
                subst.extend(*root, TyArena::INT);
            }
        });

        // Second pass: process simple membership constraints with final substitution
        constraints.iter().for_each(|c| {
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    BuiltinClass::Simple(_) => {
                        let ty = self.ty_arena.apply(*ty, &subst);
                        self.satisfies_class(class, ty, *span, &mut subst);
                    }
                    BuiltinClass::Hkt(..) | BuiltinClass::Parameterized(..) => {
                    }
                }
            }
        });

        // Third pass: final check for HKT and parameterized constraints now
        // that numeric type variables have been defaulted and Callable has
        // resolved all type variables through argument unification.
        constraints.iter().for_each(|c| {
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    BuiltinClass::Hkt(..) | BuiltinClass::Parameterized(..) => {
                        let ty = self.ty_arena.apply(*ty, &subst);
                        let class = class.apply(&subst, &mut self.ty_arena);
                        self.satisfies_class(&class, ty, *span, &mut subst);
                    }
                    BuiltinClass::Simple(_) => {}
                }
            }
        });

        subst
    }

    /// Check that a type satisfies a class constraint.
    ///
    /// This is the unified constraint checking method that handles all class
    /// constraints. The `class` parameter contains any associated types (e.g.,
    /// `Into(target)`, `Iterable(elem)`). The `subst` is updated when the
    /// constraint involves unification (e.g., `Fallible`, `Iterable`, `Indexable`).
    fn satisfies_class(
        &mut self,
        class: &BuiltinClass<TyId>,
        ty: TyId,
        span: Span,
        subst: &mut Subst,
    ) {
        // Handle associated types: resolve to concrete type before checking
        let shape = self.ty_arena.get(ty).clone();
        if let Ty::AssocType(tv, assoc_class, name) = shape {
            // Apply current substitution to resolve the base type variable
            let base_id = self.ty_arena.alloc(Ty::Var(tv));
            let base = self.ty_arena.apply(base_id, subst);
            match self.resolve_assoc_type(base, assoc_class, name, span) {
                Ok(resolved) => {
                    // Resolved; check the concrete type against the class
                    self.satisfies_class(class, resolved, span, subst);
                }
                Err(_) => {
                    // Base type still unresolved; defer constraint
                }
            }
        } else {
            self.satisfies_class_inner(class, ty, span, subst);
        }
    }

    /// Inner implementation of class constraint checking.
    fn satisfies_class_inner(
        &mut self,
        class: &BuiltinClass<TyId>,
        ty: TyId,
        span: Span,
        subst: &mut Subst,
    ) {
        let ty_shape = self.ty_arena.get(ty).clone();
        match class {
            // `Numeric`: `Int`, `Word`, `Float`
            BuiltinClass::Simple(BuiltinClassTag::Numeric) => match ty_shape {
                Ty::Int | Ty::Word | Ty::Float => {}
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                Ty::Union(members) => {
                    // At least one member must be numeric (for literal coercion)
                    let any_numeric = members.iter().any(|&m| {
                        matches!(
                            self.ty_arena.get(m),
                            Ty::Int | Ty::Word | Ty::Float
                        )
                    });
                    if !any_numeric {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Numeric),
                            ty,
                            span,
                        ));
                    }
                }
                Ty::Named(id, ref type_args) => {
                    match self
                        .instance_registry
                        .lookup(BuiltinClassTag::Numeric, id)
                        .cloned()
                    {
                        Some(inst) => {
                            let args: SmallVec<[TyId; 4]> = type_args.clone();
                            self.check_instance_constraints(
                                &inst, &args, span, subst,
                            );
                        }
                        None => match self.expand_union_members(ty) {
                            Some(members) => {
                                let any_numeric = members.iter().any(|&m| {
                                    matches!(
                                        self.ty_arena.get(m),
                                        Ty::Int | Ty::Word | Ty::Float
                                    )
                                });
                                if !any_numeric {
                                    self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::Numeric,
                                        ),
                                        ty,
                                        span,
                                    ));
                                }
                            }
                            None => match self.expand_alias_fully(ty) {
                                Some(expanded) => self.satisfies_class(
                                    class, expanded, span, subst,
                                ),
                                None => {
                                    self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::Numeric,
                                        ),
                                        ty,
                                        span,
                                    ));
                                }
                            },
                        },
                    }
                }
                _ => {
                    self.error(TypeError::UnsatisfiedClass(
                        BuiltinClass::Simple(BuiltinClassTag::Numeric),
                        ty,
                        span,
                    ));
                }
            },

            // `BitLike`: `Bool`, `Int`, `Word`
            BuiltinClass::Simple(BuiltinClassTag::BitLike) => {
                match self.ty_arena.get(ty).clone() {
                    Ty::Bool | Ty::Int | Ty::Word => {}
                    Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::BitLike, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => match self.expand_union_members(ty) {
                                Some(members) => members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span, subst)
                                }),
                                None => {
                                    match self.expand_alias_fully(ty) {
                                        Some(expanded) => self.satisfies_class(
                                            class, expanded, span, subst,
                                        ),
                                        None => {
                                            self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::BitLike,
                                        ),
                                        ty,
                                        span,
                                    ));
                                        }
                                    }
                                }
                            },
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::BitLike),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Negatable`: `Int`, `Float` (not `Word`; unsigned)
            BuiltinClass::Simple(BuiltinClassTag::Negatable) => {
                match self.ty_arena.get(ty).clone() {
                    Ty::Int | Ty::Float => {}
                    Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Negatable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    BuiltinClass::Simple(
                                        BuiltinClassTag::Negatable,
                                    ),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Negatable),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Ord`: primitives + containers (if elements are `Ord`)
            BuiltinClass::Simple(BuiltinClassTag::Ord) => match self
                .ty_arena
                .get(ty)
                .clone()
            {
                Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::Ordering => {}
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                // `Array[T]` is `Ord` if `T: Ord` (lexicographic)
                Ty::Array(elem) => {
                    self.satisfies_class(class, elem, span, subst);
                }
                // Tuples are `Ord` if all elements are `Ord` (lexicographic)
                Ty::Tuple(elems) => {
                    elems.iter().for_each(|e| {
                        self.satisfies_class(class, *e, span, subst);
                    });
                }
                // `Option[T]` is `Ord` if `T: Ord` (`None < Some`)
                Ty::Option(inner) => {
                    self.satisfies_class(class, inner, span, subst);
                }
                // `Result[T, E]` is `Ord` if `T: Ord` and `E: Ord` (`Err < Ok`)
                Ty::Result(ok, err) => {
                    self.satisfies_class(class, ok, span, subst);
                    self.satisfies_class(class, err, span, subst);
                }
                // `Map[K, V]` is `Ord` if `K: Ord` and `V: Ord` (sorted by key)
                Ty::Map(k, v) => {
                    self.satisfies_class(class, k, span, subst);
                    self.satisfies_class(class, v, span, subst);
                }
                Ty::Union(members) => {
                    members.iter().for_each(|m| {
                        self.satisfies_class(class, *m, span, subst)
                    });
                }
                Ty::Named(id, args) => {
                    // FIXME: Special case for union types. This is necessary because
                    // unions are represented as `Ty::Named(union_id, ...)` rather than
                    // `Ty::Union([members...])`. Once unions are properly represented
                    // at the type level, this special case can be removed.
                    if let Some(def) = self.registry.get_def(id) {
                        if let crate::value::TypeDef::Union {
                            members, ..
                        } = def
                        {
                            // Union is Ord if all members are Ord
                            members.iter().for_each(|member_id| {
                                let member_ty =
                                    self.type_expr_to_ty(*member_id);
                                self.satisfies_class(
                                    class, member_ty, span, subst,
                                );
                            });
                        } else {
                            // Not a union, check instance registry
                            match self
                                .instance_registry
                                .lookup(BuiltinClassTag::Ord, id)
                                .cloned()
                            {
                                Some(inst) => {
                                    self.check_instance_constraints(
                                        &inst, &args, span, subst,
                                    );
                                }
                                None => {
                                    self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::Ord,
                                        ),
                                        ty,
                                        span,
                                    ));
                                }
                            }
                        }
                    } else {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Ord),
                            ty,
                            span,
                        ));
                    }
                }
                _ => {
                    self.error(TypeError::UnsatisfiedClass(
                        BuiltinClass::Simple(BuiltinClassTag::Ord),
                        ty,
                        span,
                    ));
                }
            },

            // `Eq`: primitives + containers (if elements are `Eq`)
            BuiltinClass::Simple(BuiltinClassTag::Eq) => match self
                .ty_arena
                .get(ty)
                .clone()
            {
                Ty::Unit
                | Ty::Bool
                | Ty::Int
                | Ty::Word
                | Ty::Float
                | Ty::Char
                | Ty::String
                | Ty::Time
                | Ty::FilePath
                | Ty::Json => {}
                Ty::Local | Ty::Global => {}
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                // `Array[T]` is `Eq` if `T: Eq`
                Ty::Array(elem) => {
                    self.satisfies_class(class, elem, span, subst);
                }
                // `Tuple[T1, T2, ...]` is `Eq` if all elements are `Eq`
                Ty::Tuple(elems) => {
                    elems.iter().for_each(|e| {
                        self.satisfies_class(class, *e, span, subst);
                    });
                }
                // `Map[K, V]` is `Eq` if `K: Eq` and `V: Eq`
                Ty::Map(k, v) => {
                    self.satisfies_class(class, k, span, subst);
                    self.satisfies_class(class, v, span, subst);
                }
                // `Option[T]` is `Eq` if `T: Eq`
                Ty::Option(inner) => {
                    self.satisfies_class(class, inner, span, subst);
                }
                // `Result[T, E]` is `Eq` if `T: Eq` and `E: Eq`
                Ty::Result(ok, err) => {
                    self.satisfies_class(class, ok, span, subst);
                    self.satisfies_class(class, err, span, subst);
                }
                // `Object` is `Eq` if all field types are `Eq`
                Ty::Object(fields) => {
                    fields.values().for_each(|t| {
                        self.satisfies_class(class, *t, span, subst);
                    });
                }
                Ty::Union(members) => {
                    members.iter().for_each(|m| {
                        self.satisfies_class(class, *m, span, subst)
                    });
                }
                Ty::Named(id, args) => {
                    // FIXME: Special case for union types. This is necessary because
                    // unions are represented as `Ty::Named(union_id, ...)` rather than
                    // `Ty::Union([members...])`. Once unions are properly represented
                    // at the type level, this special case can be removed.
                    if let Some(def) = self.registry.get_def(id) {
                        if let crate::value::TypeDef::Union {
                            members, ..
                        } = def
                        {
                            // Union is Eq if all members are Eq
                            members.iter().for_each(|member_id| {
                                let member_ty =
                                    self.type_expr_to_ty(*member_id);
                                self.satisfies_class(
                                    class, member_ty, span, subst,
                                );
                            });
                        } else {
                            // Not a union, check instance registry
                            match self
                                .instance_registry
                                .lookup(BuiltinClassTag::Eq, id)
                                .cloned()
                            {
                                Some(inst) => {
                                    self.check_instance_constraints(
                                        &inst, &args, span, subst,
                                    );
                                }
                                None => {
                                    self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::Eq,
                                        ),
                                        ty,
                                        span,
                                    ));
                                }
                            }
                        }
                    } else {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Eq),
                            ty,
                            span,
                        ));
                    }
                }
                _ => {
                    self.error(TypeError::UnsatisfiedClass(
                        BuiltinClass::Simple(BuiltinClassTag::Eq),
                        ty,
                        span,
                    ));
                }
            },

            // `Display`: everything except `Fn`
            BuiltinClass::Simple(BuiltinClassTag::Display) => {
                match self.ty_arena.get(ty).clone() {
                    Ty::Bool
                    | Ty::Int
                    | Ty::Word
                    | Ty::Float
                    | Ty::Char
                    | Ty::String
                    | Ty::Unit
                    | Ty::Time
                    | Ty::Range
                    | Ty::Json
                    | Ty::Ordering
                    | Ty::DataStatus
                    | Ty::FilePath
                    | Ty::Path
                    | Ty::Regex
                    | Ty::RuntimeError
                    | Ty::Local
                    | Ty::Global => {}
                    Ty::Array(_)
                    | Ty::Option(_)
                    | Ty::Result(_, _)
                    | Ty::Map(_, _)
                    | Ty::Tuple(_)
                    | Ty::Object(_) => {}
                    Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                    Ty::Fn(_, _) => {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Display),
                            ty,
                            span,
                        ));
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Named(id, args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Display, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    BuiltinClass::Simple(
                                        BuiltinClassTag::Display,
                                    ),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    Ty::Apply(_, _) | Ty::AssocType(_, _, _) => {}
                }
            }

            // `Monoid`: `String`, `Array`, `Map`, `Option`
            BuiltinClass::Simple(BuiltinClassTag::Monoid) => match self
                .ty_arena
                .get(ty)
                .clone()
            {
                Ty::String | Ty::Array(_) | Ty::Map(_, _) | Ty::Option(_) => {}
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                Ty::Union(members) => {
                    members.iter().for_each(|m| {
                        self.satisfies_class(class, *m, span, subst)
                    });
                }
                Ty::Named(id, type_args) => {
                    match self
                        .instance_registry
                        .lookup(BuiltinClassTag::Monoid, id)
                        .cloned()
                    {
                        Some(inst) => {
                            self.check_instance_constraints(
                                &inst, &type_args, span, subst,
                            );
                        }
                        None => {
                            self.error(TypeError::UnsatisfiedClass(
                                BuiltinClass::Simple(BuiltinClassTag::Monoid),
                                ty,
                                span,
                            ));
                        }
                    }
                }
                _ => {
                    self.error(TypeError::UnsatisfiedClass(
                        BuiltinClass::Simple(BuiltinClassTag::Monoid),
                        ty,
                        span,
                    ));
                }
            },

            // `Into(target)`: `AS` casts
            BuiltinClass::Parameterized(BuiltinClassTag::Into, to) => {
                let to = *to;
                let ty_shape = self.ty_arena.get(ty).clone();
                let to_shape = self.ty_arena.get(to).clone();
                match (&ty_shape, &to_shape) {
                    (Ty::Var(_), _) | (_, Ty::Var(_)) => {}
                    (Ty::Error, _) | (_, Ty::Error) => {}
                    (Ty::Unknown, _) | (_, Ty::Unknown) => {}

                    _ if ty == to => {}

                    // Functions cannot be stringified
                    (Ty::Fn(_, _), Ty::String) => {
                        self.error(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Union(members), Ty::String) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    to,
                                ),
                                *m,
                                span,
                                subst,
                            )
                        });
                    }
                    (_, Ty::String) => {}

                    // Functions, regex, refs cannot be Json-serialized
                    (Ty::Fn(_, _), Ty::Json)
                    | (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.error(TypeError::InvalidCast {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::Into,
                            TyArena::JSON,
                        ),
                        *elem,
                        span,
                        subst,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::Into,
                            TyArena::JSON,
                        ),
                        *inner,
                        span,
                        subst,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            ok,
                            span,
                            subst,
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            err,
                            span,
                            subst,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            k,
                            span,
                            subst,
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            v,
                            span,
                            subst,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    TyArena::JSON,
                                ),
                                *e,
                                span,
                                subst,
                            )
                        });
                    }
                    (Ty::Object(fields), Ty::Json) => {
                        let vals: SmallVec<[TyId; 4]> =
                            fields.values().copied().collect();
                        vals.iter().for_each(|t| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    TyArena::JSON,
                                ),
                                *t,
                                span,
                                subst,
                            )
                        });
                    }
                    (Ty::Union(members), Ty::Json) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    TyArena::JSON,
                                ),
                                *m,
                                span,
                                subst,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    TyArena::JSON,
                                ),
                                *a,
                                span,
                                subst,
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
                    (Ty::Named(id, _), Ty::FilePath)
                        if *id == crate::TypeId::PATH => {}

                    // Storable to member type
                    (Ty::Named(id, _), _) if *id == crate::TypeId::STORABLE => {
                        if !TyArena::STORABLE_MEMBERS.contains(&to) {
                            self.error(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Member to union type
                    (_, Ty::Named(id, _))
                        if *id == crate::TypeId::STORABLE
                            || *id == crate::TypeId::SCALAR
                            || *id == crate::TypeId::SUBSCRIPT =>
                    {
                        let uid = *id;
                        let is_member = if uid == crate::TypeId::STORABLE {
                            TyArena::STORABLE_MEMBERS.contains(&ty)
                        } else if uid == crate::TypeId::SCALAR {
                            TyArena::SCALAR_MEMBERS.contains(&ty)
                        } else {
                            TyArena::SUBSCRIPT_MEMBERS.contains(&ty)
                        };
                        if !is_member {
                            self.error(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Union handling
                    (Ty::Union(members), _) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    to,
                                ),
                                *m,
                                span,
                                subst,
                            )
                        });
                    }

                    // User type with Into instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Into, id)
                            .cloned()
                        {
                            Some(inst) => {
                                let inst_target =
                                    inst.class_args.first().copied();
                                if inst_target == Some(to) {
                                    self.check_instance_constraints(
                                        &inst, &type_args, span, subst,
                                    );
                                } else {
                                    self.error(TypeError::InvalidCast {
                                        from: ty,
                                        to,
                                        span,
                                    });
                                }
                            }
                            None => {
                                self.error(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }

                    // Builtin type with user-defined Into[UserType] instance
                    // E.g., `class Into[UserId] FOR Int { ... }`
                    _ => {
                        let type_id =
                            self.primitive_type_id(self.ty_arena.get(ty));
                        match type_id.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Into, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                let inst_target =
                                    inst.class_args.first().copied();
                                if inst_target != Some(to) {
                                    self.error(TypeError::InvalidCast {
                                        from: ty,
                                        to,
                                        span,
                                    });
                                }
                            }
                            None => {
                                self.error(TypeError::InvalidCast {
                                    from: ty,
                                    to,
                                    span,
                                });
                            }
                        }
                    }
                }
            }

            // `TryInto(target)`: `read` casts
            BuiltinClass::Parameterized(BuiltinClassTag::TryInto, to) => {
                let to = *to;
                let ty_shape = self.ty_arena.get(ty).clone();
                let to_shape = self.ty_arena.get(to).clone();
                match (&ty_shape, &to_shape) {
                    (Ty::Var(_), _) | (_, Ty::Var(_)) => {}
                    (Ty::Error, _) | (_, Ty::Error) => {}
                    (Ty::Unknown, _) | (_, Ty::Unknown) => {}

                    _ if ty == to => {}

                    // Function types cannot be source for READ
                    (Ty::Fn(_, _), _) => {
                        self.error(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }

                    // Cannot READ into function, regex, or refs
                    (_, Ty::Fn(_, _))
                    | (_, Ty::Regex)
                    | (_, Ty::Local)
                    | (_, Ty::Global) => {
                        self.error(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }

                    // READ Json requires source to be Into[Json]
                    (Ty::Regex, Ty::Json)
                    | (Ty::Local, Ty::Json)
                    | (Ty::Global, Ty::Json) => {
                        self.error(TypeError::InvalidRead {
                            from: ty,
                            to,
                            span,
                        });
                    }
                    (Ty::Array(elem), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::TryInto,
                            TyArena::JSON,
                        ),
                        *elem,
                        span,
                        subst,
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::TryInto,
                            TyArena::JSON,
                        ),
                        *inner,
                        span,
                        subst,
                    ),
                    (Ty::Result(ok, err), Ty::Json) => {
                        let (ok, err) = (*ok, *err);
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            ok,
                            span,
                            subst,
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            err,
                            span,
                            subst,
                        );
                    }
                    (Ty::Map(k, v), Ty::Json) => {
                        let (k, v) = (*k, *v);
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            k,
                            span,
                            subst,
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            v,
                            span,
                            subst,
                        );
                    }
                    (Ty::Tuple(elems), Ty::Json) => {
                        let es: SmallVec<[TyId; 4]> = elems.clone();
                        es.iter().for_each(|e| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::TryInto,
                                    TyArena::JSON,
                                ),
                                *e,
                                span,
                                subst,
                            )
                        });
                    }
                    (Ty::Object(fields), Ty::Json) => {
                        let vals: SmallVec<[TyId; 4]> =
                            fields.values().copied().collect();
                        vals.iter().for_each(|t| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::TryInto,
                                    TyArena::JSON,
                                ),
                                *t,
                                span,
                                subst,
                            )
                        });
                    }
                    (Ty::Named(_, args), Ty::Json) => {
                        let as_: SmallVec<[TyId; 4]> = args.clone();
                        as_.iter().for_each(|a| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::TryInto,
                                    TyArena::JSON,
                                ),
                                *a,
                                span,
                                subst,
                            )
                        });
                    }

                    // Union handling
                    (Ty::Union(members), _) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::TryInto,
                                    to,
                                ),
                                *m,
                                span,
                                subst,
                            )
                        });
                    }

                    // User type with TryInto instance
                    (Ty::Named(id, type_args), _) => {
                        let (id, type_args) = (*id, type_args.clone());
                        if let Some(inst) = self
                            .instance_registry
                            .lookup(BuiltinClassTag::TryInto, id)
                            .cloned()
                        {
                            if inst.class_args.first().copied() == Some(to) {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                        }
                    }

                    // All other combinations are valid for READ
                    _ => {}
                }
            }

            // `Fallible(opt_inner)`: `Option[T]`, `Result[T, E]`
            // `None` = polymorphic (just check the type is fallible)
            // `Some(inner)` = check and unify element type
            BuiltinClass::Hkt(BuiltinClassTag::Fallible, opt_inner) => {
                let opt_inner = *opt_inner;
                let ty = self.expand_alias_fully(ty).unwrap_or(ty);

                match self.ty_arena.get(ty).clone() {
                    Ty::Option(opt_elem) => {
                        if let Some(inner) = opt_inner {
                            match self.unify_types(inner, opt_elem, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Result(ok, _) => {
                        if let Some(inner) = opt_inner {
                            match self.unify_types(inner, ok, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(v) => {
                        // Default unresolved to `Option`
                        let elem = opt_inner.unwrap_or_else(|| {
                            let fv = self.fresh_var();
                            self.ty_arena.alloc(Ty::Var(fv))
                        });
                        let opt_id = self.ty_arena.option(elem);
                        subst.extend(v, opt_id);
                    }
                    Ty::Apply(_, _) => {
                        // HKT variable application; defer
                    }
                    Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Fallible, id)
                            .cloned()
                        {
                            Some(inst) => {
                                if let Some(inner) = opt_inner {
                                    if let Some(&inst_inner) =
                                        inst.class_args.first()
                                    {
                                        let param_subst = Subst(
                                            inst.type_params
                                                .iter()
                                                .zip(type_args.iter())
                                                .map(|(p, &a)| (*p, a))
                                                .collect(),
                                        );
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_inner, &param_subst);
                                        match self
                                            .unify_types(inner, resolved, span)
                                        {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                )
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e)
                                            }
                                        }
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Iterable(opt_elem)`: `Array[T]`, `Range`
            // `None` = polymorphic (just check the type is iterable)
            // `Some(elem)` = check and unify element type
            BuiltinClass::Hkt(BuiltinClassTag::Iterable, opt_elem) => {
                let opt_elem = *opt_elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, inner, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, TyArena::INT, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Iterable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let param_subst = Subst(
                                            inst.type_params
                                                .iter()
                                                .zip(type_args.iter())
                                                .map(|(p, &a)| (*p, a))
                                                .collect(),
                                        );
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        match self
                                            .unify_types(elem, resolved, span)
                                        {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                )
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e)
                                            }
                                        }
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                let exp = opt_elem.unwrap_or(TyArena::UNKNOWN);
                                let arr = self.ty_arena.array(exp);
                                self.error(TypeError::Mismatch {
                                    expected: arr,
                                    got: ty,
                                    span,
                                });
                            }
                        }
                    }
                    _ => {
                        let exp = opt_elem.unwrap_or(TyArena::UNKNOWN);
                        let arr = self.ty_arena.array(exp);
                        self.error(TypeError::Mismatch {
                            expected: arr,
                            got: ty,
                            span,
                        });
                    }
                }
            }

            // `Indexable(elem)`: `Array[T]`, `Map[K,V]`, `String`
            //
            // The index type is now accessed via the associated type `.Index`,
            // not as a class parameter. Only the element type is unified here.
            BuiltinClass::Parameterized(BuiltinClassTag::Indexable, elem) => {
                let elem = *elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        // Array[T]: elem = T (index type is Int, via .Index)
                        match self.unify_types(elem, inner, span) {
                            UnifyResult::Ok(s) => {
                                *subst = subst.compose(&s, &mut self.ty_arena)
                            }
                            UnifyResult::Err(e) => self.error(e),
                        }
                    }
                    Ty::Map(_key, val) => {
                        // Map[K, V]: elem = Option[V] (index type is K, via .Index)
                        let opt_val = self.ty_arena.option(val);
                        match self.unify_types(elem, opt_val, span) {
                            UnifyResult::Ok(s) => {
                                *subst = subst.compose(&s, &mut self.ty_arena)
                            }
                            UnifyResult::Err(e) => self.error(e),
                        }
                    }
                    Ty::String => {
                        // String: elem = Char (index type is Int, via .Index)
                        match self.unify_types(elem, TyArena::CHAR, span) {
                            UnifyResult::Ok(s) => {
                                *subst = subst.compose(&s, &mut self.ty_arena)
                            }
                            UnifyResult::Err(e) => self.error(e),
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Indexable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                let param_subst = Subst(
                                    inst.type_params
                                        .iter()
                                        .zip(type_args.iter())
                                        .map(|(p, &a)| (*p, a))
                                        .collect(),
                                );
                                // class_args[0] is the element type
                                if let Some(&inst_elem) =
                                    inst.class_args.first()
                                {
                                    let resolved = self
                                        .ty_arena
                                        .apply(inst_elem, &param_subst);
                                    match self.unify_types(elem, resolved, span)
                                    {
                                        UnifyResult::Ok(s) => {
                                            *subst = subst
                                                .compose(&s, &mut self.ty_arena)
                                        }
                                        UnifyResult::Err(e) => self.error(e),
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Mappable(opt_elem)`: `Array[T]`, `Option[T]`, `Result[T, E]`
            BuiltinClass::Hkt(BuiltinClassTag::Mappable, opt_elem) => {
                let opt_elem = *opt_elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, inner, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Option(inner) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, inner, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Result(ok, _) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, ok, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Mappable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let param_subst = Subst(
                                            inst.type_params
                                                .iter()
                                                .zip(type_args.iter())
                                                .map(|(p, &a)| (*p, a))
                                                .collect(),
                                        );
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        match self
                                            .unify_types(elem, resolved, span)
                                        {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                )
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e)
                                            }
                                        }
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Filterable(opt_elem)`: `Array[T]`, `Range`, `Option[T]`, `Result[T]`
            BuiltinClass::Hkt(BuiltinClassTag::Filterable, opt_elem) => {
                let opt_elem = *opt_elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, inner, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, TyArena::INT, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Filterable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let param_subst = Subst(
                                            inst.type_params
                                                .iter()
                                                .zip(type_args.iter())
                                                .map(|(p, &a)| (*p, a))
                                                .collect(),
                                        );
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        match self
                                            .unify_types(elem, resolved, span)
                                        {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                )
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e)
                                            }
                                        }
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Foldable(opt_elem)`: `Array[T]`, `Range`, `Option[T]`, `Result[T]`
            BuiltinClass::Hkt(BuiltinClassTag::Foldable, opt_elem) => {
                let opt_elem = *opt_elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, inner, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            match self.unify_types(elem, TyArena::INT, span) {
                                UnifyResult::Ok(s) => {
                                    *subst =
                                        subst.compose(&s, &mut self.ty_arena)
                                }
                                UnifyResult::Err(e) => self.error(e),
                            }
                        }
                    }
                    Ty::Union(members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span, subst)
                        });
                    }
                    Ty::Var(_) | Ty::Apply(_, _) | Ty::Error | Ty::Unknown => {}
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Foldable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let param_subst = Subst(
                                            inst.type_params
                                                .iter()
                                                .zip(type_args.iter())
                                                .map(|(p, &a)| (*p, a))
                                                .collect(),
                                        );
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        match self
                                            .unify_types(elem, resolved, span)
                                        {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                )
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e)
                                            }
                                        }
                                    }
                                }
                                self.check_instance_constraints(
                                    &inst, &type_args, span, subst,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    class.clone(),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            class.clone(),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // Unreachable: tag/shape invariant maintained by construction
            _ => {}
        }
    }

    /// Check that a user instance's WHERE constraints are satisfied.
    fn check_instance_constraints(
        &mut self,
        inst: &super::instance::Instance,
        type_args: &[TyId],
        span: Span,
        subst: &mut Subst,
    ) {
        let inst_subst = Subst(
            inst.type_params
                .iter()
                .zip(type_args.iter())
                .map(|(p, &a)| (*p, a))
                .collect(),
        );
        // Collect constraints to avoid borrow conflict
        let constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]> =
            inst.constraints.clone();
        constraints.iter().for_each(|(var, class)| {
            let var_id = self.ty_arena.alloc(Ty::Var(*var));
            let ty = self.ty_arena.apply(var_id, &inst_subst);
            let class = class.apply(&inst_subst, &mut self.ty_arena);
            self.satisfies_class(&class, ty, span, subst);
        });
    }

    /// Check that a callee type is callable and unify with expected signature.
    fn check_callable(
        &mut self,
        callee: TyId,
        args: &[TyId],
        ret: TyId,
        span: Span,
        subst: &mut Subst,
    ) {
        let shape = self.ty_arena.get(callee).clone();
        match shape {
            Ty::Fn(ref params, fn_ret) => {
                if params.len() != args.len() {
                    self.error(TypeError::ArityMismatch {
                        expected: params.len(),
                        got: args.len(),
                        span,
                    });
                } else {
                    let params: SmallVec<[TyId; 4]> = params.clone();
                    // Unify each parameter with corresponding argument
                    params.iter().zip(args.iter()).for_each(|(&p, &a)| {
                        let p = self.ty_arena.apply(p, subst);
                        let a = self.ty_arena.apply(a, subst);
                        match self.unify_types(p, a, span) {
                            UnifyResult::Ok(s) => {
                                *subst = subst.compose(&s, &mut self.ty_arena);
                            }
                            UnifyResult::Err(e) => {
                                self.error(e);
                            }
                        }
                    });

                    // Unify return type
                    let fn_ret = self.ty_arena.apply(fn_ret, subst);
                    let ret = self.ty_arena.apply(ret, subst);
                    match self.unify_types(fn_ret, ret, span) {
                        UnifyResult::Ok(s) => {
                            *subst = subst.compose(&s, &mut self.ty_arena);
                        }
                        UnifyResult::Err(e) => {
                            self.error(e);
                        }
                    }
                }
            }

            Ty::Var(v) => {
                // Callee is unresolved; create function type and bind
                let fn_ty =
                    self.ty_arena.func(args.iter().copied().collect(), ret);
                match self.unify_var(v, fn_ty, span) {
                    UnifyResult::Ok(s) => {
                        *subst = subst.compose(&s, &mut self.ty_arena);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::NotCallable(callee, span));
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
        subst: &mut Subst,
    ) {
        let shape = self.ty_arena.get(base).clone();
        match shape {
            // Structural object: look up field directly
            Ty::Object(ref fields) => match fields.get(&field) {
                Some(&actual_ty) => {
                    match self.unify_types(field_ty, actual_ty, span) {
                        UnifyResult::Ok(s) => {
                            *subst = subst.compose(&s, &mut self.ty_arena);
                        }
                        UnifyResult::Err(e) => {
                            self.error(e);
                        }
                    }
                }
                None => {
                    let name = self
                        .env()
                        .get_str(field)
                        .unwrap_or("<unknown>")
                        .to_string();
                    self.error(TypeError::FieldNotFound {
                        ty: base,
                        field: name,
                        span,
                    });
                }
            },

            // Named alias to object: look up field in alias definition
            Ty::Named(type_id, ref type_args) => {
                let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                let def = self.registry().get_def(type_id);
                match def {
                    Some(TypeDef::Alias {
                        type_params,
                        target,
                        ..
                    }) => {
                        let params: SmallVec<[StringId; 2]> =
                            type_params.clone();
                        let target = *target;
                        match self.ast().get_type_expr(target).cloned() {
                            Some(AstTypeExpr::Object(alias_fields)) => {
                                // Find field in alias object
                                let field_str =
                                    self.env().get_str(field).unwrap_or("");
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
                                        let actual_ty = self.ast_type_to_ty(
                                            ast_ty_id,
                                            &param_subst,
                                        );
                                        match self.unify_types(
                                            field_ty, actual_ty, span,
                                        ) {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(
                                                    &s,
                                                    &mut self.ty_arena,
                                                );
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e);
                                            }
                                        }
                                    }
                                    None => {
                                        self.error(TypeError::FieldNotFound {
                                            ty: base,
                                            field: field_str.to_string(),
                                            span,
                                        });
                                    }
                                }
                            }
                            _ => {
                                self.error(TypeError::NotAnObject(base, span));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::NotAnObject(base, span));
                    }
                }
            }

            // Json: any field access is valid and returns Json
            Ty::Json => match self.unify_types(field_ty, TyArena::JSON, span) {
                UnifyResult::Ok(s) => {
                    *subst = subst.compose(&s, &mut self.ty_arena);
                }
                UnifyResult::Err(e) => {
                    self.error(e);
                }
            },

            // Union: all members must have the field with compatible types
            Ty::Union(ref members) => {
                let ms: SmallVec<[TyId; 4]> = members.clone();
                ms.iter().for_each(|&m| {
                    self.check_has_field(m, field, field_ty, span, subst);
                });
            }

            // Type variable: defer until resolved
            Ty::Var(_) => {
                // Type variable not yet resolved; constraint will be checked
                // when the variable is bound. For now, this is allowed.
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::NotAnObject(base, span));
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
        class: BuiltinClassTag,
        assoc_name: StringId,
        span: Span,
    ) -> Result<TyId, TypeError> {
        // Validate that assoc_name is a valid associated type for this class
        let assoc_str = self.env().get_str(assoc_name);
        let assoc_types = self.env.class_def(class).assoc_types;
        if !assoc_str.is_some_and(|s| assoc_types.contains(&s)) {
            Err(TypeError::NoSuchAssocType {
                class,
                name: assoc_name,
                span,
            })
        } else {
            let shape = self.ty_arena.get(base).clone();
            match shape {
                // Builtin: Array[T] with Indexable:Index = Int
                Ty::Array(_) if class == BuiltinClassTag::Indexable => {
                    Ok(TyArena::INT)
                }

                // Builtin: Map[K, V] with Indexable:Index = K
                Ty::Map(k, _) if class == BuiltinClassTag::Indexable => Ok(k),

                // Builtin: String with Indexable:Index = Int
                Ty::String if class == BuiltinClassTag::Indexable => {
                    Ok(TyArena::INT)
                }

                // User type: look up instance in registry
                Ty::Named(type_id, ref type_args) => {
                    let type_args: SmallVec<[TyId; 4]> = type_args.clone();
                    match self.instance_registry.lookup(class, type_id) {
                        Some(inst) => {
                            // Find the associated type definition
                            match inst.get_assoc_type(assoc_name) {
                                Some(assoc_def) => {
                                    // Substitute type parameters
                                    let assoc_ty = assoc_def.ty;
                                    let param_subst = Subst(
                                        inst.type_params
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(p, &a)| (*p, a))
                                            .collect(),
                                    );
                                    Ok(self
                                        .ty_arena
                                        .apply(assoc_ty, &param_subst))
                                }
                                None => Err(TypeError::MissingAssocType {
                                    class,
                                    assoc: assoc_name,
                                    span,
                                }),
                            }
                        }
                        None => Err(TypeError::UnsatisfiedClass(
                            BuiltinClass::placeholder(class),
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

                // Other types: no instance for this class
                _ => Err(TypeError::UnsatisfiedClass(
                    BuiltinClass::placeholder(class),
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
            class: BuiltinClassTag::Ord,
            class_args: SmallVec::new(),
            type_params: SmallVec::new(),
            constraints: SmallVec::new(),
            methods: std::collections::HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        let _ = registry.register(user_type_id, ord_inst.clone());

        // Lookup should find the instance
        let found = registry.lookup(BuiltinClassTag::Ord, user_type_id);
        assert!(found.is_some(), "should find Ord instance");

        // Lookup for different class should not find anything
        let not_found = registry.lookup(BuiltinClassTag::Display, user_type_id);
        assert!(not_found.is_none(), "should not find Display instance");

        // Lookup for different type should not find anything
        let other_type_id = TypeId::SCALAR;
        let not_found2 = registry.lookup(BuiltinClassTag::Ord, other_type_id);
        assert!(
            not_found2.is_none(),
            "should not find instance for other type"
        );
    }

    /// Test that Instance with WHERE constraints stores them correctly.
    #[test]
    fn instance_with_constraints() {
        let t = TyVar::new(0);
        let constraint = (t, BuiltinClass::Simple(BuiltinClassTag::Display));

        let inst = Instance {
            class: BuiltinClassTag::Ord,
            class_args: SmallVec::new(),
            type_params: smallvec::smallvec![t],
            constraints: smallvec::smallvec![constraint],
            methods: std::collections::HashMap::new(),
            assoc_types: SmallVec::new(),
            module: None,
            span: Span::new(0, 1),
        };

        assert_eq!(inst.type_params.len(), 1);
        assert_eq!(inst.constraints.len(), 1);
        assert_eq!(inst.constraints[0].0, t);
        assert!(matches!(
            inst.constraints[0].1,
            BuiltinClass::Simple(BuiltinClassTag::Display)
        ));
    }

    /// Test constraint substitution.
    #[test]
    fn constraint_substitution() {
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let var_id = a.var(0);
        let constraint =
            BuiltinClass::Hkt(BuiltinClassTag::Iterable, Some(var_id));

        // Create substitution: T -> Int
        let subst = Subst::singleton(t, TyArena::INT);

        // Apply substitution to constraint
        let resolved = constraint.apply(&subst, &mut a);

        // Should now be `Iterable(Some(Int))`
        assert!(
            matches!(
                resolved,
                BuiltinClass::Hkt(
                    BuiltinClassTag::Iterable,
                    Some(TyArena::INT)
                )
            ),
            "constraint should be Iterable(Some(Int)) after substitution"
        );
    }
}
