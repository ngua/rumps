//! Pattern matching and exhaustiveness checking.
//!
//! Contains methods for analyzing match patterns, extracting bindings,
//! and verifying exhaustiveness of match expressions.

use std::collections::HashSet;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::InferCtx;
use crate::ast::{Literal, MatchArm, MatchPattern, MatchPatternId};
use crate::intern::StringId;
use crate::typecheck::error::TypeError;
use crate::typecheck::ty::{Scheme, Ty};
use crate::value::{TypeDef, TypeId};
use crate::Span;

impl InferCtx<'_> {
    /// Get the payload types for a variant.
    ///
    /// Looks up the variant definition and extracts payload types.
    /// Uses the scrutinee type for type parameter substitution.
    pub(super) fn variant_payload_types(
        &mut self,
        ty_name: &str,
        var_name: &str,
        scrutinee_ty: &Ty,
        span: Span,
    ) -> Vec<Ty> {
        let var_name_id = self.env.intern(var_name);
        let lookup = self
            .env
            .lookup_str(ty_name)
            .and_then(|id| self.registry.lookup(id))
            .and_then(|type_id| {
                self.registry
                    .lookup_variant(type_id, var_name_id)
                    .map(|var_def| (type_id, var_def))
            });

        match lookup {
            None => {
                self.error(TypeError::UnknownType(
                    format!("{ty_name}.{var_name}"),
                    span,
                ));
                vec![]
            }
            Some((type_id, var_def)) => {
                // Special handling for Option/Result builtins
                if type_id == TypeId::OPTION {
                    if var_def.arity == 0 {
                        vec![]
                    } else if let Ty::Option(inner) = scrutinee_ty {
                        vec![inner.as_ref().clone()]
                    } else {
                        vec![self.fresh()]
                    }
                } else if type_id == TypeId::RESULT {
                    match (var_def.idx, scrutinee_ty) {
                        (0, Ty::Result(ok, _)) => vec![ok.as_ref().clone()],
                        (1, Ty::Result(_, err)) => vec![err.as_ref().clone()],
                        _ => vec![self.fresh()],
                    }
                } else if type_id == TypeId::ERROR {
                    // All Error variants have a String payload
                    vec![Ty::String]
                } else {
                    // User-defined sum types
                    let type_args: Vec<Ty> = match scrutinee_ty {
                        Ty::Named(_, args) => args.clone(),
                        Ty::Option(inner) => vec![inner.as_ref().clone()],
                        Ty::Result(ok, err) => {
                            vec![ok.as_ref().clone(), err.as_ref().clone()]
                        }
                        _ => vec![],
                    };

                    let type_params: SmallVec<[StringId; 2]> =
                        match self.registry.get_def(type_id) {
                            Some(TypeDef::Sum { type_params, .. }) => {
                                type_params.clone()
                            }
                            _ => SmallVec::new(),
                        };

                    let subst: IndexMap<StringId, Ty> = type_params
                        .iter()
                        .zip(type_args.iter())
                        .map(|(p, a)| (*p, a.clone()))
                        .collect();

                    var_def
                        .payloads
                        .iter()
                        .map(|ty_id| self.ast_type_to_ty(*ty_id, &subst))
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
        scrutinee_ty: &Ty,
        span: Span,
    ) {
        if let Some(pat) = self.ast.get_pattern(pat_id).cloned() {
            match &pat {
                MatchPattern::Wildcard => {}

                MatchPattern::Var(name) => {
                    self.env.bind(name, Scheme::mono(scrutinee_ty.clone()));
                }

                MatchPattern::Literal(lit) => {
                    let lit_ty = self.pattern_literal(lit, span);
                    self.unify(lit_ty, scrutinee_ty.clone(), span);
                }

                MatchPattern::Variant(ty_name, var_name, sub_pats) => {
                    // Check scrutinee is compatible with variant pattern
                    if !self.scrutinee_compatible_with_variant(
                        scrutinee_ty,
                        ty_name,
                    ) {
                        self.error(TypeError::IncompatibleVariantPattern {
                            pattern_ty: ty_name.clone(),
                            scrutinee_ty: scrutinee_ty.clone(),
                            span,
                        });
                    }

                    // Resolve type name using module-aware lookup; extract owned
                    // string only if it differs (avoids allocation in common case)
                    let qname = self
                        .resolve_type_name(ty_name)
                        .map(|(_, cow)| cow.into_owned())
                        .filter(|q| q != ty_name);

                    // Rewrite AST if name was resolved differently
                    if let Some(q) = &qname {
                        self.ast.set_pattern(
                            pat_id,
                            MatchPattern::Variant(
                                q.clone(),
                                var_name.clone(),
                                sub_pats.clone(),
                            ),
                        );
                    }

                    let payload_tys = self.variant_payload_types(
                        qname.as_deref().unwrap_or(ty_name),
                        var_name,
                        scrutinee_ty,
                        span,
                    );
                    sub_pats.iter().zip(payload_tys.iter()).for_each(
                        |(sub_pat_id, payload_ty)| {
                            self.pattern_bindings(
                                *sub_pat_id,
                                payload_ty,
                                span,
                            );
                        },
                    );
                }

                MatchPattern::Object(fields) => {
                    fields.iter().for_each(|(field_name, sub_pat_id)| {
                        let field_ty =
                            self.field_type(scrutinee_ty, field_name, span);
                        self.pattern_bindings(*sub_pat_id, &field_ty, span);
                    });
                }

                MatchPattern::Tuple(pats) => {
                    let elem_tys = match scrutinee_ty {
                        Ty::Tuple(ts) => ts.clone(),
                        Ty::Var(_) => {
                            let tys: Vec<Ty> =
                                (0..pats.len()).map(|_| self.fresh()).collect();
                            self.unify(
                                scrutinee_ty.clone(),
                                Ty::Tuple(tys.clone()),
                                span,
                            );
                            tys
                        }
                        _ => {
                            self.error(TypeError::NotATuple(
                                scrutinee_ty.clone(),
                                span,
                            ));
                            vec![Ty::Error; pats.len()]
                        }
                    };
                    pats.iter().zip(elem_tys.iter()).for_each(
                        |(pat_id, ty)| {
                            self.pattern_bindings(*pat_id, ty, span);
                        },
                    );
                }

                MatchPattern::Array(pats, rest) => {
                    // Extract element type from array type
                    let elem_ty = match scrutinee_ty {
                        Ty::Array(inner) => inner.as_ref().clone(),
                        Ty::Var(_) => {
                            let elem = self.fresh();
                            self.unify(
                                scrutinee_ty.clone(),
                                Ty::Array(Box::new(elem.clone())),
                                span,
                            );
                            elem
                        }
                        _ => {
                            self.error(TypeError::NotAnArray(
                                scrutinee_ty.clone(),
                                span,
                            ));
                            Ty::Error
                        }
                    };

                    // Bind prefix patterns
                    pats.iter().for_each(|pat_id| {
                        self.pattern_bindings(*pat_id, &elem_ty, span);
                    });

                    // Bind rest pattern if present
                    if let Some(crate::ast::RestPattern::Bind(name)) = rest {
                        // Rest has type `Array[T]` where `T` is the element type
                        self.env.bind(
                            name,
                            Scheme::mono(Ty::Array(Box::new(elem_ty.clone()))),
                        );
                    }
                }

                MatchPattern::Is(name, ty_id) => {
                    let narrowed_ty =
                        self.ast_type_to_ty(*ty_id, &IndexMap::new());

                    // Skip check if narrowed type is the union itself
                    let is_same_union = *scrutinee_ty == narrowed_ty;
                    if !is_same_union
                        && self.expand_union_members(scrutinee_ty).is_some()
                        && !self.is_union_member(scrutinee_ty, &narrowed_ty)
                    {
                        self.error(TypeError::NotAUnionMember {
                            member: narrowed_ty.clone(),
                            union_ty: scrutinee_ty.clone(),
                            span,
                        });
                    }

                    self.env.bind(name, Scheme::mono(narrowed_ty));
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
        scrutinee_ty: &Ty,
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
            match scrutinee_ty {
                Ty::Named(type_id, _) => {
                    if let Some(TypeDef::Sum { variants, .. }) =
                        self.registry.get_def(*type_id)
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
                                        ) => {
                                            let var_id =
                                                self.env.intern(var_name);
                                            self.registry
                                                .lookup_variant(
                                                    *type_id, var_id,
                                                )
                                                .map(|v| v.idx)
                                        }
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
                    let has_some = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Option" && var == "Some")
                        })
                    });
                    let has_none = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Option" && var == "None")
                        })
                    });
                    if !has_some || !has_none {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Result(_, _) => {
                    let has_ok = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Result" && var == "Ok")
                        })
                    });
                    let has_err = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Result" && var == "Err")
                        })
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
                    let has_lt = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Ordering" && var == "Lt")
                        })
                    });
                    let has_eq = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Ordering" && var == "Eq")
                        })
                    });
                    let has_gt = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Ordering" && var == "Gt")
                        })
                    });
                    if !has_lt || !has_eq || !has_gt {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::DataStatus => {
                    let has_no_data = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "DataStatus" && var == "NoData")
                        })
                    });
                    let has_value = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "DataStatus" && var == "HasValue")
                        })
                    });
                    let has_desc = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "DataStatus" && var == "HasDescendants")
                        })
                    });
                    let has_both = unguarded.iter().any(|arm| {
                        self.ast.get_pattern(arm.pattern).is_some_and(|p| {
                            matches!(p, MatchPattern::Variant(ty, var, _) if ty == "DataStatus" && var == "Both")
                        })
                    });
                    if !has_no_data || !has_value || !has_desc || !has_both {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::RuntimeError => {
                    let dominated =
                        ["Runtime", "Raise", "Type", "Coerce"].iter().all(|v| {
                            unguarded.iter().any(|arm| {
                                self.ast.get_pattern(arm.pattern).is_some_and(
                                    |p| matches!(p, MatchPattern::Variant(ty, var, _) if ty == "Error" && var == v),
                                )
                            })
                        });
                    if !dominated {
                        self.error(TypeError::NonExhaustiveMatch(span));
                    }
                }

                Ty::Union(members) => {
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

                    // Now convert each to Ty (requires mutable self)
                    let covered: Vec<Ty> = ty_ids
                        .into_iter()
                        .map(|ty_id| {
                            self.ast_type_to_ty(ty_id, &IndexMap::new())
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
            | MatchPattern::Is(..) => false,
        })
    }
}
