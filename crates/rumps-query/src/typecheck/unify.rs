//! Type unification and constraint solving.
//!
//! Implements the core unification algorithm for Hindley-Milner type inference.
//! Unification determines whether two types can be made equal, and if so,
//! produces a substitution mapping type variables to concrete types.

use std::collections::HashMap;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::error::TypeError;
use super::infer::{Constraint, InferCtx};
use super::ty::{Subst, Ty, TyVar};
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

    /// Core unification logic.
    fn unify_inner(&mut self, t1: &Ty, t2: &Ty, span: Span) -> UnifyResult {
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
            | (Ty::Json, Ty::Json) => UnifyResult::Ok(Subst::empty()),

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
                    UnifyResult::Ok(s) => Some(UnifyResult::Ok(s)),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    UnifyResult::Err(TypeError::Mismatch {
                        expected: t1.clone(),
                        got: t2.clone(),
                        span,
                    })
                }),

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

    /// Unify a named struct type with a structural object type.
    ///
    /// The struct must have all required fields present in the object.
    /// Extra fields in the object are allowed (extensible record semantics).
    fn unify_named_with_object(
        &mut self,
        type_id: crate::TypeId,
        type_args: &[Ty],
        obj_fields: &IndexMap<StringId, Ty>,
        span: Span,
    ) -> UnifyResult {
        // Look up struct definition
        let def = self.registry().get_def(type_id);

        match def {
            Some(TypeDef::Struct {
                type_params,
                fields: struct_fields,
                ..
            }) => {
                // Build substitution from type params to type args
                let param_subst: HashMap<StringId, Ty> = type_params
                    .iter()
                    .zip(type_args.iter())
                    .map(|(p, a)| (*p, a.clone()))
                    .collect();

                // Clone struct fields before mutable borrow of self
                let struct_fields = struct_fields.clone();

                // Check that object has all required struct fields
                struct_fields
                    .iter()
                    .try_fold(
                        Subst::empty(),
                        |acc, (field_name, field_ty_id)| {
                            let expected_ty =
                                self.ast_type_to_ty(*field_ty_id, &param_subst);
                            let expected_ty = expected_ty.apply(&acc);

                            match obj_fields.get(field_name) {
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
                                    let field_str = self
                                        .env()
                                        .get_str(*field_name)
                                        .unwrap_or("<unknown>")
                                        .to_string();
                                    Err(TypeError::MissingField {
                                        ty: type_id,
                                        field: field_str,
                                        span,
                                    })
                                }
                            }
                        },
                    )
                    .map_or_else(UnifyResult::Err, UnifyResult::Ok)
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
                | Constraint::Unwrappable { .. }
                | Constraint::HasField { .. }
                | Constraint::Iterable { .. } => {
                    // Already processed in first pass
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

                Constraint::Subscript(ty, span) => {
                    self.check_subscript(&ty.apply(&subst), *span);
                }

                Constraint::Storable(ty, span) => {
                    self.check_storable(&ty.apply(&subst), *span);
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
                self.error(TypeError::NotNumeric(ty.clone(), span));
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

            // Functions cannot be serialized to JSON
            Ty::Fn(_, _) => {
                self.error(TypeError::NotJsonable(ty.clone(), span));
            }

            // Deferred types
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}

            // Range and Time: technically not JSON-native but we allow conversion
            Ty::Range | Ty::Time => {}
        }
    }

    /// Check that a type can be used as a database subscript key.
    ///
    /// Valid types: `Bool`, `Int`, `Float`, `Char`, `String`, `Json`.
    fn check_subscript(&mut self, ty: &Ty, span: Span) {
        match ty {
            Ty::Bool
            | Ty::Int
            | Ty::Float
            | Ty::Char
            | Ty::String
            | Ty::Json => {}
            Ty::Var(_) | Ty::Unknown | Ty::Error => {}
            _ => {
                self.error(TypeError::NotSubscript(ty.clone(), span));
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
                self.error(TypeError::NotStorable(ty.clone(), span));
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

            Ty::Var(v) => {
                // Create Option[inner] and bind the variable
                let opt_ty = Ty::Option(Box::new(inner.clone()));
                match self.unify_var(*v, &opt_ty, span) {
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
                self.error(TypeError::NotUnwrappable(ty.clone(), span));
            }
        }
    }

    /// Check that a type has a specific field.
    ///
    /// Looks up the field in the resolved base type and unifies the expected
    /// field type with the actual field type. Unlike `unify_named_with_object`,
    /// this only checks the single accessed field, not all struct fields.
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

            // Named struct: look up field in struct definition
            Ty::Named(type_id, type_args) => {
                let def = self.registry().get_def(*type_id);
                match def {
                    Some(TypeDef::Struct {
                        type_params,
                        fields: struct_fields,
                        ..
                    }) => {
                        let params: SmallVec<[StringId; 2]> =
                            type_params.clone();
                        let struct_fields = struct_fields.clone();

                        match struct_fields.get(&field) {
                            Some(ast_ty_id) => {
                                let param_subst: HashMap<StringId, Ty> = params
                                    .iter()
                                    .zip(type_args.iter())
                                    .map(|(p, a)| (*p, a.clone()))
                                    .collect();
                                let actual_ty = self
                                    .ast_type_to_ty(*ast_ty_id, &param_subst);
                                match self
                                    .unify_types(field_ty, &actual_ty, span)
                                {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Ast;
    use crate::typecheck::TyVar;
    use crate::value::{TypeExprArena, TypeRegistry, ValueArena};

    /// Create an `InferCtx` for testing.
    fn test_ctx(ast: &Ast) -> InferCtx<'_> {
        let mut arena = ValueArena::new();
        let mut type_exprs = TypeExprArena::new();
        let registry = TypeRegistry::new(&mut arena, &mut type_exprs).unwrap();
        let strings = arena.interner();
        let registry = Box::leak(Box::new(registry));
        let env = Box::leak(Box::new(crate::env::Environment::new()));
        InferCtx::new(ast, registry, env, strings)
    }

    // --- Basic unification tests ---

    #[test]
    fn unify_same_primitive() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let result = ctx.unify_types(&Ty::Int, &Ty::Int, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));

        let result = ctx.unify_types(&Ty::Bool, &Ty::Bool, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));

        let result = ctx.unify_types(&Ty::String, &Ty::String, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_different_primitives_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let result = ctx.unify_types(&Ty::Bool, &Ty::String, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_numeric_coercion() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // Int ~ Float (widening)
        let result = ctx.unify_types(&Ty::Int, &Ty::Float, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));

        // Float ~ Int (symmetric)
        let result = ctx.unify_types(&Ty::Float, &Ty::Int, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_var_with_primitive() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let result = ctx.unify_types(&Ty::Var(v), &Ty::Int, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[test]
    fn unify_primitive_with_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let result = ctx.unify_types(&Ty::String, &Ty::Var(v), span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::String);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[test]
    fn unify_same_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let result = ctx.unify_types(&Ty::Var(v), &Ty::Var(v), span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_different_vars() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v1 = TyVar::new(0);
        let v2 = TyVar::new(1);

        let result = ctx.unify_types(&Ty::Var(v1), &Ty::Var(v2), span);
        match result {
            UnifyResult::Ok(s) => {
                // One var should map to the other
                let t1 = s.apply(&Ty::Var(v1));
                let t2 = s.apply(&Ty::Var(v2));
                assert_eq!(t1, t2);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[test]
    fn unify_occurs_check_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        // v ~ Array[v] would create infinite type
        let arr = Ty::Array(Box::new(Ty::Var(v)));
        let result = ctx.unify_types(&Ty::Var(v), &arr, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::InfiniteType(..))
        ))
    }

    // --- Array unification ---

    #[test]
    fn unify_array_same_elem() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let arr1 = Ty::Array(Box::new(Ty::Int));
        let arr2 = Ty::Array(Box::new(Ty::Int));
        let result = ctx.unify_types(&arr1, &arr2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_array_different_elem_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let arr1 = Ty::Array(Box::new(Ty::Int));
        let arr2 = Ty::Array(Box::new(Ty::String));
        let result = ctx.unify_types(&arr1, &arr2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_array_with_var_elem() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let arr1 = Ty::Array(Box::new(Ty::Var(v)));
        let arr2 = Ty::Array(Box::new(Ty::Int));
        let result = ctx.unify_types(&arr1, &arr2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    // --- Tuple unification ---

    #[test]
    fn unify_tuple_same_elems() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let t1 = Ty::Tuple(vec![Ty::Int, Ty::String]);
        let t2 = Ty::Tuple(vec![Ty::Int, Ty::String]);
        let result = ctx.unify_types(&t1, &t2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_tuple_different_length_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let t1 = Ty::Tuple(vec![Ty::Int]);
        let t2 = Ty::Tuple(vec![Ty::Int, Ty::String]);
        let result = ctx.unify_types(&t1, &t2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_tuple_with_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let t1 = Ty::Tuple(vec![Ty::Var(v), Ty::String]);
        let t2 = Ty::Tuple(vec![Ty::Int, Ty::String]);
        let result = ctx.unify_types(&t1, &t2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    // --- Function unification ---

    #[test]
    fn unify_fn_same_sig() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let f1 = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let f2 = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let result = ctx.unify_types(&f1, &f2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_fn_different_arity_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let f1 = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let f2 = Ty::Fn(vec![Ty::Int, Ty::Int], Box::new(Ty::Bool));
        let result = ctx.unify_types(&f1, &f2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::ArityMismatch { .. })
        ));
    }

    #[test]
    fn unify_fn_with_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let f1 = Ty::Fn(vec![Ty::Var(v)], Box::new(Ty::Bool));
        let f2 = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let result = ctx.unify_types(&f1, &f2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    // --- Option/Result unification ---

    #[test]
    fn unify_option_same() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let o1 = Ty::Option(Box::new(Ty::Int));
        let o2 = Ty::Option(Box::new(Ty::Int));
        let result = ctx.unify_types(&o1, &o2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_result_same() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let r1 = Ty::Result(Box::new(Ty::Int), Box::new(Ty::String));
        let r2 = Ty::Result(Box::new(Ty::Int), Box::new(Ty::String));
        let result = ctx.unify_types(&r1, &r2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_result_with_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let r1 = Ty::Result(Box::new(Ty::Var(v)), Box::new(Ty::String));
        let r2 = Ty::Result(Box::new(Ty::Int), Box::new(Ty::String));
        let result = ctx.unify_types(&r1, &r2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    // --- Map unification ---

    #[test]
    fn unify_map_same() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let m1 = Ty::Map(Box::new(Ty::String), Box::new(Ty::Int));
        let m2 = Ty::Map(Box::new(Ty::String), Box::new(Ty::Int));
        let result = ctx.unify_types(&m1, &m2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_map_different_key_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let m1 = Ty::Map(Box::new(Ty::String), Box::new(Ty::Int));
        let m2 = Ty::Map(Box::new(Ty::Int), Box::new(Ty::Int));
        let result = ctx.unify_types(&m1, &m2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    // --- Error/Unknown handling ---

    #[test]
    fn unify_error_with_anything() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let result = ctx.unify_types(&Ty::Error, &Ty::Int, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));

        let result = ctx.unify_types(&Ty::String, &Ty::Error, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_unknown_with_anything() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let result = ctx.unify_types(&Ty::Unknown, &Ty::Int, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));

        let result = ctx.unify_types(&Ty::String, &Ty::Unknown, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    // --- Object unification ---

    #[test]
    fn unify_object_same_fields() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let name_id = ctx.env_mut().intern("name");
        let age_id = ctx.env_mut().intern("age");

        let o1 = Ty::Object(
            [(name_id, Ty::String), (age_id, Ty::Int)]
                .into_iter()
                .collect(),
        );
        let o2 = Ty::Object(
            [(name_id, Ty::String), (age_id, Ty::Int)]
                .into_iter()
                .collect(),
        );

        let result = ctx.unify_types(&o1, &o2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_object_extensible_subset() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let name_id = ctx.env_mut().intern("name");
        let age_id = ctx.env_mut().intern("age");

        // Object with more fields unifies with object with fewer
        let o1 = Ty::Object(
            [(name_id, Ty::String), (age_id, Ty::Int)]
                .into_iter()
                .collect(),
        );
        let o2 = Ty::Object([(name_id, Ty::String)].into_iter().collect());

        let result = ctx.unify_types(&o1, &o2, span);
        assert!(matches!(result, UnifyResult::Ok(_)));
    }

    #[test]
    fn unify_object_field_type_mismatch() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let name_id = ctx.env_mut().intern("name");

        let o1 = Ty::Object([(name_id, Ty::String)].into_iter().collect());
        let o2 = Ty::Object([(name_id, Ty::Int)].into_iter().collect());

        let result = ctx.unify_types(&o1, &o2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_object_with_var_field() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        let name_id = ctx.env_mut().intern("name");

        let o1 = Ty::Object([(name_id, Ty::Var(v))].into_iter().collect());
        let o2 = Ty::Object([(name_id, Ty::String)].into_iter().collect());

        let result = ctx.unify_types(&o1, &o2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::String);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    // --- Union type unification ---

    #[test]
    fn unify_union_same() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let u1 = Ty::Union(vec![Ty::Int, Ty::String]);
        let u2 = Ty::Union(vec![Ty::Int, Ty::String]);
        let result = ctx.unify_types(&u1, &u2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_union_different_length_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let u1 = Ty::Union(vec![Ty::Int, Ty::String]);
        let u2 = Ty::Union(vec![Ty::Int]);
        let result = ctx.unify_types(&u1, &u2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    #[test]
    fn unify_union_different_order() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // Int | String should unify with String | Int
        let u1 = Ty::Union(vec![Ty::Int, Ty::String]);
        let u2 = Ty::Union(vec![Ty::String, Ty::Int]);
        let result = ctx.unify_types(&u1, &u2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_union_different_order_three_members() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // Int | String | Bool should unify with Bool | Int | String
        let u1 = Ty::Union(vec![Ty::Int, Ty::String, Ty::Bool]);
        let u2 = Ty::Union(vec![Ty::Bool, Ty::Int, Ty::String]);
        let result = ctx.unify_types(&u1, &u2, span);
        assert!(matches!(result, UnifyResult::Ok(s) if s.is_empty()));
    }

    #[test]
    fn unify_union_with_type_var() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        // Int | ?a should unify with String | Int by binding ?a = String
        let u1 = Ty::Union(vec![Ty::Int, Ty::Var(v)]);
        let u2 = Ty::Union(vec![Ty::String, Ty::Int]);
        let result = ctx.unify_types(&u1, &u2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::String);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[test]
    fn unify_union_incompatible_members() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // Int | String cannot unify with Bool | Char (no matching)
        let u1 = Ty::Union(vec![Ty::Int, Ty::String]);
        let u2 = Ty::Union(vec![Ty::Bool, Ty::Char]);
        let result = ctx.unify_types(&u1, &u2, span);
        assert!(matches!(
            result,
            UnifyResult::Err(TypeError::Mismatch { .. })
        ));
    }

    // --- Constraint solving tests ---

    #[test]
    fn solve_eq_constraint() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = ctx.fresh_var();

        ctx.constrain(Constraint::Eq(Ty::Var(v), Ty::Int, span));
        let subst = ctx.solve_constraints();

        assert_eq!(subst.apply(&Ty::Var(v)), Ty::Int);
        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_multiple_eq_constraints() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v1 = ctx.fresh_var();
        let v2 = ctx.fresh_var();

        ctx.constrain(Constraint::Eq(Ty::Var(v1), Ty::Int, span));
        ctx.constrain(Constraint::Eq(Ty::Var(v2), Ty::Var(v1), span));
        let subst = ctx.solve_constraints();

        assert_eq!(subst.apply(&Ty::Var(v1)), Ty::Int);
        assert_eq!(subst.apply(&Ty::Var(v2)), Ty::Int);
        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_conflicting_constraints_errors() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = ctx.fresh_var();

        ctx.constrain(Constraint::Eq(Ty::Var(v), Ty::Int, span));
        ctx.constrain(Constraint::Eq(Ty::Var(v), Ty::String, span));
        ctx.solve_constraints();

        assert!(ctx.has_errors());
    }

    #[test]
    fn solve_numeric_constraint_int() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Numeric(Ty::Int, span));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_numeric_constraint_float() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Numeric(Ty::Float, span));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_numeric_constraint_string_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Numeric(Ty::String, span));
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotNumeric(..)));
    }

    #[test]
    fn solve_callable_constraint() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let fn_ty = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let ret = ctx.fresh();

        ctx.constrain(Constraint::Callable {
            callee: fn_ty,
            args: smallvec::smallvec![Ty::Int],
            ret: ret.clone(),
            span,
        });
        let subst = ctx.solve_constraints();

        assert!(!ctx.has_errors());
        assert_eq!(subst.apply(&ret), Ty::Bool);
    }

    #[test]
    fn solve_callable_arity_mismatch() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let fn_ty = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        let ret = ctx.fresh();

        ctx.constrain(Constraint::Callable {
            callee: fn_ty,
            args: smallvec::smallvec![Ty::Int, Ty::Int],
            ret,
            span,
        });
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::ArityMismatch { .. }));
    }

    #[test]
    fn solve_callable_not_callable() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let ret = ctx.fresh();
        ctx.constrain(Constraint::Callable {
            callee: Ty::Int,
            args: smallvec::smallvec![Ty::Int],
            ret,
            span,
        });
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotCallable(..)));
    }

    #[test]
    fn solve_jsonable_primitive() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Jsonable(Ty::Int, span));
        ctx.constrain(Constraint::Jsonable(Ty::String, span));
        ctx.constrain(Constraint::Jsonable(Ty::Bool, span));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_jsonable_fn_fails() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let fn_ty = Ty::Fn(vec![Ty::Int], Box::new(Ty::Bool));
        ctx.constrain(Constraint::Jsonable(fn_ty, span));
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotJsonable(..)));
    }

    #[test]
    fn solve_subscript_valid() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Subscript(Ty::Int, span));
        ctx.constrain(Constraint::Subscript(Ty::String, span));
        ctx.constrain(Constraint::Subscript(Ty::Json, span));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_subscript_invalid() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Subscript(
            Ty::Array(Box::new(Ty::Int)),
            span,
        ));
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotSubscript(..)));
    }

    #[test]
    fn solve_storable_valid() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Storable(Ty::Int, span));
        ctx.constrain(Constraint::Storable(Ty::String, span));
        ctx.constrain(Constraint::Storable(Ty::Json, span));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    #[test]
    fn solve_storable_invalid() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        ctx.constrain(Constraint::Storable(Ty::Array(Box::new(Ty::Int)), span));
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotStorable(..)));
    }

    #[test]
    fn solve_unwrappable_option() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let inner = ctx.fresh();

        ctx.constrain(Constraint::Unwrappable {
            ty: Ty::Option(Box::new(Ty::Int)),
            inner: inner.clone(),
            span,
        });
        let subst = ctx.solve_constraints();

        assert!(!ctx.has_errors());
        assert_eq!(subst.apply(&inner), Ty::Int);
    }

    #[test]
    fn solve_unwrappable_result() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let inner = ctx.fresh();

        ctx.constrain(Constraint::Unwrappable {
            ty: Ty::Result(Box::new(Ty::String), Box::new(Ty::Int)),
            inner: inner.clone(),
            span,
        });
        let subst = ctx.solve_constraints();

        assert!(!ctx.has_errors());
        assert_eq!(subst.apply(&inner), Ty::String);
    }

    #[test]
    fn solve_unwrappable_invalid() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        let inner = ctx.fresh();
        ctx.constrain(Constraint::Unwrappable {
            ty: Ty::Int,
            inner,
            span,
        });
        ctx.solve_constraints();

        assert!(ctx.has_errors());
        assert!(matches!(ctx.errors()[0], TypeError::NotUnwrappable(..)));
    }

    #[test]
    fn solve_stringable_always_passes() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // All types are stringable
        ctx.constrain(Constraint::Stringable(Ty::Int, span));
        ctx.constrain(Constraint::Stringable(
            Ty::Fn(vec![], Box::new(Ty::Unit)),
            span,
        ));
        ctx.constrain(Constraint::Stringable(
            Ty::Array(Box::new(Ty::Bool)),
            span,
        ));
        ctx.solve_constraints();

        assert!(!ctx.has_errors());
    }

    // --- Transitive unification ---

    #[test]
    fn solve_transitive_vars() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // a ~ b, b ~ c, c ~ Int => a = b = c = Int
        let a = ctx.fresh_var();
        let b = ctx.fresh_var();
        let c = ctx.fresh_var();

        ctx.constrain(Constraint::Eq(Ty::Var(a), Ty::Var(b), span));
        ctx.constrain(Constraint::Eq(Ty::Var(b), Ty::Var(c), span));
        ctx.constrain(Constraint::Eq(Ty::Var(c), Ty::Int, span));
        let subst = ctx.solve_constraints();

        assert!(!ctx.has_errors());
        assert_eq!(subst.apply(&Ty::Var(a)), Ty::Int);
        assert_eq!(subst.apply(&Ty::Var(b)), Ty::Int);
        assert_eq!(subst.apply(&Ty::Var(c)), Ty::Int);
    }

    // --- Complex nested types ---

    #[test]
    fn unify_nested_array_option() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);
        let v = TyVar::new(0);

        // Array[Option[v]] ~ Array[Option[Int]]
        let t1 = Ty::Array(Box::new(Ty::Option(Box::new(Ty::Var(v)))));
        let t2 = Ty::Array(Box::new(Ty::Option(Box::new(Ty::Int))));

        let result = ctx.unify_types(&t1, &t2, span);
        match result {
            UnifyResult::Ok(s) => {
                assert_eq!(s.apply(&Ty::Var(v)), Ty::Int);
            }
            UnifyResult::Err(e) => panic!("expected Ok, got {e:?}"),
        }
    }

    #[test]
    fn solve_complex_function_call() {
        let ast = Ast::new();
        let mut ctx = test_ctx(&ast);
        let span = Span::new(0, 1);

        // Simulate: let id = x => x; id(42)
        // id has type: forall a. a -> a
        // id(42) should resolve to Int

        let a = ctx.fresh_var();
        let id_ty = Ty::Fn(vec![Ty::Var(a)], Box::new(Ty::Var(a)));
        let ret = ctx.fresh();

        ctx.constrain(Constraint::Callable {
            callee: id_ty,
            args: smallvec::smallvec![Ty::Int],
            ret: ret.clone(),
            span,
        });
        let subst = ctx.solve_constraints();

        assert!(!ctx.has_errors());
        assert_eq!(subst.apply(&ret), Ty::Int);
    }
}
