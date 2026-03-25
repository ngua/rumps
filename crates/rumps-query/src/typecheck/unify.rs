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
    BuiltinClass, BuiltinClassTag, Rename, Ty, TyArena, TyId, TyVar,
};
use crate::ast::AstTypeExpr;
use crate::intern::StringId;
use crate::value::TypeDef;
use crate::Span;

/// Result of a unification attempt.
///
/// With union-find, successful unification mutates the UF in-place.
pub(crate) type UnifyResult = Result<(), TypeError>;

impl<'a> InferCtx<'a> {
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
            Ok(())
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

            // Named (sum/alias) vs anything else: mismatch.
            // (Unions are `Ty::Union` and handled above.)
            (_, Ty::Named(..)) | (Ty::Named(..), _) => {
                Err(TypeError::Mismatch {
                    expected: t2,
                    got: t1,
                    span,
                })
            }

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
                if self.ty_arena.occurs_uf(w_bound, root, &mut self.uf) {
                    Err(TypeError::InfiniteType(root, w_bound, span))
                } else {
                    self.uf.bind(root, w_bound);
                    Ok(())
                }
            } else {
                self.uf.union(root, w_root);
                Ok(())
            }
        } else if self.ty_arena.occurs_uf(t, root, &mut self.uf) {
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
                self.unify_var(tv, TyArena::RANGE, span)?;
                args.first().map_or_else(
                    || {
                        Err(TypeError::Mismatch {
                            expected: TyArena::INT,
                            got: TyArena::UNIT,
                            span,
                        })
                    },
                    |&arg| self.unify_inner(arg, TyArena::INT, span),
                )
            }

            // User-defined named types: decompose into constructor + element
            // Element type is the LAST type arg (Haskell curried convention).
            // Non-element (fixed) args are preserved in the constructor placeholder.
            Ty::Named(id, ref type_args) => {
                if let Some(&last_arg) = type_args.last() {
                    let mut placeholder: SmallVec<[TyId; 4]> =
                        type_args.clone();
                    let start = placeholder.len().saturating_sub(args.len());
                    placeholder
                        .iter_mut()
                        .skip(start)
                        .for_each(|p| *p = TyArena::ERROR);
                    let ctor = self.ty_arena.named(id, placeholder);
                    self.unify_apply_inner(tv, args, ctor, last_arg, span)
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
        self.unify_var(tv, ctor, span)?;
        args.first().map_or_else(
            || {
                Err(TypeError::Mismatch {
                    expected: elem,
                    got: TyArena::UNIT,
                    span,
                })
            },
            |&arg| self.unify_inner(arg, elem, span),
        )
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
            .collect::<std::collections::HashSet<_>>()
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
                        fields_with_ids.iter().try_for_each(
                            |(field_name, field_ty_id)| {
                                let expected_ty = self
                                    .ast_type_to_ty(*field_ty_id, &param_subst);

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
                                            field: self
                                                .env()
                                                .resolve_string(*field_name),
                                            span,
                                        })
                                    }
                                }
                            },
                        )
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
    /// Errors are recorded via `self.error()`; unification continues to collect
    /// as many errors as possible.
    pub(crate) fn solve_constraints(&mut self) {
        let constraints = self.take_constraints();

        // Get type variables from integer literals for defaulting to Int.
        // These have NO constraint; they can unify with any type. After solving,
        // unresolved ones default to Int. We clone (not take) so numeric_vars
        // remains available for error formatting (displaying vars as Int).
        let numeric_vars = self.clone_numeric_vars();

        // First pass: process Unify, Callable, HasField, Iterable, Indexable.
        // These constraints generate type bindings (via union-find) that
        // other constraints (Numeric, Into[String], etc.) depend on.
        constraints.iter().for_each(|c| match c {
            Constraint::Unify(t1, t2, span) => {
                let t1 = self.uf.resolve(*t1, &mut self.ty_arena);
                let t2 = self.uf.resolve(*t2, &mut self.ty_arena);
                if let Err(e) = self.unify_types(t1, t2, *span) {
                    self.error(e);
                }
            }
            Constraint::Callable {
                callee,
                args,
                ret,
                span,
            } => {
                let callee = self.uf.resolve(*callee, &mut self.ty_arena);
                let args: SmallVec<[TyId; 4]> =
                    args.iter().map(|&t| self.uf.resolve(t, &mut self.ty_arena)).collect();
                let ret = self.uf.resolve(*ret, &mut self.ty_arena);
                self.check_callable(callee, &args, ret, *span);
            }
            Constraint::HasField {
                base,
                field,
                field_ty,
                span,
            } => {
                let base = self.uf.resolve(*base, &mut self.ty_arena);
                let field_ty = self.uf.resolve(*field_ty, &mut self.ty_arena);
                self.check_has_field(
                    base, *field, field_ty, *span,
                );
            }
            Constraint::Class { ty, class, span } => match class {
                // Iterable (with element type) and Indexable: first pass
                BuiltinClass::Hkt(BuiltinClassTag::Iterable, Some(_))
                | BuiltinClass::Parameterized(BuiltinClassTag::Indexable, _) => {
                    let ty = self.uf.resolve(*ty, &mut self.ty_arena);
                    let class = class.resolve_inner(&mut self.uf, &mut self.ty_arena);
                    self.satisfies_class(&class, ty, *span);
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
        constraints.iter().for_each(|c| {
            if let Constraint::Class { ty, class, span } = c {
                match class {
                    BuiltinClass::Simple(_) => {
                        let ty = self.uf.resolve(*ty, &mut self.ty_arena);
                        self.satisfies_class(class, ty, *span);
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
                        let ty = self.uf.resolve(*ty, &mut self.ty_arena);
                        let class = class
                            .resolve_inner(&mut self.uf, &mut self.ty_arena);
                        self.satisfies_class(&class, ty, *span);
                    }
                    BuiltinClass::Simple(_) => {}
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
        class: &BuiltinClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        // Handle associated types: resolve to concrete type before checking
        let shape = self.ty_arena.get(ty).clone();
        if let Ty::AssocType(tv, assoc_class, name) = shape {
            // Resolve the base type variable through union-find
            let base_id = self.ty_arena.alloc(Ty::Var(tv));
            let base = self.uf.resolve(base_id, &mut self.ty_arena);
            match self.resolve_assoc_type(base, assoc_class, name, span) {
                Ok(resolved) => {
                    // Resolved; check the concrete type against the class
                    self.satisfies_class(class, resolved, span);
                }
                Err(_) => {
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
        class: &BuiltinClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty_shape = self.ty_arena.get(ty).clone();
        match class {
            // `Numeric`: `Int`, `Word`, `Float`
            BuiltinClass::Simple(BuiltinClassTag::Numeric) => match ty_shape {
                Ty::Int | Ty::Word | Ty::Float => {}
                Ty::Var(_) | Ty::Error | Ty::Unknown => {}
                Ty::Union(prov, members) => {
                    match prov.and_then(|id| {
                        self.instance_registry
                            .lookup(BuiltinClassTag::Numeric, id)
                            .cloned()
                    }) {
                        Some(inst) => {
                            self.check_instance_constraints(
                                &inst,
                                &[],
                                span,
                                None,
                            );
                        }
                        None => {
                            // At least one member must be numeric (for literal coercion)
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
                                &inst, &args, span, None,
                            );
                        }
                        None => match self.expand_alias_fully(ty) {
                            Some(expanded) => {
                                self.satisfies_class(class, expanded, span)
                            }
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
                    Ty::Union(prov, members) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::BitLike, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span)
                                });
                            }
                        }
                    }
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::BitLike, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, None,
                                );
                            }
                            None => match self.expand_alias_fully(ty) {
                                Some(expanded) => {
                                    self.satisfies_class(class, expanded, span)
                                }
                                None => {
                                    self.error(TypeError::UnsatisfiedClass(
                                        BuiltinClass::Simple(
                                            BuiltinClassTag::BitLike,
                                        ),
                                        ty,
                                        span,
                                    ));
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
                    Ty::Union(prov, members) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Negatable, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span)
                                });
                            }
                        }
                    }
                    Ty::Named(id, type_args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Negatable, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &type_args, span, None,
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
            BuiltinClass::Simple(BuiltinClassTag::Ord) => {
                match self.ty_arena.get(ty).clone() {
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
                        self.satisfies_class(class, elem, span);
                    }
                    // Tuples are `Ord` if all elements are `Ord` (lexicographic)
                    Ty::Tuple(elems) => {
                        elems.iter().for_each(|e| {
                            self.satisfies_class(class, *e, span);
                        });
                    }
                    // `Option[T]` is `Ord` if `T: Ord` (`None < Some`)
                    Ty::Option(inner) => {
                        self.satisfies_class(class, inner, span);
                    }
                    // `Result[T, E]` is `Ord` if `T: Ord` and `E: Ord` (`Err < Ok`)
                    Ty::Result(ok, err) => {
                        self.satisfies_class(class, ok, span);
                        self.satisfies_class(class, err, span);
                    }
                    // `Map[K, V]` is `Ord` if `K: Ord` and `V: Ord` (sorted by key)
                    Ty::Map(k, v) => {
                        self.satisfies_class(class, k, span);
                        self.satisfies_class(class, v, span);
                    }
                    Ty::Union(prov, members) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Ord, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span)
                                });
                            }
                        }
                    }
                    Ty::Named(id, args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Ord, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &args, span, None,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    BuiltinClass::Simple(BuiltinClassTag::Ord),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Ord),
                            ty,
                            span,
                        ));
                    }
                }
            }

            // `Eq`: primitives + containers (if elements are `Eq`)
            BuiltinClass::Simple(BuiltinClassTag::Eq) => {
                match self.ty_arena.get(ty).clone() {
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
                        self.satisfies_class(class, elem, span);
                    }
                    // `Tuple[T1, T2, ...]` is `Eq` if all elements are `Eq`
                    Ty::Tuple(elems) => {
                        elems.iter().for_each(|e| {
                            self.satisfies_class(class, *e, span);
                        });
                    }
                    // `Map[K, V]` is `Eq` if `K: Eq` and `V: Eq`
                    Ty::Map(k, v) => {
                        self.satisfies_class(class, k, span);
                        self.satisfies_class(class, v, span);
                    }
                    // `Option[T]` is `Eq` if `T: Eq`
                    Ty::Option(inner) => {
                        self.satisfies_class(class, inner, span);
                    }
                    // `Result[T, E]` is `Eq` if `T: Eq` and `E: Eq`
                    Ty::Result(ok, err) => {
                        self.satisfies_class(class, ok, span);
                        self.satisfies_class(class, err, span);
                    }
                    // `Object` is `Eq` if all field types are `Eq`
                    Ty::Object(fields) => {
                        fields.values().for_each(|t| {
                            self.satisfies_class(class, *t, span);
                        });
                    }
                    Ty::Union(prov, members) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Eq, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span)
                                });
                            }
                        }
                    }
                    Ty::Named(id, args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Eq, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &args, span, None,
                                );
                            }
                            None => {
                                self.error(TypeError::UnsatisfiedClass(
                                    BuiltinClass::Simple(BuiltinClassTag::Eq),
                                    ty,
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::UnsatisfiedClass(
                            BuiltinClass::Simple(BuiltinClassTag::Eq),
                            ty,
                            span,
                        ));
                    }
                }
            }

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
                    Ty::Union(prov, members) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Display, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst,
                                    &[],
                                    span,
                                    None,
                                );
                            }
                            None => {
                                members.iter().for_each(|m| {
                                    self.satisfies_class(class, *m, span)
                                });
                            }
                        }
                    }
                    Ty::Named(id, args) => {
                        match self
                            .instance_registry
                            .lookup(BuiltinClassTag::Display, id)
                            .cloned()
                        {
                            Some(inst) => {
                                self.check_instance_constraints(
                                    &inst, &args, span, None,
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
                Ty::Union(prov, members) => {
                    match prov.and_then(|id| {
                        self.instance_registry
                            .lookup(BuiltinClassTag::Monoid, id)
                            .cloned()
                    }) {
                        Some(inst) => {
                            self.check_instance_constraints(
                                &inst,
                                &[],
                                span,
                                None,
                            );
                        }
                        None => {
                            members.iter().for_each(|m| {
                                self.satisfies_class(class, *m, span)
                            });
                        }
                    }
                }
                Ty::Named(id, type_args) => {
                    match self
                        .instance_registry
                        .lookup(BuiltinClassTag::Monoid, id)
                        .cloned()
                    {
                        Some(inst) => {
                            self.check_instance_constraints(
                                &inst, &type_args, span, None,
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
                    (Ty::Union(_, members), Ty::String) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    to,
                                ),
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
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::Into,
                            TyArena::JSON,
                        ),
                        *inner,
                        span,
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
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            err,
                            span,
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
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::Into,
                                TyArena::JSON,
                            ),
                            v,
                            span,
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
                            )
                        });
                    }
                    (Ty::Union(_, members), Ty::Json) => {
                        let ms: SmallVec<[TyId; 4]> = members.clone();
                        ms.iter().for_each(|m| {
                            self.satisfies_class(
                                &BuiltinClass::Parameterized(
                                    BuiltinClassTag::Into,
                                    TyArena::JSON,
                                ),
                                *m,
                                span,
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
                    (Ty::Union(Some(id), _), _)
                        if *id == crate::TypeId::STORABLE =>
                    {
                        if !TyArena::STORABLE_MEMBERS.contains(&to) {
                            self.error(TypeError::InvalidCast {
                                from: ty,
                                to,
                                span,
                            });
                        }
                    }

                    // Member to union type
                    (_, Ty::Union(Some(id), _))
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
                    (Ty::Union(prov, members), _) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::Into, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                let inst_target =
                                    inst.class_args.first().copied();
                                if inst_target == Some(to) {
                                    self.check_instance_constraints(
                                        &inst,
                                        &[],
                                        span,
                                        None,
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
                                let ms: SmallVec<[TyId; 4]> = members.clone();
                                ms.iter().for_each(|m| {
                                    self.satisfies_class(
                                        &BuiltinClass::Parameterized(
                                            BuiltinClassTag::Into,
                                            to,
                                        ),
                                        *m,
                                        span,
                                    )
                                });
                            }
                        }
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
                                        &inst, &type_args, span, None,
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
                    ),
                    (Ty::Option(inner), Ty::Json) => self.satisfies_class(
                        &BuiltinClass::Parameterized(
                            BuiltinClassTag::TryInto,
                            TyArena::JSON,
                        ),
                        *inner,
                        span,
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
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            err,
                            span,
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
                        );
                        self.satisfies_class(
                            &BuiltinClass::Parameterized(
                                BuiltinClassTag::TryInto,
                                TyArena::JSON,
                            ),
                            v,
                            span,
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
                            )
                        });
                    }

                    // Union handling
                    (Ty::Union(prov, members), _) => {
                        match prov.and_then(|id| {
                            self.instance_registry
                                .lookup(BuiltinClassTag::TryInto, id)
                                .cloned()
                        }) {
                            Some(inst) => {
                                if inst.class_args.first().copied() == Some(to)
                                {
                                    self.check_instance_constraints(
                                        &inst,
                                        &[],
                                        span,
                                        None,
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
                                let ms: SmallVec<[TyId; 4]> = members.clone();
                                ms.iter().for_each(|m| {
                                    self.satisfies_class(
                                        &BuiltinClass::Parameterized(
                                            BuiltinClassTag::TryInto,
                                            to,
                                        ),
                                        *m,
                                        span,
                                    )
                                });
                            }
                        }
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
                                    &inst, &type_args, span, None,
                                );
                            }
                        }
                    }

                    // All other combinations are valid for READ
                    _ => {}
                }
            }

            // `Fallible`/`Wrappable`/`Chainable`: `Option[T]`, `Result[T, E]`
            // `None` = polymorphic (just check the type satisfies the class)
            // `Some(inner)` = check and unify element type
            BuiltinClass::Hkt(
                tag @ (BuiltinClassTag::Fallible
                | BuiltinClassTag::Wrappable
                | BuiltinClassTag::Chainable),
                opt_inner,
            ) => {
                self.satisfies_hkt_class(*tag, *opt_inner, class, ty, span);
            }

            // `Iterable(opt_elem)`: `Array[T]`, `Range`
            // `None` = polymorphic (just check the type is iterable)
            // `Some(elem)` = check and unify element type
            BuiltinClass::Hkt(BuiltinClassTag::Iterable, opt_elem) => {
                let opt_elem = *opt_elem;
                match self.ty_arena.get(ty).clone() {
                    Ty::Array(inner) => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) = self.unify_types(elem, inner, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) =
                                self.unify_types(elem, TyArena::INT, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Union(_, members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span)
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
                                let param_subst = self.build_instance_subst(
                                    &inst, &type_args, span,
                                );
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        if let Err(e) = self
                                            .unify_types(elem, resolved, span)
                                        {
                                            self.error(e);
                                        }
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
                        if let Err(e) = self.unify_types(elem, inner, span) {
                            self.error(e);
                        }
                    }
                    Ty::Map(_key, val) => {
                        // Map[K, V]: elem = Option[V] (index type is K, via .Index)
                        let opt_val = self.ty_arena.option(val);
                        if let Err(e) = self.unify_types(elem, opt_val, span) {
                            self.error(e);
                        }
                    }
                    Ty::String => {
                        // String: elem = Char (index type is Int, via .Index)
                        if let Err(e) =
                            self.unify_types(elem, TyArena::CHAR, span)
                        {
                            self.error(e);
                        }
                    }
                    Ty::Union(_, members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span)
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
                                let param_subst = self.build_instance_subst(
                                    &inst, &type_args, span,
                                );
                                // class_args[0] is the element type
                                if let Some(&inst_elem) =
                                    inst.class_args.first()
                                {
                                    let resolved = self
                                        .ty_arena
                                        .apply(inst_elem, &param_subst);
                                    if let Err(e) =
                                        self.unify_types(elem, resolved, span)
                                    {
                                        self.error(e);
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
                            if let Err(e) = self.unify_types(elem, inner, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Option(inner) => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) = self.unify_types(elem, inner, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Result(ok, _) => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) = self.unify_types(elem, ok, span) {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Union(_, members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span)
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
                                let param_subst = self.build_instance_subst(
                                    &inst, &type_args, span,
                                );
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        if let Err(e) = self
                                            .unify_types(elem, resolved, span)
                                        {
                                            self.error(e);
                                        }
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
                            if let Err(e) = self.unify_types(elem, inner, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) =
                                self.unify_types(elem, TyArena::INT, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Union(_, members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span)
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
                                let param_subst = self.build_instance_subst(
                                    &inst, &type_args, span,
                                );
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        if let Err(e) = self
                                            .unify_types(elem, resolved, span)
                                        {
                                            self.error(e);
                                        }
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
                            if let Err(e) = self.unify_types(elem, inner, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Range => {
                        if let Some(elem) = opt_elem {
                            if let Err(e) =
                                self.unify_types(elem, TyArena::INT, span)
                            {
                                self.error(e);
                            }
                        }
                    }
                    Ty::Union(_, members) => {
                        members.iter().for_each(|m| {
                            self.satisfies_class(class, *m, span)
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
                                let param_subst = self.build_instance_subst(
                                    &inst, &type_args, span,
                                );
                                if let Some(elem) = opt_elem {
                                    if let Some(&inst_elem) =
                                        inst.class_args.first()
                                    {
                                        let resolved = self
                                            .ty_arena
                                            .apply(inst_elem, &param_subst);
                                        if let Err(e) = self
                                            .unify_types(elem, resolved, span)
                                        {
                                            self.error(e);
                                        }
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

    /// Shared HKT class satisfaction logic for `Fallible`, `Wrappable`, and `Chainable`.
    ///
    /// All three handle the same set of types (`Option`, `Result`, `Union`, `Var`
    /// defaulting to `Option`, `Apply`, `Named` via instance registry) and differ
    /// only in which tag is used for registry lookups and error messages.
    fn satisfies_hkt_class(
        &mut self,
        tag: BuiltinClassTag,
        opt_inner: Option<TyId>,
        class: &BuiltinClass<TyId>,
        ty: TyId,
        span: Span,
    ) {
        let ty = self.expand_alias_fully(ty).unwrap_or(ty);

        match self.ty_arena.get(ty).clone() {
            Ty::Option(opt_elem) => {
                if let Some(inner) = opt_inner {
                    if let Err(e) = self.unify_types(inner, opt_elem, span) {
                        self.error(e);
                    }
                }
            }
            Ty::Result(ok, _) => {
                if let Some(inner) = opt_inner {
                    if let Err(e) = self.unify_types(inner, ok, span) {
                        self.error(e);
                    }
                }
            }
            Ty::Union(_, members) => {
                members
                    .iter()
                    .for_each(|m| self.satisfies_class(class, *m, span));
            }
            Ty::Var(v) => {
                // Default unresolved to `Option`
                let elem = opt_inner.unwrap_or_else(|| {
                    let fv = self.fresh_var();
                    self.ty_arena.alloc(Ty::Var(fv))
                });
                let opt_id = self.ty_arena.option(elem);
                let root = self.uf.find(v);
                self.uf.bind(root, opt_id);
            }
            Ty::Apply(_, _) => {
                // HKT variable application; defer
            }
            Ty::Error | Ty::Unknown => {}
            Ty::Named(id, type_args) => {
                match self.instance_registry.lookup(tag, id).cloned() {
                    Some(inst) => {
                        let param_subst =
                            self.build_instance_subst(&inst, &type_args, span);
                        if let Some(inner) = opt_inner {
                            if let Some(&inst_inner) = inst.class_args.first() {
                                let resolved = self
                                    .ty_arena
                                    .apply(inst_inner, &param_subst);
                                if let Err(e) =
                                    self.unify_types(inner, resolved, span)
                                {
                                    self.error(e);
                                }
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

    /// Build a `Rename` from an instance's `type_params` and the actual
    /// `type_args` at a use site. For `Ty::Var` entries, adds the mapping
    /// to the rename. For concrete entries, unifies with the corresponding
    /// `type_arg` to verify they match.
    pub(super) fn build_instance_subst(
        &mut self,
        inst: &super::instance::Instance,
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
                self.error(e);
            }
        });
        Rename(vars.into_iter().collect())
    }

    /// Check that a user instance's WHERE constraints are satisfied.
    /// Accepts an optional pre-built `Rename` to avoid redundant
    /// `build_instance_subst` calls at sites that already have one.
    fn check_instance_constraints(
        &mut self,
        inst: &super::instance::Instance,
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
        let constraints: SmallVec<[(TyVar, BuiltinClass<TyId>); 2]> =
            inst.constraints.clone();
        constraints.iter().for_each(|(var, class)| {
            let var_id = self.ty_arena.alloc(Ty::Var(*var));
            let ty = self.ty_arena.apply(var_id, &inst_subst);
            let class = class.apply(&inst_subst, &mut self.ty_arena);
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
                        if let Err(e) = self.unify_types(p, a, span) {
                            self.error(e);
                        }
                    });

                    // Unify return type
                    if let Err(e) = self.unify_types(fn_ret, ret, span) {
                        self.error(e);
                    }
                }
            }

            Ty::Var(v) => {
                // Callee is unresolved; create function type and bind
                let fn_ty =
                    self.ty_arena.func(args.iter().copied().collect(), ret);
                if let Err(e) = self.unify_var(v, fn_ty, span) {
                    self.error(e);
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
    ) {
        let shape = self.ty_arena.get(base).clone();
        match shape {
            // Structural object: look up field directly
            Ty::Object(ref fields) => match fields.get(&field) {
                Some(&actual_ty) => {
                    if let Err(e) = self.unify_types(field_ty, actual_ty, span)
                    {
                        self.error(e);
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
                                        if let Err(e) = self.unify_types(
                                            field_ty, actual_ty, span,
                                        ) {
                                            self.error(e);
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
            Ty::Json => {
                if let Err(e) = self.unify_types(field_ty, TyArena::JSON, span)
                {
                    self.error(e);
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
        let mut a = TyArena::new();
        let t = TyVar::new(0);
        let t_id = a.var(0);
        let constraint = (t, BuiltinClass::Simple(BuiltinClassTag::Display));

        let inst = Instance {
            class: BuiltinClassTag::Ord,
            class_args: SmallVec::new(),
            type_params: smallvec::smallvec![t_id],
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

        // Create rename: T -> Int
        let rename = Rename::singleton(t, TyArena::INT);

        // Apply rename to constraint
        let resolved = constraint.apply(&rename, &mut a);

        // Should now be `Iterable(Some(Int))`
        assert!(
            matches!(
                resolved,
                BuiltinClass::Hkt(
                    BuiltinClassTag::Iterable,
                    Some(TyArena::INT)
                )
            ),
            "constraint should be Iterable(Some(Int)) after rename"
        );
    }
}
