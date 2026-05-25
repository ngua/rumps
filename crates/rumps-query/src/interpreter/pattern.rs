//! Pattern matching and destructuring.

use std::sync::Arc;

use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{
    AstTypeExprId, BindingPattern, ExprId, MatchPattern, MatchPatternId,
    RestPattern, TypePattern,
};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::typecheck::{RuntimeTyId, Ty};
use crate::value::{Payload, TypeDef, TypeId, ValueId, ValueMeta, VariantDef};
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// No-op after Phase 7: `Payload::Union`/`Payload::Newtype` are removed.
    ///
    /// Retained temporarily for callers outside this module; always returns
    /// `None` (the value is never wrapped).
    pub(super) fn unwrap_value_recursive(
        &self,
        _val: &Payload,
    ) -> Option<Payload> {
        None
    }

    /// Check if a value matches a type pattern (without binding).
    ///
    /// `checked_ty` is the statically-known type of `val` from the typechecker,
    /// used when the payload alone cannot determine its parameterized type.
    pub(super) fn check_pattern(
        &mut self,
        val: &Payload,
        checked_ty: Option<RuntimeTyId>,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<bool> {
        match pattern {
            TypePattern::Type(ast_ty_id) => {
                match self.checked.ast_type_map.get(ast_ty_id).copied() {
                    Some(expected)
                        if !self.checked.types.contains_var(expected) =>
                    {
                        let payload_ty = self.payload_runtime_ty(val);
                        let actual = if payload_ty == RuntimeTyId::UNKNOWN {
                            checked_ty.unwrap_or(payload_ty)
                        } else {
                            payload_ty
                        };
                        let matched = self
                            .checked
                            .types
                            .matches(actual, actual, expected);
                        Ok(matched
                            || self.alias_structurally_matches(
                                val, expected, *ast_ty_id, span,
                            ))
                    }
                    Some(expected) => {
                        let payload_ty = self.payload_runtime_ty(val);
                        let actual = if payload_ty == RuntimeTyId::UNKNOWN {
                            checked_ty.unwrap_or(payload_ty)
                        } else {
                            payload_ty
                        };
                        Ok(self.checked.types.matches(actual, actual, expected))
                    }
                    None => {
                        typechecked!("type pattern", "in ast_type_map")
                    }
                }
            }
            TypePattern::Variant(ref ty_name, var_name) => {
                self.check_variant_zero_arity(val, ty_name, *var_name, span)
            }
            TypePattern::VariantWildcard(ref ty_name, var_name) => {
                self.check_variant(val, ty_name, *var_name, span)
            }
            TypePattern::VariantBind(ref ty_name, var_name, _) => {
                self.check_variant(val, ty_name, *var_name, span)
            }
            TypePattern::Object(fields) => {
                // Structural object check: `is { name: String, age: Int }`
                match val {
                    Payload::Object(obj) => {
                        let obj = obj.clone();
                        fields.iter().try_fold(true, |acc, (name, ty_id)| {
                            let expected = self
                                .checked
                                .ast_type_map
                                .get(ty_id)
                                .copied()
                                .unwrap_or_else(|| {
                                    typechecked!(
                                        "object field type",
                                        "in ast_type_map"
                                    )
                                });
                            let matches = obj.get(name).is_some_and(|&vid| {
                                self.value_id_matches_type(vid, expected)
                            });
                            Ok(acc && matches)
                        })
                    }
                    _ => Ok(false),
                }
            }
        }
    }

    /// Check if a runtime value matches an expected type, handling nested
    /// objects recursively. For primitives, uses `payload_runtime_ty` + `matches`.
    /// For objects, recurses into fields structurally.
    fn value_matches_type(&self, val: &Payload, expected: RuntimeTyId) -> bool {
        match self.checked.types.get(expected) {
            Ty::Object(fields) => match val {
                Payload::Object(obj) => fields.iter().all(|(name, &ty)| {
                    obj.get(name).is_some_and(|&vid| {
                        self.value_id_matches_type(vid, RuntimeTyId::from(ty))
                    })
                }),
                _ => false,
            },
            _ => {
                let actual = self.payload_runtime_ty(val);
                self.checked.types.matches(actual, actual, expected)
            }
        }
    }

    fn value_id_matches_type(
        &self,
        id: ValueId,
        expected: RuntimeTyId,
    ) -> bool {
        self.arena
            .meta(id)
            .is_some_and(|m| self.checked.types.matches(m.ty, m.repr, expected))
            || self
                .arena
                .get(id)
                .is_some_and(|v| self.value_matches_type(v, expected))
    }

    /// Check if a value matches a `Named` alias or union type structurally.
    ///
    /// When `expected` is `Ty::Named(type_id, _)` and the registry entry is an
    /// alias or union, performs structural base-type comparison. For aliases,
    /// resolves the target through `ast_type_map` (if available) or the AST
    /// type expression. For unions, checks membership.
    fn alias_structurally_matches(
        &mut self,
        val: &Payload,
        expected: RuntimeTyId,
        ast_ty_id: AstTypeExprId,
        _span: Span,
    ) -> bool {
        if let Some(&expanded) = self.checked.alias_expansions.get(&ast_ty_id) {
            self.value_matches_type(val, RuntimeTyId::from(expanded))
        } else {
            self.alias_structurally_matches_inner(val, expected)
        }
    }

    fn alias_structurally_matches_inner(
        &mut self,
        val: &Payload,
        expected: RuntimeTyId,
    ) -> bool {
        let type_id = match self.checked.types.get(expected) {
            Ty::Named(id, _) => *id,
            _ => TypeId::UNKNOWN,
        };

        self.registry
            .get_def(type_id)
            .cloned()
            .is_some_and(|def| match def {
                TypeDef::Alias {
                    target,
                    type_params,
                    ..
                } => {
                    let args = match self.checked.types.get(expected) {
                        Ty::Named(_, args) => args.clone(),
                        _ => Default::default(),
                    };
                    self.alias_target_matches(val, target, &type_params, &args)
                }
                TypeDef::Union { members, .. } => {
                    members.iter().any(|m| *m == val.base_type())
                }
                _ => false,
            })
    }

    fn alias_target_matches(
        &mut self,
        val: &Payload,
        target: AstTypeExprId,
        ps: &[StringId],
        args: &[crate::typecheck::TyId],
    ) -> bool {
        let subst: Vec<_> =
            ps.iter().copied().zip(args.iter().copied()).collect();
        self.ast
            .get_type_expr(target)
            .cloned()
            .is_some_and(|te| match te {
                crate::ast::AstTypeExpr::Object(fields) => match val {
                    Payload::Object(obj) => fields.iter().all(|(name, ty)| {
                        obj.get(name).is_some_and(|&vid| {
                            self.resolve_alias_ty(*ty, &subst).is_some_and(
                                |rt| self.value_id_matches_type(vid, rt),
                            )
                        })
                    }),
                    _ => false,
                },
                _ => self.ast_alias_base_matches(val, target),
            })
    }

    fn resolve_alias_ty(
        &mut self,
        ast_id: AstTypeExprId,
        subst: &[(StringId, crate::typecheck::TyId)],
    ) -> Option<RuntimeTyId> {
        self.ast
            .get_type_expr(ast_id)
            .cloned()
            .and_then(|te| match te {
                crate::ast::AstTypeExpr::Named(name) => subst
                    .iter()
                    .find(|(n, _)| *n == name.local_name())
                    .map(|(_, ty)| RuntimeTyId::from(*ty))
                    .or_else(|| {
                        self.registry.lookup(&name).map(|tid| {
                            RuntimeTyId::from(Self::type_id_to_ty_id(
                                tid,
                                &mut self.ty_arena,
                            ))
                        })
                    }),
                crate::ast::AstTypeExpr::App(name, args) => {
                    self.registry.lookup(&name).map(|tid| {
                        let ts = args
                            .iter()
                            .filter_map(|id| {
                                self.resolve_alias_ty(*id, subst)
                                    .map(|rt| rt.raw())
                            })
                            .collect();
                        RuntimeTyId::from(self.ty_arena.named(tid, ts))
                    })
                }
                crate::ast::AstTypeExpr::Tuple(elems) => {
                    let ts = elems
                        .iter()
                        .filter_map(|id| {
                            self.resolve_alias_ty(*id, subst).map(|rt| rt.raw())
                        })
                        .collect();
                    Some(RuntimeTyId::from(self.ty_arena.alloc(Ty::Tuple(ts))))
                }
                crate::ast::AstTypeExpr::Object(fields) => {
                    let fs = fields
                        .iter()
                        .filter_map(|(name, id)| {
                            self.resolve_alias_ty(*id, subst)
                                .map(|rt| (*name, rt.raw()))
                        })
                        .collect();
                    Some(RuntimeTyId::from(self.ty_arena.alloc(Ty::Object(fs))))
                }
                _ => None,
            })
    }

    /// Fallback alias match via AST type expression when the target is not in
    /// `ast_type_map`. Compares the value's base `TypeId` with the AST type's
    /// implied base.
    fn ast_alias_base_matches(
        &self,
        val: &Payload,
        target: AstTypeExprId,
    ) -> bool {
        self.ast.get_type_expr(target).is_some_and(|te| {
            let base = val.base_type();
            match te {
                crate::ast::AstTypeExpr::Object(_) => base == TypeId::OBJECT,
                crate::ast::AstTypeExpr::Tuple(_) => base == TypeId::TUPLE,
                crate::ast::AstTypeExpr::Named(name)
                | crate::ast::AstTypeExpr::App(name, _) => {
                    self.registry.lookup(name).is_some_and(|tid| base == tid)
                }
                _ => false,
            }
        })
    }

    /// Check variant match, requiring zero-arity.
    ///
    /// Used for `is Type.Variant` without parens; variants with payloads
    /// must use `is Type.Variant(_)` or `is Type.Variant(name)`.
    fn check_variant_zero_arity(
        &self,
        val: &Payload,
        ty_name: &QualifiedName,
        var_name: StringId,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        // Type checker guarantees bare variant patterns match zero-arity variants
        if var_def.arity != 0 {
            typechecked!("is Type.Variant", "zero-arity");
        }

        Ok(match val {
            Payload::Tagged(ty, idx, _) => {
                *ty == type_id && *idx == var_def.idx
            }
            _ => false,
        })
    }

    /// Check if a value is a Tagged variant matching the given type and variant.
    pub(super) fn check_variant(
        &self,
        val: &Payload,
        ty_name: &QualifiedName,
        var_name: StringId,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        Ok(match val {
            Payload::Tagged(ty, idx, _) => {
                *ty == type_id && *idx == var_def.idx
            }
            _ => false,
        })
    }

    /// Look up a type and variant, returning their IDs.
    ///
    /// Typechecker validates that type and variant names are valid.
    pub(super) fn lookup_variant(
        &self,
        ty_name: &QualifiedName,
        var_name: StringId,
        _span: Span,
    ) -> Result<(TypeId, VariantDef)> {
        let type_id = self
            .registry
            .lookup(ty_name)
            .unwrap_or_else(|| typechecked!("pattern", "known type"));

        let var_def = self
            .registry
            .lookup_variant(type_id, var_name)
            .cloned()
            .unwrap_or_else(|| typechecked!("pattern", "known variant"));

        Ok((type_id, var_def))
    }

    /// Bind payload values to names in the current scope.
    pub(super) fn bind_payloads(
        &mut self,
        names: &[StringId],
        payloads: &[ValueId],
        span: Span,
    ) {
        // Fallback value if arena lookup fails (shouldn't happen normally)
        let fallback = self.make_none();
        names
            .iter()
            .zip(payloads.iter())
            .for_each(|(&nid, &val_id)| {
                // Re-add the value to get a fresh ValueId in case it matters
                let val =
                    self.arena.get(val_id).cloned().unwrap_or(fallback.clone());
                let new_val_id =
                    self.arena.add_typed(val, ValueMeta::untyped(), span);
                self.env.scopes.bind(nid, new_val_id);
            });
    }

    /// Try to match a pattern against a value.
    ///
    /// Returns `Some(bindings)` if the pattern matches, where bindings is a
    /// list of `(name_id, value_id)` pairs to bind in scope.
    /// Returns `None` if the pattern does not match.
    pub(super) fn try_match_pattern(
        &mut self,
        scrutinee: ExprId,
        pat_id: MatchPatternId,
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let pat = self.ast.get_pattern(pat_id).cloned().unwrap_or_else(|| {
            typechecked!("match pattern", "valid PatternId")
        });

        match &pat {
            MatchPattern::Wildcard => Ok(Some(vec![])),
            MatchPattern::Var(name) => {
                let val_id = self.arena.add_typed(
                    val.clone(),
                    ValueMeta::untyped(),
                    span,
                );
                Ok(Some(vec![(*name, val_id)]))
            }
            MatchPattern::Literal(lit) => {
                let lit_val = self.pattern_literal(lit, val);
                Ok(self.values_eq(val, &lit_val).then_some(vec![]))
            }
            MatchPattern::Variant(ty_name, var_name, sub_pats) => self
                .try_match_variant(
                    scrutinee, ty_name, *var_name, sub_pats, val, span,
                ),
            MatchPattern::Object(fields) => {
                self.try_match_object(scrutinee, fields, val, span)
            }
            MatchPattern::Tuple(pats) => {
                self.try_match_tuple(scrutinee, pats, val, span)
            }
            MatchPattern::Array(pats, rest) => {
                self.try_match_array(scrutinee, pats, rest.as_ref(), val, span)
            }
            MatchPattern::Is(name, ty_id) => {
                self.try_match_is(scrutinee, *name, *ty_id, val, span)
            }
        }
    }

    /// Try to match a type-narrowing pattern: `x IS Type`
    fn try_match_is(
        &mut self,
        scrutinee: ExprId,
        name: StringId,
        ast_ty_id: AstTypeExprId,
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let expected = self
            .checked
            .ast_type_map
            .get(&ast_ty_id)
            .copied()
            .unwrap_or_else(|| typechecked!("match IS", "in ast_type_map"));
        let matched = if self.checked.types.contains_var(expected) {
            let actual_base = val.base_type();
            self.checked
                .types
                .base_type(expected)
                .is_some_and(|eb| actual_base == eb)
        } else {
            let payload_ty = self.payload_runtime_ty(val);
            let actual = if payload_ty == RuntimeTyId::UNKNOWN {
                self.checked
                    .exprs
                    .get(&scrutinee)
                    .map(|e| e.ty)
                    .unwrap_or(payload_ty)
            } else {
                payload_ty
            };
            self.checked.types.matches(actual, actual, expected)
        };
        if matched {
            let val_id =
                self.arena
                    .add_typed(val.clone(), ValueMeta::untyped(), span);
            Ok(Some(vec![(name, val_id)]))
        } else {
            Ok(None)
        }
    }

    /// Try to match a variant pattern against a value.
    fn try_match_variant(
        &mut self,
        scrutinee: ExprId,
        ty_name: &QualifiedName,
        var_name: StringId,
        sub_pats: &[MatchPatternId],
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        match val {
            Payload::Tagged(ty, idx, payloads) => {
                if *ty == type_id && *idx == var_def.idx {
                    if payloads.len() != sub_pats.len() {
                        typechecked!("match variant", "matching arity");
                    }
                    self.try_match_all(scrutinee, sub_pats, payloads, span)
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    /// Try to match an object pattern against a value.
    fn try_match_object(
        &mut self,
        scrutinee: ExprId,
        fields: &[(StringId, MatchPatternId)],
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Payload::Object(obj) => {
                // Collect bindings from all field matches
                fields.iter().try_fold(Some(vec![]), |acc, (fid, pat_id)| {
                    acc.map_or(Ok(None), |mut bindings| {
                        obj.get(fid)
                            .and_then(|&vid| self.arena.get(vid).cloned())
                            .map_or(Ok(None), |fval| {
                                self.try_match_pattern(
                                    scrutinee, *pat_id, &fval, span,
                                )
                                .map(
                                    |maybe_sub| {
                                        maybe_sub.map(|sub| {
                                            bindings.extend(sub);
                                            bindings
                                        })
                                    },
                                )
                            })
                    })
                })
            }
            _ => Ok(None),
        }
    }

    /// Try to match a tuple pattern against a value.
    fn try_match_tuple(
        &mut self,
        scrutinee: ExprId,
        pats: &[MatchPatternId],
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Payload::Tuple(elems) => {
                if elems.len() != pats.len() {
                    Ok(None)
                } else {
                    self.try_match_all(scrutinee, pats, elems, span)
                }
            }
            _ => Ok(None),
        }
    }

    /// Try to match an array pattern against a value.
    ///
    /// - Without rest: matches arrays of exactly `pats.len()` elements
    /// - With rest: matches arrays of at least `pats.len()` elements
    fn try_match_array(
        &mut self,
        scrutinee: ExprId,
        pats: &[MatchPatternId],
        rest: Option<&RestPattern>,
        val: &Payload,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Payload::Array(elems) => {
                let len_ok = rest.map_or_else(
                    || elems.len() == pats.len(),
                    |_| elems.len() >= pats.len(),
                );
                if !len_ok {
                    Ok(None)
                } else {
                    let prefix_vals: SmallVec<[ValueId; 4]> =
                        elems.iter().take(pats.len()).copied().collect();
                    self.try_match_all(scrutinee, pats, &prefix_vals, span).map(
                        |maybe_bindings| {
                            maybe_bindings.map(|mut bindings| {
                                // Handle rest pattern
                                match rest {
                                    None | Some(RestPattern::Ignore) => {}
                                    Some(RestPattern::Bind(name)) => {
                                        // Bind remaining elements to `name`
                                        let rest_elems: SmallVec<_> = elems
                                            .iter()
                                            .skip(pats.len())
                                            .copied()
                                            .collect();
                                        let rest_arr = Payload::Array(
                                            Arc::new(rest_elems),
                                        );
                                        let val_id = self.arena.add_typed(
                                            rest_arr,
                                            ValueMeta::untyped(),
                                            span,
                                        );
                                        bindings.push((*name, val_id));
                                    }
                                }
                                bindings
                            })
                        },
                    )
                }
            }
            _ => Ok(None),
        }
    }

    /// Try to match multiple patterns against corresponding values.
    ///
    /// Returns `Some(bindings)` if all patterns match, `None` if any fails.
    fn try_match_all(
        &mut self,
        scrutinee: ExprId,
        pats: &[MatchPatternId],
        val_ids: &[ValueId],
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        pats.iter().zip(val_ids.iter()).try_fold(
            Some(vec![]),
            |acc, (&pat_id, &val_id)| {
                acc.map_or(Ok(None), |mut bindings| {
                    let val = self
                        .arena
                        .get(val_id)
                        .cloned()
                        .unwrap_or_else(|| invariant!("ValueId in arena"));
                    self.try_match_pattern(scrutinee, pat_id, &val, span).map(
                        |maybe_sub| {
                            maybe_sub.map(|sub| {
                                bindings.extend(sub);
                                bindings
                            })
                        },
                    )
                })
            },
        )
    }

    /// Apply bindings to the current scope.
    pub(super) fn apply_bindings(
        &mut self,
        bindings: &[(StringId, ValueId)],
        _span: Span,
    ) {
        bindings.iter().for_each(|&(name_id, val_id)| {
            self.env.scopes.bind(name_id, val_id);
        });
    }

    /// Check if two values are equal (for pattern matching literals).
    fn values_eq(&self, a: &Payload, b: &Payload) -> bool {
        match (a, b) {
            (Payload::Bool(x), Payload::Bool(y)) => x == y,
            (Payload::Int(x), Payload::Int(y)) => x == y,
            (Payload::Word(x), Payload::Word(y)) => x == y,
            (Payload::Float(x), Payload::Float(y)) => x == y,
            (Payload::String(x), Payload::String(y)) => x == y,
            (Payload::Char(x), Payload::Char(y)) => x == y,
            _ => false,
        }
    }

    /// Destructure a value according to a binding pattern, creating bindings.
    pub(super) fn destructure(
        &mut self,
        pat: &BindingPattern,
        val: &Payload,
        span: Span,
    ) -> Result<()> {
        match pat {
            BindingPattern::Var(name) => {
                let val_id = self.arena.add_typed(
                    val.clone(),
                    ValueMeta::untyped(),
                    span,
                );
                self.env.scopes.bind(*name, val_id);
                Ok(())
            }
            BindingPattern::Wildcard => Ok(()),
            BindingPattern::Tuple(pats) => match val {
                Payload::Tuple(elems) => {
                    if elems.len() != pats.len() {
                        typechecked!("destructure tuple", "matching size");
                    }
                    pats.iter().zip(elems.iter()).try_for_each(|(p, eid)| {
                        let elem =
                            self.arena.get(*eid).cloned().unwrap_or_else(
                                || typechecked!("tuple elem", "ValueId"),
                            );
                        self.destructure(p, &elem, span)
                    })
                }
                _ => typechecked!("destructure", "Tuple"),
            },
            BindingPattern::Object(fields) => match val {
                Payload::Object(obj) => {
                    let obj = obj.clone();
                    fields.iter().try_for_each(|(name, pat)| {
                        let vid = obj.get(name).copied().unwrap_or_else(|| {
                            typechecked!("object field", "exists")
                        });
                        let fval =
                            self.arena.get(vid).cloned().unwrap_or_else(|| {
                                typechecked!("field value", "ValueId")
                            });
                        self.destructure(pat, &fval, span)
                    })
                }
                _ => typechecked!("destructure", "Object"),
            },
            BindingPattern::Array(..) => {
                typechecked!("destructure", "no array pattern in LET")
            }
        }
    }
}
