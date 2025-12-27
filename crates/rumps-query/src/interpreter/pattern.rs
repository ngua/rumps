//! Pattern matching and destructuring.

use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{
    BindingPattern, MatchPattern, MatchPatternId, RestPattern, TypePattern,
};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Check if a value matches a type pattern (without binding).
    pub(super) fn check_pattern(
        &mut self,
        val: &Value,
        pattern: &TypePattern,
        span: Span,
    ) -> Result<bool> {
        match pattern {
            TypePattern::Type(ast_ty_id) => {
                // Type check: `is Int`, `is Array[String]`, `is Map[K, V]`
                let ty_expr = self.resolve_type_expr(*ast_ty_id, span)?;
                Ok(self.value_matches_type_expr(val, ty_expr))
            }
            TypePattern::Variant(ty_name, var_name) => {
                // Variant check (zero-arity only): `is Option.None`
                // Variants with payloads must use `(_)` or `(name)`
                self.check_variant_zero_arity(val, ty_name, var_name, span)
            }
            TypePattern::VariantWildcard(ty_name, var_name) => {
                // Variant check ignoring payload: `is Option.Some(_)`
                self.check_variant(val, ty_name, var_name, span)
            }
            TypePattern::VariantBind(ty_name, var_name, _) => {
                // Variant check (bindings handled elsewhere): `is Option.Some(val)`
                self.check_variant(val, ty_name, var_name, span)
            }
            TypePattern::Object(fields) => {
                // Structural object check: `is { name: String, age: Int }`
                // Resolve field type exprs, then check value matches.
                match val {
                    Value::Object(obj) => {
                        let obj = obj.clone();
                        fields.iter().try_fold(true, |acc, (name, ty_id)| {
                            let ty = self.resolve_type_expr(*ty_id, span)?;
                            let name_id = self.arena.intern(name);
                            let matches =
                                obj.get(&name_id).is_some_and(|&vid| {
                                    self.arena.get(vid).cloned().is_some_and(
                                        |v| {
                                            self.value_matches_type_expr(&v, ty)
                                        },
                                    )
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
        ty_name: &str,
        var_name: &str,
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
        ty_name: &str,
        var_name: &str,
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
    pub(super) fn lookup_variant(
        &self,
        ty_name: &str,
        var_name: &str,
        span: Span,
    ) -> Result<(TypeId, crate::value::VariantDef)> {
        let ty_id = self.arena.lookup_string(ty_name);
        let var_id = self.arena.lookup_string(var_name);

        let type_id = ty_id
            .and_then(|id| self.registry.lookup(id))
            .ok_or_else(|| {
                Error::runtime(span, format!("unknown type `{ty_name}`"))
            })?;

        let var_def = var_id
            .and_then(|id| self.registry.lookup_variant(type_id, id))
            .cloned()
            .ok_or_else(|| {
                Error::runtime(
                    span,
                    format!("unknown variant `{ty_name}.{var_name}`"),
                )
            })?;

        Ok((type_id, var_def))
    }

    /// Bind payload values to names in the current scope.
    pub(super) fn bind_payloads(
        &mut self,
        names: &[String],
        payloads: &[ValueId],
        span: Span,
    ) {
        // Fallback value if arena lookup fails (shouldn't happen normally)
        let fallback = self.make_none();
        names
            .iter()
            .zip(payloads.iter())
            .for_each(|(name, &val_id)| {
                let name_id = self.arena.intern(name);
                // Re-add the value to get a fresh ValueId in case it matters
                let val =
                    self.arena.get(val_id).cloned().unwrap_or(fallback.clone());
                let new_val_id = self.arena.add(val, span);
                self.env.scopes.bind(name_id, new_val_id);
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
        let pat = self
            .ast
            .get_pattern(pat_id)
            .ok_or_else(|| Error::runtime(span, "invalid pattern id"))?
            .clone();
        match &pat {
            MatchPattern::Wildcard => Ok(Some(vec![])),
            MatchPattern::Var(name) => {
                let name_id = self.arena.intern(name);
                let val_id = self.arena.add(val.clone(), span);
                Ok(Some(vec![(name_id, val_id)]))
            }
            MatchPattern::Literal(lit) => {
                let lit_val = self.literal(lit);
                Ok(self.values_eq(val, &lit_val).then_some(vec![]))
            }
            MatchPattern::Variant(ty_name, var_name, sub_pats) => {
                self.try_match_variant(ty_name, var_name, sub_pats, val, span)
            }
            MatchPattern::Object(fields) => {
                self.try_match_object(fields, val, span)
            }
            MatchPattern::Tuple(pats) => self.try_match_tuple(pats, val, span),
            MatchPattern::Is(name, ty_id) => {
                self.try_match_is(name, *ty_id, val, span)
            }
        }
    }

    /// Try to match a type-narrowing pattern: `x IS Type`
    fn try_match_is(
        &mut self,
        name: &str,
        ast_ty_id: crate::ast::AstTypeExprId,
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        let ty_expr = self.resolve_type_expr(ast_ty_id, span)?;
        if self.value_matches_type_expr(val, ty_expr) {
            let name_id = self.arena.intern(name);
            let val_id = self.arena.add(val.clone(), span);
            Ok(Some(vec![(name_id, val_id)]))
        } else {
            Ok(None)
        }
    }

    /// Try to match a variant pattern against a value.
    fn try_match_variant(
        &mut self,
        ty_name: &str,
        var_name: &str,
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
        fields: &[(String, MatchPatternId)],
        val: &Value,
        span: Span,
    ) -> Result<Option<Vec<(StringId, ValueId)>>> {
        match val {
            Value::Object(obj) => {
                // Collect bindings from all field matches
                fields
                    .iter()
                    .try_fold(Some(vec![]), |acc, (fname, pat_id)| {
                        acc.map_or(Ok(None), |mut bindings| {
                            let fid = self.arena.intern(fname);
                            obj.get(&fid)
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
                    self.arena
                        .get(val_id)
                        .cloned()
                        .ok_or_else(|| Error::runtime(span, "invalid value id"))
                        .and_then(|val| {
                            self.try_match_pattern(pat_id, &val, span).map(
                                |maybe_sub| {
                                    maybe_sub.map(|sub| {
                                        bindings.extend(sub);
                                        bindings
                                    })
                                },
                            )
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
    fn values_eq(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Bool(x), Value::Bool(y)) => x == y,
            (Value::Int(x), Value::Int(y)) => x == y,
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
        match pat {
            BindingPattern::Var(name) => {
                let name_id = self.arena.intern(name);
                let val_id = self.arena.add(val.clone(), span);
                self.env.scopes.bind(name_id, val_id);
                Ok(())
            }
            BindingPattern::Wildcard => Ok(()), // discard value
            BindingPattern::Tuple(pats) => {
                self.destructure_tuple(pats, val, span)
            }
            BindingPattern::Object(fields) => {
                self.destructure_object(fields, val, span)
            }
            BindingPattern::Array(pats, rest) => {
                self.destructure_array(pats, rest.as_ref(), val, span)
            }
        }
    }

    /// Destructure a tuple value.
    ///
    /// Type checker guarantees pattern and value have matching sizes.
    fn destructure_tuple(
        &mut self,
        pats: &[BindingPattern],
        val: &Value,
        span: Span,
    ) -> Result<()> {
        match val {
            Value::Tuple(_, elems) => {
                // Type checker guarantees pattern and value have matching sizes
                if elems.len() != pats.len() {
                    typechecked!("destructure tuple", "matching size");
                }
                pats.iter().zip(elems.iter()).try_for_each(|(p, elem_id)| {
                    let elem =
                        self.arena.get(*elem_id).cloned().unwrap_or_else(
                            || typechecked!("tuple elem", "ValueId"),
                        );
                    self.destructure(p, &elem, span)
                })
            }
            // Type checker guarantees destructure target is a Tuple
            _ => typechecked!("destructure", "Tuple"),
        }
    }

    /// Destructure an object value.
    ///
    /// Type checker guarantees pattern fields exist in the object.
    fn destructure_object(
        &mut self,
        fields: &[(String, BindingPattern)],
        val: &Value,
        span: Span,
    ) -> Result<()> {
        match val {
            Value::Object(obj) => fields.iter().try_for_each(|(name, pat)| {
                let fid = self.arena.intern(name);
                // Type checker guarantees field exists
                let val_id = obj.get(&fid).copied().unwrap_or_else(|| {
                    typechecked!("destructure object", "field")
                });
                let field_val =
                    self.arena.get(val_id).cloned().unwrap_or_else(|| {
                        typechecked!("field value", "ValueId")
                    });
                self.destructure(pat, &field_val, span)
            }),
            // Type checker guarantees destructure target is an Object
            _ => typechecked!("destructure", "Object"),
        }
    }

    /// Destructure an array value.
    ///
    /// Size constraints are runtime checks (array length is not in the type).
    fn destructure_array(
        &mut self,
        pats: &[BindingPattern],
        rest: Option<&RestPattern>,
        val: &Value,
        span: Span,
    ) -> Result<()> {
        match val {
            Value::Array(ty_id, elems) => {
                // Array length is runtime-only; size mismatches are runtime errors
                if rest.is_none() && elems.len() != pats.len() {
                    Err(Error::runtime(
                        span,
                        format!(
                            "array size mismatch: pattern has {} elements, \
                             value has {}",
                            pats.len(),
                            elems.len()
                        ),
                    ))?;
                }
                if rest.is_some() && elems.len() < pats.len() {
                    Err(Error::runtime(
                        span,
                        format!(
                            "array too short: pattern needs at least {} elements, \
                             value has {}",
                            pats.len(),
                            elems.len()
                        ),
                    ))?;
                }

                // Bind prefix elements
                pats.iter()
                    .zip(elems.iter().take(pats.len()))
                    .try_for_each(|(p, elem_id)| {
                        let elem =
                            self.arena.get(*elem_id).cloned().unwrap_or_else(
                                || typechecked!("array elem", "ValueId"),
                            );
                        self.destructure(p, &elem, span)
                    })?;

                // Handle rest pattern
                match rest {
                    None => Ok(()),
                    Some(RestPattern::Ignore) => Ok(()),
                    Some(RestPattern::Bind(name)) => {
                        let rest_elems: SmallVec<_> =
                            elems.iter().skip(pats.len()).copied().collect();
                        let rest_arr = Value::Array(*ty_id, rest_elems);
                        let name_id = self.arena.intern(name);
                        let val_id = self.arena.add(rest_arr, span);
                        self.env.scopes.bind(name_id, val_id);
                        Ok(())
                    }
                }
            }
            // Type checker guarantees destructure target is an Array
            _ => typechecked!("destructure", "Array"),
        }
    }
}
