//! Type checking, matching, and validation.

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::io::IoContext;
use crate::value::{StringId, TypeExprId, TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Get the type expression for a runtime value.
    pub(super) fn value_type_expr(&mut self, v: &Value) -> TypeExprId {
        match v {
            Value::Bool(_) => self.type_exprs.named(TypeId::BOOL),
            Value::Int(_) => self.type_exprs.named(TypeId::INT),
            Value::Float(_) => self.type_exprs.named(TypeId::FLOAT),
            Value::Char(_) => self.type_exprs.named(TypeId::CHAR),
            Value::String(_) => self.type_exprs.named(TypeId::STRING),
            Value::Array(elem_ty, _) => {
                // Array[elem_ty]
                self.type_exprs
                    .app(TypeId::ARRAY, smallvec::smallvec![*elem_ty])
            }
            Value::Object(_) => self.type_exprs.named(TypeId::OBJECT),
            Value::Tuple(ty, _) => *ty,
            Value::Map(k_ty, v_ty, _) => {
                // Map[k_ty, v_ty]
                self.type_exprs
                    .app(TypeId::MAP, smallvec::smallvec![*k_ty, *v_ty])
            }
            Value::Time(_) => self.type_exprs.named(TypeId::TIME),
            Value::Tagged(ty_expr, _, _) => *ty_expr,
            Value::Closure { params, ret, .. }
            | Value::Function { params, ret, .. } => {
                // Build function type from params and return type
                let param_tys: SmallVec<[TypeExprId; 4]> = params
                    .iter()
                    .map(|(_, ty)| {
                        ty.unwrap_or_else(|| {
                            self.type_exprs.named(TypeId::UNKNOWN)
                        })
                    })
                    .collect();
                let ret_ty = ret
                    .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
                self.type_exprs.fn_type(param_tys, ret_ty)
            }
            // Module functions don't have a simple type expression
            Value::ModuleFn { .. } => self.type_exprs.named(TypeId::UNKNOWN),
        }
    }

    /// Get a human-readable name for a type expression (for error messages).
    pub(super) fn type_expr_name(&self, id: TypeExprId) -> String {
        self.type_exprs
            .format(id, |ty| {
                self.registry
                    .type_name(ty, &self.arena)
                    .unwrap_or("?")
                    .to_owned()
            })
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Resolve an AST type expression to a runtime `TypeExprId`.
    ///
    /// Looks up type names in the registry and builds the runtime type.
    pub(super) fn resolve_type_expr(
        &mut self,
        ast_id: AstTypeExprId,
        span: Span,
    ) -> Result<TypeExprId> {
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid type expression id")
            })?;

        match ast_ty {
            AstTypeExpr::Named(name) => {
                let name_id = self.arena.intern(&name);
                let ty_id = self.registry.lookup(name_id).ok_or_else(|| {
                    Error::type_err(span, format!("unknown type: {name}"))
                })?;
                Ok(self.type_exprs.named(ty_id))
            }
            AstTypeExpr::App(name, params) => {
                let name_id = self.arena.intern(&name);
                let ty_id = self.registry.lookup(name_id).ok_or_else(|| {
                    Error::type_err(span, format!("unknown type: {name}"))
                })?;
                // Recursively resolve type parameters
                let resolved: Result<SmallVec<[TypeExprId; 2]>> = params
                    .iter()
                    .map(|&p| self.resolve_type_expr(p, span))
                    .collect();
                Ok(self.type_exprs.app(ty_id, resolved?))
            }
            AstTypeExpr::Fn(params, ret) => {
                // Recursively resolve param types
                let resolved_params: Result<SmallVec<[TypeExprId; 4]>> = params
                    .iter()
                    .map(|&p| self.resolve_type_expr(p, span))
                    .collect();
                // Resolve return type
                let resolved_ret = self.resolve_type_expr(ret, span)?;
                Ok(self.type_exprs.fn_type(resolved_params?, resolved_ret))
            }
            AstTypeExpr::Tuple(elems) => {
                // Recursively resolve element types
                let resolved: Result<SmallVec<[TypeExprId; 4]>> = elems
                    .iter()
                    .map(|&e| self.resolve_type_expr(e, span))
                    .collect();
                Ok(self.type_exprs.tuple(resolved?))
            }
        }
    }

    /// Perform type coercion for `as` casts.
    pub(super) fn coerce(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        match (val, target) {
            // Identity casts
            (Value::Int(_), TypeId::INT)
            | (Value::Float(_), TypeId::FLOAT)
            | (Value::Bool(_), TypeId::BOOL)
            | (Value::Char(_), TypeId::CHAR)
            | (Value::String(_), TypeId::STRING) => Ok(val.clone()),

            // Int -> Float (widen)
            (Value::Int(n), TypeId::FLOAT) => {
                Ok(Value::Float(OrderedFloat(*n as f64)))
            }

            // Float -> Int (truncate)
            (Value::Float(f), TypeId::INT) => Ok(Value::Int(f.0 as i64)),

            // Bool -> Int
            (Value::Bool(b), TypeId::INT) => {
                Ok(Value::Int(if *b { 1 } else { 0 }))
            }

            // T -> String (stringify anything)
            (_, TypeId::STRING) => {
                let s = self.stringify(val);
                let id = self.arena.intern(&s);
                Ok(Value::String(id))
            }

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(&self.registry, &self.type_exprs);
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::type_err(
                    span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Perform fallible type conversion for `read`.
    ///
    /// Returns a RUMPS `Result[T, String]` value.
    pub(super) fn try_convert(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        match (val, target) {
            // String -> Int
            (Value::String(sid), TypeId::INT) => {
                let s = self
                    .arena
                    .get_str(*sid)
                    .map(str::to_owned)
                    .unwrap_or_default();
                match s.parse::<i64>() {
                    Ok(n) => Ok(self.make_result_ok(Value::Int(n), span)),
                    Err(_) => {
                        let msg = format!("invalid integer: {s}");
                        Ok(self.make_result_err(&msg, span))
                    }
                }
            }

            // String -> Float
            (Value::String(sid), TypeId::FLOAT) => {
                let s = self
                    .arena
                    .get_str(*sid)
                    .map(str::to_owned)
                    .unwrap_or_default();
                match s.parse::<f64>() {
                    Ok(n) => Ok(self
                        .make_result_ok(Value::Float(OrderedFloat(n)), span)),
                    Err(_) => {
                        let msg = format!("invalid float: {s}");
                        Ok(self.make_result_err(&msg, span))
                    }
                }
            }

            // Int -> Bool (strict: only 0 and 1)
            (Value::Int(n), TypeId::BOOL) => match *n {
                0 => Ok(self.make_result_ok(Value::Bool(false), span)),
                1 => Ok(self.make_result_ok(Value::Bool(true), span)),
                _ => {
                    let msg = format!("expected 0 or 1 for Bool, got {n}");
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(&self.registry, &self.type_exprs);
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::type_err(
                    span,
                    format!("cannot read {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Check if a value matches a simple type (non-variant).
    pub(super) fn value_matches_type(
        &self,
        val: &Value,
        type_id: TypeId,
    ) -> bool {
        match val {
            Value::Bool(_) => type_id == TypeId::BOOL,
            Value::Int(_) => type_id == TypeId::INT,
            Value::Float(_) => type_id == TypeId::FLOAT,
            Value::Char(_) => type_id == TypeId::CHAR,
            Value::String(_) => type_id == TypeId::STRING,
            Value::Array(_, _) => type_id == TypeId::ARRAY,
            Value::Object(obj) => {
                // Object matches Object type directly, or any struct type
                // whose required fields are present with correct types
                if type_id == TypeId::OBJECT {
                    true
                } else {
                    self.registry.get_def(type_id).is_some_and(
                        |def| match def {
                            crate::value::TypeDef::Struct {
                                fields, ..
                            } => self.object_matches_struct_fields(obj, fields),
                            _ => false,
                        },
                    )
                }
            }
            Value::Tuple(_, _) => type_id == TypeId::TUPLE,
            Value::Map(_, _, _) => type_id == TypeId::MAP,
            Value::Time(_) => type_id == TypeId::TIME,
            Value::Tagged(ty_expr, _, _) => self
                .type_exprs
                .base_type(*ty_expr)
                .is_some_and(|t| t == type_id),
            // Closures, functions, and module functions don't have a simple TypeId
            Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. } => false,
        }
    }

    /// Check if an object matches struct field requirements (presence + types).
    pub(super) fn object_matches_struct_fields(
        &self,
        obj: &IndexMap<StringId, ValueId>,
        fields: &IndexMap<StringId, TypeExprId>,
    ) -> bool {
        fields.iter().all(|(fname_id, fty)| {
            obj.get(fname_id).is_some_and(|val_id| {
                self.arena
                    .get(*val_id)
                    .is_some_and(|val| self.field_matches_type(val, *fty))
            })
        })
    }

    /// Check if a field value matches its expected type (recursive for nested structs).
    pub(super) fn field_matches_type(
        &self,
        val: &Value,
        expected_ty: TypeExprId,
    ) -> bool {
        self.get_struct_fields(expected_ty).map_or_else(
            || self.value_matches_type_expr(val, expected_ty),
            |nested_fields| match val {
                Value::Object(nested_obj) => {
                    self.object_matches_struct_fields(nested_obj, nested_fields)
                }
                _ => false,
            },
        )
    }

    /// Check if a value matches a type expression.
    ///
    /// For simple types, delegates to `value_matches_type`.
    /// For function types, checks arity and param/return type compatibility.
    pub(super) fn value_matches_type_expr(
        &self,
        val: &Value,
        ty: TypeExprId,
    ) -> bool {
        // Try simple type first
        self.type_exprs.base_type(ty).map_or_else(
            || {
                // Function type: check if value is a function/closure with matching signature
                self.type_exprs.fn_parts(ty).is_some_and(|(params, ret)| {
                    self.fn_value_matches(val, params, ret)
                })
            },
            |type_id| self.value_matches_type(val, type_id),
        )
    }

    /// Check if a function/closure value matches a function type.
    fn fn_value_matches(
        &self,
        val: &Value,
        expected_params: &[TypeExprId],
        expected_ret: TypeExprId,
    ) -> bool {
        match val {
            Value::Closure { params, ret, .. }
            | Value::Function { params, ret, .. } => {
                // Check arity
                params.len() == expected_params.len()
                    // Check param types (if annotated)
                    && params.iter().zip(expected_params.iter()).all(
                        |((_, actual_ty), expected_ty)| {
                            actual_ty.is_none_or(|a| self.type_exprs.eq(a, *expected_ty))
                        },
                    )
                    // Check return type (if annotated)
                    && ret.is_none_or(|r| self.type_exprs.eq(r, expected_ret))
            }
            _ => false,
        }
    }

    /// Format a type expression for error messages.
    pub(super) fn format_type_expr(&self, ty: TypeExprId) -> String {
        self.type_exprs
            .format(ty, |tid| {
                self.registry
                    .type_name(tid, &self.arena)
                    .unwrap_or("?")
                    .to_owned()
            })
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Extract struct field definitions from a type expression.
    ///
    /// Returns `Some(&fields)` if `ty` resolves to a struct type, `None` otherwise.
    pub(super) fn get_struct_fields(
        &self,
        ty: TypeExprId,
    ) -> Option<&IndexMap<StringId, TypeExprId>> {
        self.type_exprs
            .base_type(ty)
            .and_then(|ty_id| self.registry.get_def(ty_id))
            .and_then(|def| match def {
                crate::value::TypeDef::Struct { fields, .. } => Some(fields),
                _ => None,
            })
    }

    /// Validate an object against struct field requirements.
    ///
    /// Checks both field presence AND field types recursively.
    /// Extensible-record style: extra fields in the object are allowed.
    pub(super) fn validate_object_fields(
        &self,
        obj: &IndexMap<StringId, ValueId>,
        expected_fields: &IndexMap<StringId, TypeExprId>,
        span: Span,
        ctx: Option<&str>,
    ) -> Result<()> {
        expected_fields.iter().try_for_each(|(fname_id, fty)| {
            let fname = self.arena.get_str(*fname_id).unwrap_or("?");
            obj.get(fname_id).map_or_else(
                || {
                    let msg = ctx.map_or_else(
                        || format!("missing required field `{fname}`"),
                        |p| format!("parameter `{p}`: missing field `{fname}`"),
                    );
                    Err(Error::type_err(span, msg))
                },
                |val_id| {
                    self.arena.get(*val_id).map_or(Ok(()), |val| {
                        self.validate_field_type(val, *fty, fname, span, ctx)
                    })
                },
            )
        })
    }

    /// Validate a single field value against its expected type.
    pub(super) fn validate_field_type(
        &self,
        val: &Value,
        expected_ty: TypeExprId,
        fname: &str,
        span: Span,
        ctx: Option<&str>,
    ) -> Result<()> {
        // Check for nested struct
        self.get_struct_fields(expected_ty).map_or_else(
            || {
                // Non-struct: use standard type matching
                if self.value_matches_type_expr(val, expected_ty) {
                    Ok(())
                } else {
                    let expected = self.format_type_expr(expected_ty);
                    let actual =
                        val.type_name(&self.registry, &self.type_exprs);
                    let msg = ctx.map_or_else(
                        || {
                            format!(
                                "field `{fname}`: expected `{expected}`, \
                                 got `{actual}`"
                            )
                        },
                        |p| {
                            format!(
                                "parameter `{p}`: field `{fname}` expected \
                                 `{expected}`, got `{actual}`"
                            )
                        },
                    );
                    Err(Error::type_err(span, msg))
                }
            },
            |nested_fields| {
                // Nested struct: recursively validate
                match val {
                    Value::Object(nested_obj) => self.validate_object_fields(
                        nested_obj,
                        nested_fields,
                        span,
                        ctx,
                    ),
                    _ => {
                        let expected = self.format_type_expr(expected_ty);
                        let actual =
                            val.type_name(&self.registry, &self.type_exprs);
                        let msg = ctx.map_or_else(
                            || {
                                format!(
                                    "field `{fname}`: expected `{expected}`, \
                                     got `{actual}`"
                                )
                            },
                            |p| {
                                format!(
                                    "parameter `{p}`: field `{fname}` expected \
                                     `{expected}`, got `{actual}`"
                                )
                            },
                        );
                        Err(Error::type_err(span, msg))
                    }
                }
            },
        )
    }

    /// Validate that a value conforms to an expected type.
    ///
    /// For struct types, validates extensible-record style: the object must
    /// have at least the declared fields with correct types (extra fields OK).
    pub(super) fn validate_type(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
        span: Span,
    ) -> Result<()> {
        // Check if this is a struct type; if so, validate object fields
        let is_struct = self.get_struct_fields(expected_ty).is_some();

        if is_struct {
            match val {
                Value::Object(obj) => {
                    // Re-fetch fields (reference released by is_some() check)
                    self.get_struct_fields(expected_ty).map_or(
                        Ok(()),
                        |fields| {
                            self.validate_object_fields(obj, fields, span, None)
                        },
                    )
                }
                _ => {
                    let actual =
                        val.type_name(&self.registry, &self.type_exprs);
                    Err(Error::type_err(
                        span,
                        format!("expected struct (Object), got {actual}"),
                    ))
                }
            }
        } else {
            // Non-struct type: use exact type matching
            let actual_ty = self.value_type_expr(val);
            if self.type_exprs.eq(expected_ty, actual_ty) {
                Ok(())
            } else {
                let expected = self.type_expr_name(expected_ty);
                let actual = self.type_expr_name(actual_ty);
                Err(Error::type_err(
                    span,
                    format!("type mismatch: expected {expected}, got {actual}"),
                ))
            }
        }
    }
}
