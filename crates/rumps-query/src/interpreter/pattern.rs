//! Pattern matching and destructuring.

use std::sync::Arc;

use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{
    BindingPattern, ExprId, MatchPattern, MatchPatternId, RestPattern,
    TypePattern,
};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::typecheck::TypePatternInfo;
use crate::value::{Payload, TypeId, Value, ValueId, VariantDef};
use crate::{Result, Span};

struct VariantMatch<'a> {
    scrutinee: ExprId,
    ty_name: &'a QualifiedName,
    var_name: StringId,
    sub_pats: &'a [MatchPatternId],
    val_id: Option<ValueId>,
    val: &'a Value,
    span: Span,
}

impl<I: IoContext> Interpreter<'_, I> {
    /// Check if a value matches a type pattern (without binding).
    ///
    /// Uses the checked `Value` metadata for semantic and representation type
    /// checks.
    pub(super) fn check_pattern(
        &mut self,
        val: &Value,
        pattern: &TypePattern,
        info: Option<&TypePatternInfo>,
        span: Span,
    ) -> Result<bool> {
        match pattern {
            TypePattern::Type(_) => match info {
                Some(TypePatternInfo::Type(expected)) => {
                    Ok(self.checked.types.matches(val.ty, val.repr, *expected))
                }
                _ => {
                    typechecked!("type pattern", "checked target type")
                }
            },
            TypePattern::Variant(ref ty_name, var_name) => {
                self.check_variant_zero_arity(val, ty_name, *var_name, span)
            }
            TypePattern::VariantWildcard(ref ty_name, var_name) => {
                self.check_variant(val, ty_name, *var_name, span)
            }
            TypePattern::VariantBind(ref ty_name, var_name, _) => {
                self.check_variant(val, ty_name, *var_name, span)
            }
            TypePattern::Object(_) => match info {
                Some(TypePatternInfo::Object(fields)) => Ok(self
                    .checked
                    .types
                    .object_matches(&self.arena, val, fields)),
                _ => typechecked!("object pattern", "checked field types"),
            },
        }
    }

    /// Check variant match, requiring zero-arity.
    ///
    /// Used for `is Type.Variant` without parens; variants with payloads
    /// must use `is Type.Variant(_)` or `is Type.Variant(name)`.
    fn check_variant_zero_arity(
        &self,
        val: &Value,
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

        Ok(match &val.payload {
            Payload::Variant { tag, .. } => {
                self.value_matches_type_id(val, type_id) && *tag == var_def.idx
            }
            _ => false,
        })
    }

    /// Check if a value is a variant matching the given type and variant.
    pub(super) fn check_variant(
        &self,
        val: &Value,
        ty_name: &QualifiedName,
        var_name: StringId,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        Ok(match &val.payload {
            Payload::Variant { tag, .. } => {
                self.value_matches_type_id(val, type_id) && *tag == var_def.idx
            }
            _ => false,
        })
    }

    fn value_matches_type_id(&self, val: &Value, expected: TypeId) -> bool {
        self.checked
            .types
            .to_type_id(val.ty)
            .is_some_and(|ty| ty == expected)
            || self
                .checked
                .types
                .to_type_id(val.repr)
                .is_some_and(|ty| ty == expected)
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
        _span: Span,
    ) {
        names
            .iter()
            .zip(payloads.iter())
            .for_each(|(&nid, &val_id)| {
                self.env.scopes.bind(nid, val_id);
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
        val_id: Option<ValueId>,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let pat = self.ast.get_pattern(pat_id).cloned().unwrap_or_else(|| {
            typechecked!("match pattern", "valid PatternId")
        });

        match &pat {
            MatchPattern::Wildcard => Ok(Some(vec![])),
            MatchPattern::Var(name) => {
                let val_id =
                    val_id.unwrap_or_else(|| self.add_value(val.clone(), span));
                Ok(Some(vec![(*name, val_id)]))
            }
            MatchPattern::Literal(lit) => {
                let lit_val = self.pattern_literal(lit, &val.payload);
                Ok(self.values_eq(&val.payload, &lit_val).then_some(vec![]))
            }
            MatchPattern::Variant(ty_name, var_name, sub_pats) => self
                .try_match_variant(VariantMatch {
                    scrutinee,
                    ty_name,
                    var_name: *var_name,
                    sub_pats,
                    val_id,
                    val,
                    span,
                }),
            MatchPattern::Object(fields) => {
                self.try_match_object(scrutinee, fields, val, span)
            }
            MatchPattern::Tuple(pats) => {
                self.try_match_tuple(scrutinee, pats, val, span)
            }
            MatchPattern::Array(pats, rest) => self.try_match_array(
                scrutinee,
                pats,
                rest.as_ref(),
                val_id,
                val,
                span,
            ),
            MatchPattern::Is(name, _) => {
                self.try_match_is(pat_id, *name, val_id, val, span)
            }
        }
    }

    /// Try to match a type-narrowing pattern: `x IS Type`
    fn try_match_is(
        &mut self,
        pat: MatchPatternId,
        name: StringId,
        val_id: Option<ValueId>,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let expected = self.checked.match_target(pat);
        let matched = self.checked.types.matches(val.ty, val.repr, expected);
        if matched {
            let val_id =
                val_id.unwrap_or_else(|| self.add_value(val.clone(), span));
            Ok(Some(vec![(name, val_id)]))
        } else {
            Ok(None)
        }
    }

    /// Try to match a variant pattern against a value.
    fn try_match_variant(
        &mut self,
        m: VariantMatch<'_>,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let (type_id, var_def) =
            self.lookup_variant(m.ty_name, m.var_name, m.span)?;

        match &m.val.payload {
            Payload::Variant { tag, vals } => {
                if self.value_matches_type_id(m.val, type_id)
                    && *tag == var_def.idx
                {
                    if vals.len() != m.sub_pats.len() {
                        typechecked!("match variant", "matching arity");
                    }
                    self.try_match_all(m.scrutinee, m.sub_pats, vals, m.span)
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
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match &val.payload {
            Payload::Object(obj) => {
                fields.iter().try_fold(Some(vec![]), |acc, (fid, pat_id)| {
                    acc.map_or(Ok(None), |mut bindings| {
                        obj.get(fid)
                            .and_then(|&vid| {
                                self.arena
                                    .value(vid)
                                    .cloned()
                                    .map(|fval| (vid, fval))
                            })
                            .map_or(Ok(None), |(vid, fval)| {
                                self.try_match_pattern(
                                    scrutinee,
                                    *pat_id,
                                    Some(vid),
                                    &fval,
                                    span,
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
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match &val.payload {
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
        val_id: Option<ValueId>,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match &val.payload {
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
                                        let source = val_id
                                            .and_then(|id| {
                                                self.arena.value(id).cloned()
                                            })
                                            .unwrap_or_else(|| val.clone());
                                        let rest_id = self.add_value(
                                            Value {
                                                payload: rest_arr,
                                                ..source
                                            },
                                            span,
                                        );
                                        bindings.push((*name, rest_id));
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
                        .value(val_id)
                        .cloned()
                        .unwrap_or_else(|| invariant!("ValueId in arena"));
                    self.try_match_pattern(
                        scrutinee,
                        pat_id,
                        Some(val_id),
                        &val,
                        span,
                    )
                    .map(|maybe_sub| {
                        maybe_sub.map(|sub| {
                            bindings.extend(sub);
                            bindings
                        })
                    })
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
        val: &Value,
        span: Span,
    ) -> Result<()> {
        match pat {
            BindingPattern::Var(name) => {
                let val_id = self.add_value(val.clone(), span);
                self.env.scopes.bind(*name, val_id);
                Ok(())
            }
            BindingPattern::Wildcard => Ok(()),
            BindingPattern::Tuple(pats) => match &val.payload {
                Payload::Tuple(elems) => {
                    if elems.len() != pats.len() {
                        typechecked!("destructure tuple", "matching size");
                    }
                    pats.iter().zip(elems.iter()).try_for_each(|(p, eid)| {
                        let elem =
                            self.arena.value(*eid).cloned().unwrap_or_else(
                                || typechecked!("tuple elem", "ValueId"),
                            );
                        self.destructure(p, &elem, span)
                    })
                }
                _ => typechecked!("destructure", "Tuple"),
            },
            BindingPattern::Object(fields) => match &val.payload {
                Payload::Object(obj) => {
                    let obj = obj.clone();
                    fields.iter().try_for_each(|(name, pat)| {
                        let vid = obj.get(name).copied().unwrap_or_else(|| {
                            typechecked!("object field", "exists")
                        });
                        let fval =
                            self.arena.value(vid).cloned().unwrap_or_else(
                                || typechecked!("field value", "ValueId"),
                            );
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
