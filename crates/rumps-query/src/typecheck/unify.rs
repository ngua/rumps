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
//! 2. Edge cases (numeric coercion, occurs check, union ordering) are implicitly
//!    tested through scripts that rely on correct unification
//! 3. Integration tests catch bugs that synthetic type construction misses
//!
//! See `infer.rs` for the full rationale on our testing approach.

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::error::{ConstraintKind, TypeError};
use super::infer::{Constraint, InferCtx};
use super::ty::{Subst, Ty, TyVar};
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
    /// 1. `Var(v) ~ t` → `{ v -> t }` (if `v` not in `fv(t)`; occurs check)
    /// 2. `t ~ Var(v)` → `{ v -> t }` (symmetric)
    /// 3. `Int ~ Float` or `Float ~ Int` → `{}` (numeric coercion)
    /// 4. `Array[a] ~ Array[b]` → `unify(a, b)` (recursive)
    /// 5. `Fn[p1] -> r1 ~ Fn[p2] -> r2` → `unify(p1, p2) . unify(r1, r2)`
    /// 6. `{ f1 } ~ { f2 }` → unify common fields (structural objects)
    /// 7. `Named(id, args1) ~ Named(id, args2)` → unify corresponding args
    /// 8. `Unknown ~ _` or `_ ~ Unknown` → `{}` (unifies with anything)
    /// 9. `Error ~ _` or `_ ~ Error` → `{}` (error recovery)
    /// 10. `T ~ T` → `{}` (primitives equal)
    /// 11. Otherwise → error
    ///
    /// # Note on Numeric Coercion
    ///
    /// Unlike typical HM unification, we allow `Int ~ Float` without error.
    /// This is because RUMPS uses automatic widening: `Int + Float = Float`.
    /// The coercion produces an empty substitution since no variables are bound.
    pub(crate) fn unify_types(
        &mut self,
        t1: &Ty,
        t2: &Ty,
        span: Span,
    ) -> UnifyResult {
        self.unify_inner(t1, t2, span)
    }

    /// Expand a `Ty::Named` alias fully to its target type.
    ///
    /// Recursively expands chained aliases (e.g., `A = B`, `B = Int`) until
    /// reaching a non-alias type. Object aliases are NOT expanded; they need
    /// special handling in `unify_named_with_object`.
    fn expand_alias_fully(&mut self, ty: &Ty) -> Option<Ty> {
        let mut current = ty.clone();
        let mut expanded = false;
        // Expand until we hit a non-alias or object alias
        while let Some(next) = self.expand_alias_once(&current) {
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
    fn expand_alias_once(&mut self, ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Named(type_id, args) => {
                match self.registry().get_def(*type_id) {
                    Some(TypeDef::Alias {
                        type_params,
                        target,
                        ..
                    }) => {
                        // Don't expand if target is an object type; let
                        // `unify_named_with_object` handle it for proper
                        // required-field checking
                        let is_obj =
                            self.ast().get_type_expr(*target).is_some_and(
                                |te| matches!(te, AstTypeExpr::Object(_)),
                            );
                        if is_obj {
                            None
                        } else {
                            let subst: HashMap<StringId, Ty> = type_params
                                .iter()
                                .zip(args.iter())
                                .map(|(p, a)| (*p, a.clone()))
                                .collect();
                            let target = *target;
                            Some(self.ast_type_to_ty(target, &subst))
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Core unification logic.
    fn unify_inner(&mut self, t1: &Ty, t2: &Ty, span: Span) -> UnifyResult {
        // Expand aliases fully before unifying (transparent type aliases)
        let exp1 = self.expand_alias_fully(t1);
        let exp2 = self.expand_alias_fully(t2);
        let t1 = exp1.as_ref().unwrap_or(t1);
        let t2 = exp2.as_ref().unwrap_or(t2);

        match (t1, t2) {
            // Error recovery: Error unifies with anything
            (Ty::Error, _) | (_, Ty::Error) => UnifyResult::Ok(Subst::empty()),

            // Unknown unifies with anything (database reads before narrowing)
            (Ty::Unknown, _) | (_, Ty::Unknown) => {
                UnifyResult::Ok(Subst::empty())
            }

            // Type variable on left: bind it
            (Ty::Var(v), t) => self.unify_var(*v, t, span),

            // Type variable on right: symmetric
            (t, Ty::Var(v)) => self.unify_var(*v, t, span),

            // Identical primitives
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

            // Numeric coercion: Int and Float unify (widening)
            (Ty::Int, Ty::Int) | (Ty::Float, Ty::Float) => {
                UnifyResult::Ok(Subst::empty())
            }
            (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int) => {
                UnifyResult::Ok(Subst::empty())
            }

            // Array: unify element types
            (Ty::Array(a), Ty::Array(b)) => self.unify_inner(a, b, span),

            // Range coerces to Array[Int] (for Array HOFs)
            (Ty::Range, Ty::Array(elem)) | (Ty::Array(elem), Ty::Range) => {
                self.unify_inner(elem, &Ty::Int, span)
            }

            // Option: unify inner types
            (Ty::Option(a), Ty::Option(b)) => self.unify_inner(a, b, span),

            // Result: unify both ok and err types
            (Ty::Result(ok1, err1), Ty::Result(ok2, err2)) => {
                match self.unify_inner(ok1, ok2, span) {
                    UnifyResult::Ok(s1) => {
                        let err1 = err1.apply(&s1);
                        let err2 = err2.apply(&s1);
                        match self.unify_inner(&err1, &err2, span) {
                            UnifyResult::Ok(s2) => {
                                UnifyResult::Ok(s1.compose(&s2))
                            }
                            err => err,
                        }
                    }
                    err => err,
                }
            }

            // Map: unify key and value types
            (Ty::Map(k1, v1), Ty::Map(k2, v2)) => {
                match self.unify_inner(k1, k2, span) {
                    UnifyResult::Ok(s1) => {
                        let v1 = v1.apply(&s1);
                        let v2 = v2.apply(&s1);
                        match self.unify_inner(&v1, &v2, span) {
                            UnifyResult::Ok(s2) => {
                                UnifyResult::Ok(s1.compose(&s2))
                            }
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
                        expected: t1.clone(),
                        got: t2.clone(),
                        span,
                    })
                } else {
                    self.unify_sequence(ts1.iter(), ts2.iter(), span)
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
                    match self.unify_sequence(
                        params1.iter(),
                        params2.iter(),
                        span,
                    ) {
                        UnifyResult::Ok(s) => {
                            let ret1 = ret1.apply(&s);
                            let ret2 = ret2.apply(&s);
                            match self.unify_inner(&ret1, &ret2, span) {
                                UnifyResult::Ok(s2) => {
                                    UnifyResult::Ok(s.compose(&s2))
                                }
                                err => err,
                            }
                        }
                        err => err,
                    }
                }
            }

            // Structural objects: unify common fields
            (Ty::Object(fields1), Ty::Object(fields2)) => {
                self.unify_objects(fields1, fields2, span)
            }

            // Named type with structural object (extensible record check)
            (Ty::Named(id, args), Ty::Object(obj_fields))
            | (Ty::Object(obj_fields), Ty::Named(id, args)) => {
                self.unify_named_with_object(*id, args, obj_fields, span)
            }

            // Named types: same TypeId, unify type arguments
            (Ty::Named(id1, args1), Ty::Named(id2, args2)) => {
                if id1 != id2 || args1.len() != args2.len() {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1.clone(),
                        got: t2.clone(),
                        span,
                    })
                } else {
                    self.unify_sequence(args1.iter(), args2.iter(), span)
                }
            }

            // Union types: structural equality (same members, order-independent)
            (Ty::Union(members1), Ty::Union(members2)) => {
                if members1.len() != members2.len() {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1.clone(),
                        got: t2.clone(),
                        span,
                    })
                } else {
                    // Find a bijective matching between union members
                    let available: Vec<usize> = (0..members2.len()).collect();
                    self.unify_union_bijection(
                        members1,
                        members2,
                        &available,
                        Subst::empty(),
                        span,
                    )
                    .unwrap_or_else(|| {
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: t1.clone(),
                            got: t2.clone(),
                            span,
                        })
                    })
                }
            }

            // Concrete type with union: T unifies if it matches any member
            (t, Ty::Union(members)) | (Ty::Union(members), t) => members
                .iter()
                .find_map(|m| match self.unify_inner(t, m, span) {
                    ok @ UnifyResult::Ok(_) => Some(ok),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1.clone(),
                        got: t2.clone(),
                        span,
                    })
                }),

            // Named union with concrete type: expand union and check membership
            (t, named @ Ty::Named(..)) | (named @ Ty::Named(..), t) => {
                self.expand_union_members(named).map_or_else(
                    || {
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: t1.clone(),
                            got: t2.clone(),
                            span,
                        })
                    },
                    |members| {
                        members
                            .iter()
                            .find_map(|m| match self.unify_inner(t, m, span) {
                                ok @ UnifyResult::Ok(_) => Some(ok),
                                _ => None,
                            })
                            .unwrap_or_else(|| {
                                UnifyResult::Err(TypeError::Mismatch {
                                    expected: t1.clone(),
                                    got: t2.clone(),
                                    span,
                                })
                            })
                    },
                )
            }

            // All other combinations are type mismatches
            _ => UnifyResult::Err(TypeError::Mismatch {
                expected: t1.clone(),
                got: t2.clone(),
                span,
            }),
        }
    }

    /// Unify a type variable with a type.
    ///
    /// Performs the occurs check to prevent infinite types like `a = Array[a]`.
    fn unify_var(&mut self, v: TyVar, t: &Ty, span: Span) -> UnifyResult {
        // If t is the same variable, nothing to do
        if *t == Ty::Var(v) {
            UnifyResult::Ok(Subst::empty())
        } else if t.occurs(v) {
            // Occurs check failed; would create infinite type
            UnifyResult::Err(TypeError::InfiniteType(v, t.clone(), span))
        } else {
            UnifyResult::Ok(Subst::singleton(v, t.clone()))
        }
    }

    /// Unify two sequences of types element-wise.
    fn unify_sequence<'b>(
        &mut self,
        ts1: impl Iterator<Item = &'b Ty>,
        ts2: impl Iterator<Item = &'b Ty>,
        span: Span,
    ) -> UnifyResult {
        ts1.zip(ts2)
            .try_fold(Subst::empty(), |acc, (t1, t2)| {
                let t1 = t1.apply(&acc);
                let t2 = t2.apply(&acc);
                match self.unify_inner(&t1, &t2, span) {
                    UnifyResult::Ok(s) => Ok(acc.compose(&s)),
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
        remaining1: &[Ty],
        all2: &[Ty],
        available: &[usize],
        acc: Subst,
        span: Span,
    ) -> Option<UnifyResult> {
        match remaining1.split_first() {
            None => Some(UnifyResult::Ok(acc)),
            Some((first, rest)) => {
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
        first: &Ty,
        rest: &[Ty],
        all2: &[Ty],
        available: &[usize],
        acc: Subst,
        span: Span,
        start_idx: usize,
    ) -> Option<UnifyResult> {
        available.get(start_idx).and_then(|&idx| {
            let m2 = all2.get(idx)?;
            let first_applied = first.apply(&acc);
            let m2_applied = m2.apply(&acc);

            match self.unify_inner(&first_applied, &m2_applied, span) {
                UnifyResult::Ok(s) => {
                    let new_acc = acc.compose(&s);
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
        fields1: &IndexMap<StringId, Ty>,
        fields2: &IndexMap<StringId, Ty>,
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
                    (Some(t1), Some(t2)) => {
                        let t1 = t1.apply(&acc);
                        let t2 = t2.apply(&acc);
                        match self.unify_inner(&t1, &t2, span) {
                            UnifyResult::Ok(s) => Ok(acc.compose(&s)),
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
        type_args: &[Ty],
        obj_fields: &IndexMap<StringId, Ty>,
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
                        let param_subst: HashMap<StringId, Ty> = type_params
                            .iter()
                            .zip(type_args.iter())
                            .map(|(p, a)| (*p, a.clone()))
                            .collect();

                        // Pre-intern field names before the fold
                        let fields_with_ids: Vec<_> = alias_fields
                            .iter()
                            .map(|(name, ty)| {
                                let id = self.env_mut().intern(name);
                                (name.clone(), id, *ty)
                            })
                            .collect();

                        // Check that object has all required fields
                        fields_with_ids
                            .iter()
                            .try_fold(
                                Subst::empty(),
                                |acc, (field_name, field_name_id, field_ty_id)| {
                                    let expected_ty = self.ast_type_to_ty(
                                        *field_ty_id,
                                        &param_subst,
                                    );
                                    let expected_ty = expected_ty.apply(&acc);

                                    match obj_fields.get(field_name_id) {
                                        Some(obj_ty) => {
                                            let obj_ty = obj_ty.apply(&acc);
                                            match self.unify_inner(
                                                &expected_ty,
                                                &obj_ty,
                                                span,
                                            ) {
                                                UnifyResult::Ok(s) => {
                                                    Ok(acc.compose(&s))
                                                }
                                                UnifyResult::Err(e) => Err(e),
                                            }
                                        }
                                        None => {
                                            // Missing required field
                                            Err(TypeError::MissingField {
                                                ty: type_id,
                                                field: field_name.clone(),
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
                        UnifyResult::Err(TypeError::Mismatch {
                            expected: Ty::Named(type_id, type_args.to_vec()),
                            got: Ty::Object(obj_fields.clone()),
                            span,
                        })
                    }
                }
            }

            Some(TypeDef::Union { .. }) => {
                // Union types don't unify with structural objects directly
                UnifyResult::Err(TypeError::Mismatch {
                    expected: Ty::Named(type_id, type_args.to_vec()),
                    got: Ty::Object(obj_fields.clone()),
                    span,
                })
            }

            Some(TypeDef::Sum { .. }) => {
                // Sum types don't unify with structural objects
                UnifyResult::Err(TypeError::Mismatch {
                    expected: Ty::Named(type_id, type_args.to_vec()),
                    got: Ty::Object(obj_fields.clone()),
                    span,
                })
            }

            Some(TypeDef::Builtin(_)) | None => {
                // Builtin types don't unify with structural objects
                UnifyResult::Err(TypeError::Mismatch {
                    expected: Ty::Named(type_id, type_args.to_vec()),
                    got: Ty::Object(obj_fields.clone()),
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
    /// 4. `Stringable` constraints (always satisfied; all types stringify)
    /// 5. `Jsonable` constraints (rejects `Fn` types)
    /// 6. `Subscript` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 7. `Storable` constraints (must be `Bool | Int | Float | Char | String | Json`)
    /// 8. `Unwrappable` constraints (must be `Option[T]` or `Result[T, E]`)
    ///
    /// Errors are recorded via `self.error()`; unification continues to collect
    /// as many errors as possible.
    pub(crate) fn solve_constraints(&mut self) -> Subst {
        let constraints = self.take_constraints();
        let mut subst = Subst::empty();

        // First pass: process Eq, Unwrappable, and HasField constraints to
        // build substitution. Unwrappable and HasField must be processed early
        // so type variables get resolved before they're used in other constraints.
        constraints.iter().for_each(|c| match c {
            Constraint::Eq(t1, t2, span) => {
                let t1 = t1.apply(&subst);
                let t2 = t2.apply(&subst);
                match self.unify_types(&t1, &t2, *span) {
                    UnifyResult::Ok(s) => {
                        subst = subst.compose(&s);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }
            Constraint::Unwrappable { ty, inner, span } => {
                let ty = ty.apply(&subst);
                let inner = inner.apply(&subst);
                self.check_unwrappable(&ty, &inner, *span, &mut subst);
            }
            Constraint::HasField {
                base,
                field,
                field_ty,
                span,
            } => {
                let base = base.apply(&subst);
                let field_ty = field_ty.apply(&subst);
                self.check_has_field(
                    &base, *field, &field_ty, *span, &mut subst,
                );
            }
            Constraint::Iterable { coll, elem, span } => {
                let coll = coll.apply(&subst);
                let elem = elem.apply(&subst);
                self.check_iterable(&coll, &elem, *span, &mut subst);
            }
            _ => {}
        });

        // Second pass: process all other constraints with final substitution
        constraints.iter().for_each(|c| {
            match c {
                Constraint::Eq(..)
                | Constraint::HasField { .. }
                | Constraint::Iterable { .. } => {
                    // Already processed in first pass
                }

                // Re-check Unwrappable now that Callable has resolved types
                Constraint::Unwrappable { ty, inner, span } => {
                    let ty = ty.apply(&subst);
                    let inner = inner.apply(&subst);
                    self.check_unwrappable(&ty, &inner, *span, &mut subst);
                }

                Constraint::Numeric(ty, span) => {
                    self.check_numeric(&ty.apply(&subst), *span, &mut subst);
                }

                Constraint::Callable {
                    callee,
                    args,
                    ret,
                    span,
                } => {
                    let callee = callee.apply(&subst);
                    let args: Vec<Ty> =
                        args.iter().map(|t| t.apply(&subst)).collect();
                    let ret = ret.apply(&subst);
                    self.check_callable(
                        &callee, &args, &ret, *span, &mut subst,
                    );
                }

                Constraint::Stringable(_, _) => {
                    // All types can be stringified; no check needed
                }

                Constraint::Jsonable(ty, span) => {
                    self.check_jsonable(&ty.apply(&subst), *span);
                }

                Constraint::Subscriptable(ty, span) => {
                    self.check_subscriptable(&ty.apply(&subst), *span);
                }

                Constraint::Storable(ty, span) => {
                    self.check_storable(&ty.apply(&subst), *span);
                }

                Constraint::Monoid(ty, span) => {
                    self.check_monoid(&ty.apply(&subst), *span);
                }
            }
        });

        subst
    }

    /// Check that a type is numeric (`Int` or `Float`).
    ///
    /// If the type is an unresolved type variable, defaults it to `Int` (like
    /// Haskell's defaulting rules). This enables inference for expressions
    /// like `x => x + 1` when passed to HOFs with polymorphic empty arrays.
    fn check_numeric(&mut self, ty: &Ty, span: Span, subst: &mut Subst) {
        match ty {
            Ty::Int | Ty::Float => {}
            Ty::Var(v) => {
                // Default unresolved numeric type variables to Int
                *subst = subst.compose(&Subst::singleton(*v, Ty::Int));
            }
            Ty::Error | Ty::Unknown => {}
            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Numeric,
                    ty.clone(),
                    span,
                ));
            }
        }
    }

    /// Check that a callee type is callable and unify with expected signature.
    fn check_callable(
        &mut self,
        callee: &Ty,
        args: &[Ty],
        ret: &Ty,
        span: Span,
        subst: &mut Subst,
    ) {
        match callee {
            Ty::Fn(params, fn_ret) => {
                if params.len() != args.len() {
                    self.error(TypeError::ArityMismatch {
                        expected: params.len(),
                        got: args.len(),
                        span,
                    });
                } else {
                    // Unify each parameter with corresponding argument
                    params.iter().zip(args.iter()).for_each(|(p, a)| {
                        let p = p.apply(subst);
                        let a = a.apply(subst);
                        match self.unify_types(&p, &a, span) {
                            UnifyResult::Ok(s) => {
                                *subst = subst.compose(&s);
                            }
                            UnifyResult::Err(e) => {
                                self.error(e);
                            }
                        }
                    });

                    // Unify return type
                    let fn_ret = fn_ret.apply(subst);
                    let ret = ret.apply(subst);
                    match self.unify_types(&fn_ret, &ret, span) {
                        UnifyResult::Ok(s) => {
                            *subst = subst.compose(&s);
                        }
                        UnifyResult::Err(e) => {
                            self.error(e);
                        }
                    }
                }
            }

            Ty::Var(v) => {
                // Callee is unresolved; create function type and bind
                let fn_ty = Ty::Fn(args.to_vec(), Box::new(ret.clone()));
                match self.unify_var(*v, &fn_ty, span) {
                    UnifyResult::Ok(s) => {
                        *subst = subst.compose(&s);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::NotCallable(callee.clone(), span));
            }
        }
    }

    /// Check that a type can be converted to JSON.
    ///
    /// Rejects function types (closures, named functions, module functions).
    fn check_jsonable(&mut self, ty: &Ty, span: Span) {
        match ty {
            // Primitives are JSON-serializable
            Ty::Bool
            | Ty::Int
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Unit
            | Ty::Json => {}

            // Compound types: recursively check
            Ty::Array(elem) => self.check_jsonable(elem, span),
            Ty::Option(inner) => self.check_jsonable(inner, span),
            Ty::Result(ok, err) => {
                self.check_jsonable(ok, span);
                self.check_jsonable(err, span);
            }
            Ty::Map(k, v) => {
                self.check_jsonable(k, span);
                self.check_jsonable(v, span);
            }
            Ty::Tuple(elems) => {
                elems.iter().for_each(|e| self.check_jsonable(e, span));
            }
            Ty::Object(fields) => {
                fields.values().for_each(|t| self.check_jsonable(t, span));
            }
            Ty::Union(members) => {
                members.iter().for_each(|m| self.check_jsonable(m, span));
            }
            Ty::Named(_, args) => {
                args.iter().for_each(|a| self.check_jsonable(a, span));
            }

            // Functions and regex cannot be serialized to JSON
            Ty::Fn(_, _) | Ty::Regex => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Jsonable,
                    ty.clone(),
                    span,
                ));
            }

            // Deferred types
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}

            // Range, Time, Ordering, DataStatus, FilePath, Path, RuntimeError: technically not JSON-native but we allow conversion
            Ty::Range
            | Ty::Time
            | Ty::Ordering
            | Ty::DataStatus
            | Ty::FilePath
            | Ty::Path
            | Ty::RuntimeError => {}
        }
    }

    /// Check that a type can be used as a database subscript key.
    ///
    /// Valid types: `Bool`, `Int`, `Float`, `Char`, `String`, `Json`, or the
    /// `Subscript` union itself.
    fn check_subscriptable(&mut self, ty: &Ty, span: Span) {
        match ty {
            Ty::Bool
            | Ty::Int
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Json => {}
            // Allow the `Subscript` union type itself
            Ty::Named(id, _) if *id == crate::TypeId::SUBSCRIPT => {}
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}
            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Subscriptable,
                    ty.clone(),
                    span,
                ));
            }
        }
    }

    /// Check that a type can be stored in the database.
    ///
    /// Valid types: members of the `Storable` union.
    fn check_storable(&mut self, ty: &Ty, span: Span) {
        match ty {
            Ty::Bool
            | Ty::Int
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Json => {}
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}
            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Storable,
                    ty.clone(),
                    span,
                ));
            }
        }
    }

    /// Check that a type is monoidal (supports `++` concatenation).
    ///
    /// Valid monoidal types are `String`, `Array[T]`, and `Map[K, V]`.
    /// Unresolved type variables are left polymorphic (no defaulting).
    fn check_monoid(&mut self, ty: &Ty, span: Span) {
        match ty {
            Ty::String | Ty::Array(_) | Ty::Map(_, _) => {}
            // Type variables remain polymorphic; caller provides concrete type
            Ty::Var(_) | Ty::Error | Ty::Unknown => {}
            _ => {
                self.error(TypeError::UnsatisfiedConstraint(
                    ConstraintKind::Monoid,
                    ty.clone(),
                    span,
                ));
            }
        }
    }

    /// Check that a type is unwrappable (`Option[T]` or `Result[T, E]`).
    ///
    /// Unifies the `inner` type variable with the extracted inner type.
    fn check_unwrappable(
        &mut self,
        ty: &Ty,
        inner: &Ty,
        span: Span,
        subst: &mut Subst,
    ) {
        // Expand aliases (e.g., `Maybe[T]` -> `Option[T]`)
        let expanded = self.expand_alias_fully(ty);
        let ty = expanded.as_ref().unwrap_or(ty);

        match ty {
            Ty::Option(opt_inner) => {
                match self.unify_types(inner, opt_inner, span) {
                    UnifyResult::Ok(s) => {
                        *subst = subst.compose(&s);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }

            Ty::Result(ok, _) => match self.unify_types(inner, ok, span) {
                UnifyResult::Ok(s) => {
                    *subst = subst.compose(&s);
                }
                UnifyResult::Err(e) => {
                    self.error(e);
                }
            },

            // Type variable: defer until resolved. The Callable constraint for
            // the expression that produces this value will eventually bind it.
            Ty::Var(_) => {}

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::NotUnwrappable(ty.clone(), span));
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
        base: &Ty,
        field: StringId,
        field_ty: &Ty,
        span: Span,
        subst: &mut Subst,
    ) {
        match base {
            // Structural object: look up field directly
            Ty::Object(fields) => match fields.get(&field) {
                Some(actual_ty) => {
                    match self.unify_types(field_ty, actual_ty, span) {
                        UnifyResult::Ok(s) => {
                            *subst = subst.compose(&s);
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
                        ty: base.clone(),
                        field: name,
                        span,
                    });
                }
            },

            // Named alias to object: look up field in alias definition
            Ty::Named(type_id, type_args) => {
                let def = self.registry().get_def(*type_id);
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
                                    .find(|(n, _)| n == field_str)
                                    .map(|(_, ty)| *ty);
                                match field_ty_id {
                                    Some(ast_ty_id) => {
                                        let param_subst: HashMap<_, _> = params
                                            .iter()
                                            .zip(type_args.iter())
                                            .map(|(p, a)| (*p, a.clone()))
                                            .collect();
                                        let actual_ty = self.ast_type_to_ty(
                                            ast_ty_id,
                                            &param_subst,
                                        );
                                        match self.unify_types(
                                            field_ty, &actual_ty, span,
                                        ) {
                                            UnifyResult::Ok(s) => {
                                                *subst = subst.compose(&s);
                                            }
                                            UnifyResult::Err(e) => {
                                                self.error(e);
                                            }
                                        }
                                    }
                                    None => {
                                        self.error(TypeError::FieldNotFound {
                                            ty: base.clone(),
                                            field: field_str.to_string(),
                                            span,
                                        });
                                    }
                                }
                            }
                            _ => {
                                self.error(TypeError::NotAnObject(
                                    base.clone(),
                                    span,
                                ));
                            }
                        }
                    }
                    _ => {
                        self.error(TypeError::NotAnObject(base.clone(), span));
                    }
                }
            }

            // Json: any field access is valid and returns Json
            Ty::Json => match self.unify_types(field_ty, &Ty::Json, span) {
                UnifyResult::Ok(s) => {
                    *subst = subst.compose(&s);
                }
                UnifyResult::Err(e) => {
                    self.error(e);
                }
            },

            // Type variable: defer until resolved
            Ty::Var(_) => {
                // Type variable not yet resolved; constraint will be checked
                // when the variable is bound. For now, this is allowed.
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::NotAnObject(base.clone(), span));
            }
        }
    }

    /// Check that a type is iterable and unify the element type.
    ///
    /// Iterable types are `Array[T]` (element type T) and `Range` (element
    /// type `Int`). Used by Array HOFs like `map`, `filter`, `foreach`.
    fn check_iterable(
        &mut self,
        coll: &Ty,
        elem: &Ty,
        span: Span,
        subst: &mut Subst,
    ) {
        match coll {
            Ty::Array(inner) => match self.unify_types(elem, inner, span) {
                UnifyResult::Ok(s) => {
                    *subst = subst.compose(&s);
                }
                UnifyResult::Err(e) => {
                    self.error(e);
                }
            },

            Ty::Range => {
                // Range iterates over Int
                match self.unify_types(elem, &Ty::Int, span) {
                    UnifyResult::Ok(s) => {
                        *subst = subst.compose(&s);
                    }
                    UnifyResult::Err(e) => {
                        self.error(e);
                    }
                }
            }

            Ty::Var(_) => {
                // Not yet resolved; defer
            }

            Ty::Error | Ty::Unknown => {}

            _ => {
                self.error(TypeError::Mismatch {
                    expected: Ty::Array(Box::new(elem.clone())),
                    got: coll.clone(),
                    span,
                });
            }
        }
    }
}
