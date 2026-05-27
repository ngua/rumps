//! Pattern matching and exhaustiveness checking.
//!
//! Contains methods for analyzing match patterns, extracting bindings,
//! and verifying exhaustiveness of match expressions.

use std::collections::HashSet;

use indexmap::IndexMap;
use smallvec::{smallvec, SmallVec};

use super::InferCtx;
use crate::ast::{
    Literal, MatchArm, MatchPattern, MatchPatternId, RestPattern,
};
use crate::intern::{QualifiedName, StringId};
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty, TyArena, TyId};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Get the payload types for a variant.
    ///
    /// Looks up the variant definition and extracts payload types.
    /// Uses the scrutinee type for type parameter substitution.
    pub(super) fn variant_payload_types(
        &mut self,
        ty_name: &QualifiedName,
        var_name: StringId,
        scrutinee_ty: TyId,
        span: Span,
    ) -> SmallVec<[TyId; 4]> {
        let lookup = self.convert().resolve_type_name(ty_name).and_then(
            |(type_id, _)| {
                self.registry
                    .lookup_variant(type_id, var_name)
                    .map(|var_def| (type_id, var_def))
            },
        );

        match lookup {
            None => {
                let tn = ty_name.display(&self.env.strings);
                let vn = self.env.resolve_string(var_name);
                self.error(TypeError::UnknownType(format!("{tn}.{vn}"), span));
                SmallVec::new()
            }
            Some((type_id, var_def)) => {
                let resolved =
                    self.uf.resolve(scrutinee_ty, &mut self.ty_arena);
                let s_ty = self.ty_arena.get(resolved).clone();
                // Special handling for Option/Result builtins
                if type_id == TypeId::OPTION {
                    if var_def.arity == 0 {
                        SmallVec::new()
                    } else if let Ty::Option(inner) = s_ty {
                        smallvec![inner]
                    } else {
                        smallvec![self.fresh()]
                    }
                } else if type_id == TypeId::RESULT {
                    match (var_def.idx, &s_ty) {
                        (0, Ty::Result(ok, _)) => smallvec![*ok],
                        (1, Ty::Result(_, err)) => smallvec![*err],
                        _ => smallvec![self.fresh()],
                    }
                } else if type_id == TypeId::ERROR {
                    // All Error variants have a String payload
                    smallvec![TyArena::STRING]
                } else {
                    // User-defined sum types
                    let type_args: SmallVec<[TyId; 4]> = match &s_ty {
                        Ty::Named(_, args) => args.clone(),
                        Ty::Option(inner) => smallvec![*inner],
                        Ty::Result(ok, err) => smallvec![*ok, *err],
                        _ => SmallVec::new(),
                    };

                    let type_params: SmallVec<[StringId; 2]> =
                        match self.registry.get_def(type_id) {
                            Some(TypeDef::Sum { type_params, .. }) => {
                                type_params.clone()
                            }
                            _ => SmallVec::new(),
                        };

                    let subst: IndexMap<StringId, TyId> = type_params
                        .iter()
                        .zip(type_args.iter())
                        .map(|(p, &a)| (*p, a))
                        .collect();

                    let payloads = self
                        .decls
                        .variant_payloads(type_id, var_def.name)
                        .cloned()
                        .unwrap_or_default();

                    payloads
                        .iter()
                        .map(|ty_id| {
                            self.convert().ast_type_to_ty(*ty_id, &subst)
                        })
                        .collect()
                }
            }
        }
    }

    /// Extract bindings from a pattern and add them to the current scope.
    ///
    /// Also validates that the pattern is compatible with the scrutinee type.
    pub(super) fn pattern_bindings(
        &mut self,
        pat_id: MatchPatternId,
        scrutinee_ty: TyId,
        span: Span,
    ) {
        if let Some(pat) = self.ast.get_pattern(pat_id).cloned() {
            match &pat {
                MatchPattern::Wildcard => {}

                MatchPattern::Var(name) => {
                    self.env.bind(*name, Scheme::mono(scrutinee_ty));
                }

                MatchPattern::Literal(lit) => {
                    let lit_ty = self.pattern_literal(lit, span);
                    self.unify(lit_ty, scrutinee_ty, span);
                }

                MatchPattern::Variant(ty_name, var_name, sub_pats) => {
                    // Check scrutinee is compatible with variant pattern
                    if !self.scrutinee_compatible_with_variant(
                        scrutinee_ty,
                        ty_name,
                    ) {
                        let pat_ty = ty_name.display(&self.env.strings);
                        self.error(TypeError::IncompatibleVariantPattern {
                            pattern_ty: pat_ty,
                            scrutinee_ty,
                            span,
                        });
                    }

                    // Resolve type name using module-aware lookup
                    let qid = self
                        .convert()
                        .resolve_type_name(ty_name)
                        .map(|(_, qid)| qid)
                        .filter(|qid| *qid != *ty_name);

                    // Rewrite AST if name was resolved differently
                    if let Some(ref qid) = qid {
                        self.ast.set_pattern(
                            pat_id,
                            MatchPattern::Variant(
                                qid.clone(),
                                *var_name,
                                sub_pats.clone(),
                            ),
                        );
                    }

                    let eff = qid.unwrap_or(ty_name.clone());
                    let payload_tys = self.variant_payload_types(
                        &eff,
                        *var_name,
                        scrutinee_ty,
                        span,
                    );
                    sub_pats.iter().zip(payload_tys.iter()).for_each(
                        |(sub_pat_id, &payload_ty)| {
                            self.pattern_bindings(
                                *sub_pat_id,
                                payload_ty,
                                span,
                            );
                        },
                    );
                }

                MatchPattern::NakedVariant(var_name, sub_pats) => {
                    if let Some((_, qn)) =
                        self.resolve_naked_variant(*var_name, span)
                    {
                        self.ast.set_pattern(
                            pat_id,
                            MatchPattern::Variant(
                                qn.clone(),
                                *var_name,
                                sub_pats.clone(),
                            ),
                        );
                        self.pattern_bindings(pat_id, scrutinee_ty, span);
                    }
                }

                MatchPattern::Object(fields) => {
                    fields.iter().for_each(|(field_name, sub_pat_id)| {
                        let f = self.env.resolve_string(*field_name);
                        let field_ty = self.field_type(scrutinee_ty, &f, span);
                        self.pattern_bindings(*sub_pat_id, field_ty, span);
                    });
                }

                MatchPattern::Tuple(pats) => {
                    let s_ty = self.ty_arena.get(scrutinee_ty).clone();
                    let elem_tys: SmallVec<[TyId; 4]> = match s_ty {
                        Ty::Tuple(ts) => ts,
                        Ty::Var(_) => {
                            let tys: SmallVec<[TyId; 4]> =
                                (0..pats.len()).map(|_| self.fresh()).collect();
                            let tuple_ty =
                                self.ty_arena.alloc(Ty::Tuple(tys.clone()));
                            self.unify(scrutinee_ty, tuple_ty, span);
                            tys
                        }
                        _ => {
                            self.error(TypeError::NotATuple(
                                scrutinee_ty,
                                span,
                            ));
                            (0..pats.len()).map(|_| TyArena::ERROR).collect()
                        }
                    };
                    pats.iter().zip(elem_tys.iter()).for_each(
                        |(pat_id, &ty)| {
                            self.pattern_bindings(*pat_id, ty, span);
                        },
                    );
                }

                MatchPattern::Array(pats, rest) => {
                    // Extract element type from array type
                    let s_ty = self.ty_arena.get(scrutinee_ty).clone();
                    let elem_ty = match s_ty {
                        Ty::Array(inner) => inner,
                        Ty::Var(_) => {
                            let elem = self.fresh();
                            let arr_ty = self.ty_arena.array(elem);
                            self.unify(scrutinee_ty, arr_ty, span);
                            elem
                        }
                        _ => {
                            self.error(TypeError::NotAnArray(
                                scrutinee_ty,
                                span,
                            ));
                            TyArena::ERROR
                        }
                    };

                    // Bind prefix patterns
                    pats.iter().for_each(|pat_id| {
                        self.pattern_bindings(*pat_id, elem_ty, span);
                    });

                    // Bind rest pattern if present
                    if let Some(RestPattern::Bind(name)) = rest {
                        // Rest has type `Array[T]` where `T` is the element type
                        let rest_ty = self.ty_arena.array(elem_ty);
                        self.env.bind(*name, Scheme::mono(rest_ty));
                    }
                }

                MatchPattern::Is(name, ty_id) => {
                    let narrowed_ty =
                        self.convert().ast_type_to_ty(*ty_id, &IndexMap::new());
                    self.interp.match_targets.insert(pat_id, narrowed_ty);

                    // Function types cannot be inspected at runtime for
                    // opaque callables (class method refs, module fn refs,
                    // partial apps); reject them at any depth so the
                    // interpreter's `fn_value_matches` never has to answer
                    // this question dynamically. This covers the direct
                    // case and containers that smuggle one in (arrays,
                    // tuples, unions, objects, `Named` type args).
                    // On rejection, bind the name to `Error` to suppress
                    // cascading errors in the arm body.
                    if Self::type_contains_fn(narrowed_ty, &self.ty_arena) {
                        self.error(TypeError::FnTypeInPattern(span));
                        self.env.bind(*name, Scheme::mono(TyArena::ERROR));
                    } else {
                        // Skip check if narrowed type is the union itself
                        let is_same_union = scrutinee_ty == narrowed_ty;
                        if !is_same_union
                            && self.expand_union_members(scrutinee_ty).is_some()
                            && !self.is_union_member(scrutinee_ty, narrowed_ty)
                        {
                            self.error(TypeError::NotAUnionMember {
                                member: narrowed_ty,
                                union_ty: scrutinee_ty,
                                span,
                            });
                        }

                        self.env.bind(*name, Scheme::mono(narrowed_ty));
                    }
                }
            }
        }
    }

    /// Check exhaustiveness of match patterns.
    ///
    /// For sum types: all variants must be covered (or wildcard present).
    /// For literals: require wildcard/else arm.
    /// Patterns with guards do NOT count for coverage (guard might fail).
    /// Emits `TypeError::NonExhaustiveMatch` if not exhaustive.
    pub(super) fn check_exhaustiveness(
        &mut self,
        arms: &[MatchArm],
        scrutinee_ty: TyId,
        span: Span,
    ) {
        // Only unguarded patterns count for exhaustiveness
        let unguarded: Vec<_> =
            arms.iter().filter(|arm| arm.guard.is_none()).collect();

        // If any unguarded arm is irrefutable (catch-all), it's exhaustive
        let has_catch_all = unguarded
            .iter()
            .any(|arm| self.is_irrefutable_pattern(arm.pattern));

        if !has_catch_all {
            let resolved = self.uf.resolve(scrutinee_ty, &mut self.ty_arena);
            let s_ty = self.ty_arena.get(resolved).clone();
            match s_ty {
                Ty::Named(type_id, _) => {
                    if let Some(TypeDef::Sum { variants, .. }) =
                        self.registry.get_def(type_id)
                    {
                        let covered: HashSet<u8> = unguarded
                            .iter()
                            .filter_map(|arm| {
                                self.ast.get_pattern(arm.pattern).and_then(
                                    |p| match p {
                                        MatchPattern::Variant(
                                            _,
                                            var_name,
                                            _,
                                        ) => self
                                            .registry
                                            .lookup_variant(type_id, *var_name)
                                            .map(|v| v.idx),
                                        _ => None,
                                    },
                                )
                            })
                            .collect();

                        if !variants.iter().all(|v| covered.contains(&v.idx)) {
                            self.error(TypeError::NonExhaustiveMatch(span));
                        }
                    }
                }

                Ty::Option(_) => {
                    let oid = QualifiedName::local(self.env.intern("Option"));
                    let sid = self.env.intern("Some");
                    let nid = self.env.intern("None");
                    let has_some = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == oid && *var == sid))
                    });
                    let has_none = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == oid && *var == nid))
                    });
                    if !has_some || !has_none {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Result(_, _) => {
                    let rid = QualifiedName::local(self.env.intern("Result"));
                    let ok = self.env.intern("Ok");
                    let er = self.env.intern("Err");
                    let has_ok = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == rid && *var == ok))
                    });
                    let has_err = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == rid && *var == er))
                    });
                    if !has_ok || !has_err {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Bool => {
                    let has_true = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(
                                p,
                                MatchPattern::Literal(Literal::Bool(true))
                            )
                        })
                    });
                    let has_false = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(
                                p,
                                MatchPattern::Literal(Literal::Bool(false))
                            )
                        })
                    });
                    if !has_true || !has_false {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Ordering => {
                    let oid = QualifiedName::local(self.env.intern("Ordering"));
                    let lt = self.env.intern("Lt");
                    let eq = self.env.intern("Eq");
                    let gt = self.env.intern("Gt");
                    let has_lt = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == oid && *var == lt))
                    });
                    let has_eq = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == oid && *var == eq))
                    });
                    let has_gt = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == oid && *var == gt))
                    });
                    if !has_lt || !has_eq || !has_gt {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::DataStatus => {
                    let did =
                        QualifiedName::local(self.env.intern("DataStatus"));
                    let nd = self.env.intern("NoData");
                    let hv = self.env.intern("HasValue");
                    let hd = self.env.intern("HasDescendants");
                    let bt = self.env.intern("Both");
                    let has_no_data = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == did && *var == nd))
                    });
                    let has_value = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == did && *var == hv))
                    });
                    let has_desc = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == did && *var == hd))
                    });
                    let has_both = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == did && *var == bt))
                    });
                    if !has_no_data || !has_value || !has_desc || !has_both {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::RuntimeError => {
                    let eid = QualifiedName::local(self.env.intern("Error"));
                    let vars = [
                        self.env.intern("Runtime"),
                        self.env.intern("Raise"),
                        self.env.intern("Type"),
                        self.env.intern("Coerce"),
                    ];
                    let dominated = vars.iter().all(|&v| {
                        unguarded.iter().any(|arm| {
                            self.ast.get_pattern(arm.pattern).is_some_and(
                                |p| matches!(p, MatchPattern::Variant(ty, var, _) if *ty == eid && *var == v),
                            )
                        })
                    });
                    if !dominated {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Union(_, members) => {
                    // Collect type IDs from Is patterns first (to avoid borrow)
                    let ty_ids: Vec<_> = unguarded
                        .iter()
                        .filter_map(|arm| {
                            self.ast.get_pattern(arm.pattern).cloned()
                        })
                        .filter_map(|p| match p {
                            MatchPattern::Is(_, ty_id) => Some(ty_id),
                            _ => None,
                        })
                        .collect();

                    // Now convert each to `TyId` (requires mutable self)
                    let covered: SmallVec<[TyId; 4]> = ty_ids
                        .into_iter()
                        .map(|ty_id| {
                            self.convert()
                                .ast_type_to_ty(ty_id, &IndexMap::new())
                        })
                        .collect();

                    if !members.iter().all(|m| covered.contains(m)) {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                // For other types (Int, String, etc.), require wildcard
                _ => {
                    self.error(TypeError::NonExhaustiveMatch(span));
                }
            }
        }
    }

    /// Check if a pattern is irrefutable (always matches any value).
    ///
    /// Irrefutable patterns:
    /// - `_` (wildcard)
    /// - `x` (variable binding)
    /// - `(a, b, ...)` where all elements are irrefutable
    /// - `{ field1, field2, ... }` where all field patterns are irrefutable
    ///   (object patterns are partial; extra fields allowed)
    /// - `[..]` or `[...rest]` (array with rest and no prefix)
    pub(super) fn is_irrefutable_pattern(
        &self,
        pat_id: MatchPatternId,
    ) -> bool {
        self.ast.get_pattern(pat_id).is_some_and(|p| match p {
            MatchPattern::Wildcard | MatchPattern::Var(_) => true,
            MatchPattern::Tuple(elems) => {
                elems.iter().all(|e| self.is_irrefutable_pattern(*e))
            }
            MatchPattern::Object(fields) => {
                fields.iter().all(|(_, p)| self.is_irrefutable_pattern(*p))
            }
            // Array with rest and no prefix patterns is irrefutable (`[..]` or `[...rest]`)
            MatchPattern::Array(pats, rest) => {
                pats.is_empty() && rest.is_some()
            }
            // Literals, variants, and IS patterns are refutable
            MatchPattern::Literal(_)
            | MatchPattern::Variant(..)
            | MatchPattern::NakedVariant(..)
            | MatchPattern::Is(..) => false,
        })
    }
}
