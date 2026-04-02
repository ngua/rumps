//! Pattern matching and destructuring.

use std::sync::Arc;

use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{
    BindingPattern, MatchPattern, MatchPatternId, RestPattern, TypePattern,
};
use crate::intern::{QualifiedName, StringId};
use crate::io::IoContext;
use crate::value::{TypeId, Value, ValueId};
use crate::{Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Recursively unwrap `Union`/`Newtype` wrappers to get the inner value.
    ///
    /// Returns `Some(inner)` if `val` was wrapped, `None` if it was not.
    pub(super) fn unwrap_value_recursive(&self, val: &Value) -> Option<Value> {
        match val {
            Value::Union(_, inner_id) | Value::Newtype(_, inner_id) => {
                self.arena.get(*inner_id).cloned().map(|inner| {
                    self.unwrap_value_recursive(&inner).unwrap_or(inner)
                })
            }
            _ => None,
        }
    }

    /// Check if a value matches a type pattern (without binding).
    pub(super) fn check_pattern(
        &mut self,
        val: &Value,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<bool> {
        // Unwrap Union/Newtype to check inner value for structural patterns
        let unwrapped = self.unwrap_value_recursive(val);
        let v = unwrapped.as_ref().unwrap_or(val);

        match pattern {
            TypePattern::Type(ast_ty_id) => {
                // Type check: `is Int`, `is Array[String]`, `is Option[_]`
                // Try to resolve; if None, type contains wildcards
                // Note: use original `val` since `value_matches_type_expr`
                // already handles Union/Newtype unwrapping
                match self.try_resolve_type_expr(*ast_ty_id, span)? {
                    Some(ty_expr) => {
                        Ok(self.value_matches_type_expr(val, ty_expr))
                    }
                    None => {
                        // Contains wildcards; check base type only
                        self.value_matches_ast_type_with_wildcards(
                            val, *ast_ty_id,
                        )
                    }
                }
            }
            TypePattern::Variant(ref ty_name, var_name) => {
                self.check_variant_zero_arity(v, ty_name, *var_name, span)
            }
            TypePattern::VariantWildcard(ref ty_name, var_name) => {
                self.check_variant(v, ty_name, *var_name, span)
            }
            TypePattern::VariantBind(ref ty_name, var_name, _) => {
                self.check_variant(v, ty_name, *var_name, span)
            }
            TypePattern::Object(fields) => {
                // Structural object check: `is { name: String, age: Int }`
                // Resolve field type exprs, then check value matches.
                match v {
                    Value::Object(obj) => {
                        let obj = obj.clone();
                        fields.iter().try_fold(true, |acc, (name, ty_id)| {
                            let ty = self.resolve_type_expr(*ty_id, span)?;
                            let matches = obj.get(name).is_some_and(|&vid| {
                                self.arena.get(vid).cloned().is_some_and(|fv| {
                                    self.value_matches_type_expr(&fv, ty)
                                })
                            });
                            Ok(acc && matches)
                        })
                    }
                    _ => Ok(false),
                }
            }
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

        Ok(match val {
            Value::Tagged(ty_expr, idx, _) => {
                self.type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id)
                    && *idx == var_def.idx
            }
            _ => false,
        })
    }

    /// Check if a value is a Tagged variant matching the given type and variant.
    pub(super) fn check_variant(
        &self,
        val: &Value,
        ty_name: &QualifiedName,
        var_name: StringId,
        span: Span,
    ) -> Result<bool> {
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        Ok(match val {
            Value::Tagged(ty_expr, idx, _) => {
                self.type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id)
                    && *idx == var_def.idx
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
    ) -> Result<(TypeId, crate::value::VariantDef)> {
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
                let new_val_id = self.arena.add(val, span);
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
        pat_id: MatchPatternId,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let pat = self.ast.get_pattern(pat_id).cloned().unwrap_or_else(|| {
            typechecked!("match pattern", "valid PatternId")
        });

        // For most structural patterns, unwrap Union/Newtype
        let unwrapped = self.unwrap_value_recursive(val);
        let v = unwrapped.as_ref().unwrap_or(val);

        match &pat {
            // Wildcard and Var bind the ORIGINAL value (preserving wrapper)
            MatchPattern::Wildcard => Ok(Some(vec![])),
            MatchPattern::Var(name) => {
                let val_id = self.arena.add(val.clone(), span);
                Ok(Some(vec![(*name, val_id)]))
            }
            // Literal uses unwrapped value for comparison
            MatchPattern::Literal(lit) => {
                let lit_val = self.pattern_literal(lit, v);
                Ok(self.values_eq(v, &lit_val).then_some(vec![]))
            }
            // Structural patterns use unwrapped value
            MatchPattern::Variant(ty_name, var_name, sub_pats) => {
                self.try_match_variant(ty_name, *var_name, sub_pats, v, span)
            }
            MatchPattern::Object(fields) => {
                self.try_match_object(fields, v, span)
            }
            MatchPattern::Tuple(pats) => self.try_match_tuple(pats, v, span),
            MatchPattern::Array(pats, rest) => {
                self.try_match_array(pats, rest.as_ref(), v, span)
            }
            // Is pattern uses original value (value_matches_type_expr handles unwrapping)
            MatchPattern::Is(name, ty_id) => {
                self.try_match_is(*name, *ty_id, val, span)
            }
        }
    }

    /// Try to match a type-narrowing pattern: `x IS Type`
    fn try_match_is(
        &mut self,
        name: StringId,
        ast_ty_id: crate::ast::AstTypeExprId,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let ty_expr = self.resolve_type_expr(ast_ty_id, span)?;
        if self.value_matches_type_expr(val, ty_expr) {
            // Unwrap Union/Newtype wrappers to bind the inner value
            let unwrapped = self.unwrap_value_recursive(val);
            let bound_val = unwrapped.as_ref().unwrap_or(val);
            let val_id = self.arena.add(bound_val.clone(), span);
            Ok(Some(vec![(name, val_id)]))
        } else {
            Ok(None)
        }
    }

    /// Try to match a variant pattern against a value.
    fn try_match_variant(
        &mut self,
        ty_name: &QualifiedName,
        var_name: StringId,
        sub_pats: &[MatchPatternId],
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        // Look up the type and variant
        let (type_id, var_def) =
            self.lookup_variant(ty_name, var_name, span)?;

        match val {
            Value::Tagged(ty_expr, idx, payloads) => {
                // Check type and variant match
                let type_matches = self
                    .type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id);
                let variant_matches = *idx == var_def.idx;

                if type_matches && variant_matches {
                    // Type checker guarantees pattern arity matches variant arity
                    if payloads.len() != sub_pats.len() {
                        typechecked!("match variant", "matching arity");
                    }
                    // Recursively match sub-patterns against payloads
                    self.try_match_all(sub_pats, payloads, span)
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
        fields: &[(StringId, MatchPatternId)],
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Value::Object(obj) => {
                // Collect bindings from all field matches
                fields.iter().try_fold(Some(vec![]), |acc, (fid, pat_id)| {
                    acc.map_or(Ok(None), |mut bindings| {
                        obj.get(fid)
                            .and_then(|&vid| self.arena.get(vid).cloned())
                            .map_or(Ok(None), |fval| {
                                self.try_match_pattern(*pat_id, &fval, span)
                                    .map(|maybe_sub| {
                                        maybe_sub.map(|sub| {
                                            bindings.extend(sub);
                                            bindings
                                        })
                                    })
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
        pats: &[MatchPatternId],
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Value::Tuple(_, elems) => {
                if elems.len() != pats.len() {
                    Ok(None)
                } else {
                    self.try_match_all(pats, elems, span)
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
        pats: &[MatchPatternId],
        rest: Option<&crate::ast::RestPattern>,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Value::Array(ty_id, elems) => {
                // Check length constraints
                let len_ok = rest.map_or_else(
                    || elems.len() == pats.len(),
                    |_| elems.len() >= pats.len(),
                );
                if !len_ok {
                    Ok(None)
                } else {
                    // Match prefix elements
                    let prefix_vals: SmallVec<[ValueId; 4]> =
                        elems.iter().take(pats.len()).copied().collect();
                    self.try_match_all(pats, &prefix_vals, span).map(
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
                                        let rest_arr = Value::Array(
                                            *ty_id,
                                            Arc::new(rest_elems),
                                        );
                                        let val_id =
                                            self.arena.add(rest_arr, span);
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
                    self.try_match_pattern(pat_id, &val, span).map(
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
    fn values_eq(&self, a: &Value, b: &Value) -> bool {
        // Unwrap Union/Newtype wrappers
        let ua = self.unwrap_value_recursive(a);
        let ub = self.unwrap_value_recursive(b);
        let a = ua.as_ref().unwrap_or(a);
        let b = ub.as_ref().unwrap_or(b);

        match (a, b) {
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y,
            (Value::Word(x), Value::Word(y)) => x == y,
            (Value::Float(x), Value::Float(y)) => x == y,
            (Value::String(x), Value::String(y)) => x == y,
            (Value::Char(x), Value::Char(y)) => x == y,
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
        // Unwrap Union/Newtype for structural patterns
        let unwrapped = self.unwrap_value_recursive(val);
        let v = unwrapped.as_ref().unwrap_or(val);

        match pat {
            // Var binds the ORIGINAL value (preserving wrapper)
            BindingPattern::Var(name) => {
                let val_id = self.arena.add(val.clone(), span);
                self.env.scopes.bind(*name, val_id);
                Ok(())
            }
            BindingPattern::Wildcard => Ok(()),
            BindingPattern::Tuple(pats) => match v {
                Value::Tuple(_, elems) => {
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
            BindingPattern::Object(fields) => match v {
                Value::Object(obj) => {
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
