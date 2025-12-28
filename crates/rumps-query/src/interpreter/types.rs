//! Type checking, matching, and validation.

use indexmap::IndexMap;
use ordered_float::OrderedFloat;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::value::{TypeExprId, TypeId, Value, ValueId};
use crate::{Error, Result, Span};

impl<I: IoContext> Interpreter<'_, I> {
    /// Get the type expression for a runtime value.
    pub(super) fn value_type_expr(&mut self, v: &Value) -> TypeExprId {
        match v {
            Value::Unit => self.type_exprs.named(TypeId::UNIT),
            Value::Bool(_) => self.type_exprs.named(TypeId::BOOL),
            Value::Int(_) => self.type_exprs.named(TypeId::INT),
            Value::Float(_) => self.type_exprs.named(TypeId::FLOAT),
            Value::Char(_) => self.type_exprs.named(TypeId::CHAR),
            Value::String(_) => self.type_exprs.named(TypeId::STRING),
            Value::FilePath(_) => self.type_exprs.named(TypeId::FILEPATH),
            Value::Array(elem_ty, _) => {
                // Array[elem_ty]
                self.type_exprs
                    .app(TypeId::ARRAY, smallvec::smallvec![*elem_ty])
            }
            Value::Object(obj) => {
                // Build structural object type from actual field types.
                // Collect field values first to avoid borrow conflict.
                let entries: SmallVec<[(StringId, Value); 8]> = obj
                    .iter()
                    .filter_map(|(&name, &val_id)| {
                        self.arena.get(val_id).cloned().map(|v| (name, v))
                    })
                    .collect();
                let fields: IndexMap<StringId, TypeExprId> = entries
                    .into_iter()
                    .map(|(name, val)| {
                        let ty = self.value_type_expr(&val);
                        (name, ty)
                    })
                    .collect();
                self.type_exprs.object(fields)
            }
            Value::Tuple(ty, _) => *ty,
            Value::Map(k_ty, v_ty, _) => {
                // Map[k_ty, v_ty]
                self.type_exprs
                    .app(TypeId::MAP, smallvec::smallvec![*k_ty, *v_ty])
            }
            Value::Time(_) => self.type_exprs.named(TypeId::TIME),
            Value::Json(_) => self.type_exprs.named(TypeId::JSON),
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
            Value::Range { .. } => self.type_exprs.named(TypeId::RANGE),
        }
    }

    /// Get a human-readable name for a type expression (for error messages).
    pub(super) fn type_expr_name(&self, id: TypeExprId) -> String {
        self.type_exprs
            .format(
                id,
                |ty| {
                    self.registry
                        .type_name(ty, &self.arena)
                        .unwrap_or("?")
                        .to_owned()
                },
                |sid| self.arena.get_str(sid).unwrap_or("?").to_owned(),
            )
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
        self.try_resolve_type_expr(ast_id, span)?.ok_or_else(|| {
            Error::runtime_type(span, "unresolved type parameter")
        })
    }

    /// Try to resolve an AST type expression to a runtime `TypeExprId`.
    ///
    /// Returns `Ok(None)` if the type contains unresolved type parameters
    /// (e.g., `T` in a generic function). This is expected behavior, not an
    /// error: the typechecker has already validated that type parameters are
    /// used correctly at compile time. At runtime, the interpreter doesn't
    /// need concrete types for these annotations; it only needs runtime types
    /// for actual values being manipulated.
    pub(super) fn try_resolve_type_expr(
        &mut self,
        ast_id: AstTypeExprId,
        span: Span,
    ) -> Result<Option<TypeExprId>> {
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid type expression id")
            })?;

        match ast_ty {
            AstTypeExpr::Named(name) => {
                let name_id = self.arena.intern(&name);
                // If the type is not in the registry, it's likely a type parameter
                // from a generic function; return None to indicate unresolved
                Ok(self
                    .registry
                    .lookup(name_id)
                    .map(|ty_id| self.type_exprs.named(ty_id)))
            }
            AstTypeExpr::App(name, params) => {
                let name_id = self.arena.intern(&name);
                // If base type is not in registry, it's a type parameter
                let ty_id = match self.registry.lookup(name_id) {
                    Some(id) => id,
                    None => {
                        // Unresolved type param in App position
                        return Ok(None);
                    }
                };
                // Recursively resolve type parameters; if any is None, return None
                let resolved: Option<SmallVec<[TypeExprId; 2]>> = params
                    .iter()
                    .map(|&p| self.try_resolve_type_expr(p, span))
                    .collect::<Result<Option<SmallVec<_>>>>()?;
                Ok(resolved.map(|r| self.type_exprs.app(ty_id, r)))
            }
            AstTypeExpr::Fn(params, ret) => {
                // Recursively resolve param types; if any is None, return None
                let resolved_params: Option<SmallVec<[TypeExprId; 4]>> = params
                    .iter()
                    .map(|&p| self.try_resolve_type_expr(p, span))
                    .collect::<Result<Option<SmallVec<_>>>>()?;
                // Resolve return type
                let resolved_ret = self.try_resolve_type_expr(ret, span)?;
                Ok(resolved_params.and_then(|ps| {
                    resolved_ret.map(|r| self.type_exprs.fn_type(ps, r))
                }))
            }
            AstTypeExpr::Tuple(elems) => {
                // Recursively resolve element types; if any is None, return None
                let resolved: Option<SmallVec<[TypeExprId; 4]>> = elems
                    .iter()
                    .map(|&e| self.try_resolve_type_expr(e, span))
                    .collect::<Result<Option<SmallVec<_>>>>()?;
                Ok(resolved.map(|r| self.type_exprs.tuple(r)))
            }
            AstTypeExpr::Union(members) => {
                // Recursively resolve member types; if any is None, return None
                let resolved: Option<SmallVec<[TypeExprId; 4]>> = members
                    .iter()
                    .map(|&m| self.try_resolve_type_expr(m, span))
                    .collect::<Result<Option<SmallVec<_>>>>()?;
                Ok(resolved.map(|r| self.type_exprs.union(r)))
            }
            AstTypeExpr::Object(fields) => {
                // Resolve each field's type and intern field names; if any is None, return None
                let resolved: Option<IndexMap<StringId, TypeExprId>> = fields
                    .iter()
                    .map(|(name, ty_id)| {
                        let name_id = self.arena.intern(name);
                        self.try_resolve_type_expr(*ty_id, span)
                            .map(|opt| opt.map(|ty| (name_id, ty)))
                    })
                    .collect::<Result<Option<IndexMap<_, _>>>>()?;
                Ok(resolved.map(|r| self.type_exprs.object(r)))
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

            // T -> Json (jsonify anything that can be serialized)
            (_, TypeId::JSON) => self
                .jsonify(val)
                .map(Value::Json)
                .map_err(|e| Error::runtime_type(span, e.to_string())),

            // String -> FilePath
            (Value::String(sid), TypeId::FILEPATH) => Ok(Value::FilePath(*sid)),

            // FilePath identity
            (Value::FilePath(_), TypeId::FILEPATH) => Ok(val.clone()),

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::runtime_type(
                    span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Perform fallible type conversion for `READ`.
    ///
    /// This is the runtime helper for `expr READ Type` syntax.
    /// Returns a RUMPS `Result[T, String]` value (not `crate::Result`).
    pub(super) fn read_value(
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

            // Json -> Bool
            (Value::Json(j), TypeId::BOOL) => match j {
                serde_json::Value::Bool(b) => {
                    Ok(self.make_result_ok(Value::Bool(*b), span))
                }
                serde_json::Value::Null => {
                    Ok(self.make_result_err("expected Bool, got null", span))
                }
                _ => {
                    let msg =
                        format!("expected Bool, got {}", json_type_name(j));
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Json -> Int
            (Value::Json(j), TypeId::INT) => match j {
                serde_json::Value::Number(n) => match n.as_i64() {
                    Some(i) => Ok(self.make_result_ok(Value::Int(i), span)),
                    None => {
                        let msg =
                            format!("expected Int, got non-integer number {n}");
                        Ok(self.make_result_err(&msg, span))
                    }
                },
                serde_json::Value::Null => {
                    Ok(self.make_result_err("expected Int, got null", span))
                }
                _ => {
                    let msg =
                        format!("expected Int, got {}", json_type_name(j));
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Json -> Float
            (Value::Json(j), TypeId::FLOAT) => match j {
                serde_json::Value::Number(n) => match n.as_f64() {
                    Some(f) => Ok(self
                        .make_result_ok(Value::Float(OrderedFloat(f)), span)),
                    None => {
                        let msg =
                            format!("expected Float, got invalid number {n}");
                        Ok(self.make_result_err(&msg, span))
                    }
                },
                serde_json::Value::Null => {
                    Ok(self.make_result_err("expected Float, got null", span))
                }
                _ => {
                    let msg =
                        format!("expected Float, got {}", json_type_name(j));
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Json -> String
            (Value::Json(j), TypeId::STRING) => match j {
                serde_json::Value::String(s) => {
                    let id = self.arena.intern(s);
                    Ok(self.make_result_ok(Value::String(id), span))
                }
                serde_json::Value::Null => {
                    Ok(self.make_result_err("expected String, got null", span))
                }
                _ => {
                    let msg =
                        format!("expected String, got {}", json_type_name(j));
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Json -> Object
            (Value::Json(j), TypeId::OBJECT) => match j {
                serde_json::Value::Object(obj) => {
                    let fields = obj
                        .iter()
                        .map(|(k, v)| {
                            let key = self.arena.intern(k);
                            let val = self.unjsonify(v.clone());
                            let val_id = self.arena.add(val, span);
                            (key, val_id)
                        })
                        .collect();
                    Ok(self.make_result_ok(Value::Object(fields), span))
                }
                serde_json::Value::Null => {
                    Ok(self.make_result_err("expected Object, got null", span))
                }
                _ => {
                    let msg =
                        format!("expected Object, got {}", json_type_name(j));
                    Ok(self.make_result_err(&msg, span))
                }
            },

            // Unsupported conversion
            _ => {
                let src_name = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let tgt_name = self
                    .registry
                    .type_name(target, &self.arena)
                    .unwrap_or("Unknown");
                Err(Error::runtime_type(
                    span,
                    format!("cannot read {src_name} as {tgt_name}"),
                ))
            }
        }
    }

    /// Perform typed conversion for `READ` with full type expression support.
    ///
    /// Handles struct types (with type parameters), arrays, options, and
    /// delegates to `read_value` for primitive types.
    pub(super) fn read_value_expr(
        &mut self,
        val: &Value,
        target: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        let base_ty = self.type_exprs.base_type(target);
        let type_args = self.type_exprs.type_args(target).cloned();

        // Identity: if value already matches target type, return as-is
        if self.value_matches_type_expr(val, target) {
            Ok(self.make_result_ok(val.clone(), span))
        } else if let Some(fields) = self.get_struct_fields_resolved(target) {
            // Struct type: read object into struct
            self.read_to_struct(val, &fields, target, span)
        } else if base_ty == Some(TypeId::ARRAY) {
            // Array[T]: read JSON array with element type
            let elem_ty = type_args
                .as_ref()
                .and_then(|args| args.first().copied())
                .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
            self.read_json_to_array(val, elem_ty, span)
        } else if base_ty == Some(TypeId::OPTION) {
            // Option[T]: null -> None, otherwise read inner
            let inner_ty = type_args
                .as_ref()
                .and_then(|args| args.first().copied())
                .unwrap_or_else(|| self.type_exprs.named(TypeId::UNKNOWN));
            self.read_json_to_option(val, inner_ty, target, span)
        } else if let Some(ty_id) = base_ty {
            // Primitive type: delegate to read_value
            self.read_value(val, ty_id, span)
        } else if let Some(fields) =
            self.type_exprs.object_fields(target).cloned()
        {
            // Anonymous structural object type: `{ field: Type, ... }`
            // Reuse read_to_struct with the structural fields.
            self.read_to_struct(val, &fields, target, span)
        } else {
            let tgt_name = self.format_type_expr(target);
            Err(Error::runtime_type(
                span,
                format!("cannot read into type `{tgt_name}`"),
            ))
        }
    }

    /// Read an object (JSON or native) into a struct type.
    ///
    /// Accepts both `Value::Json(Object)` and `Value::Object`. The object must
    /// have all required fields with compatible types (extra fields are allowed).
    fn read_to_struct(
        &mut self,
        val: &Value,
        fields: &IndexMap<StringId, TypeExprId>,
        struct_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        // Extract object source; hard error if not an object
        let (json_obj, native_obj) = match val {
            Value::Json(serde_json::Value::Object(obj)) => {
                (Some(obj.clone()), None)
            }
            Value::Object(obj) => (None, Some(obj.clone())),
            _ => {
                let src = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let tgt = self.format_type_expr(struct_ty);
                Err(Error::runtime_type(
                    span,
                    format!("cannot read `{src}` as `{tgt}`"),
                ))?
            }
        };

        let fields = fields.clone();

        // Use a local enum to distinguish soft errors (Result.Err) from hard errors
        enum FieldErr {
            Soft(String),
            Hard(Error),
        }

        // Process each required field
        let result: std::result::Result<IndexMap<StringId, ValueId>, FieldErr> =
            fields
                .iter()
                .try_fold(IndexMap::new(), |mut acc, (&fid, &fty)| {
                    let fname = self
                        .arena
                        .get_str(fid)
                        .map(str::to_owned)
                        .unwrap_or_else(|| "?".to_owned());

                    // Get field value from JSON or native object
                    let field_val = json_obj
                        .as_ref()
                        .and_then(|obj| {
                            obj.get(&fname).map(|v| Value::Json(v.clone()))
                        })
                        .or_else(|| {
                            native_obj
                                .as_ref()
                                .and_then(|obj| obj.get(&fid))
                                .and_then(|id| self.arena.get(*id).cloned())
                        });

                    match field_val {
                        None => Err(FieldErr::Soft(format!(
                            "missing field `{fname}`"
                        ))),
                        Some(v) => {
                            let field_result = self
                                .read_value_expr(&v, fty, span)
                                .map_err(FieldErr::Hard)?;

                            // Check for Result.Err (soft error)
                            if let Value::Tagged(ty, 1, _) = &field_result {
                                if self.type_exprs.base_type(*ty)
                                    == Some(TypeId::RESULT)
                                {
                                    let msg = self
                                        .extract_result_err_msg(&field_result);
                                    Err(FieldErr::Soft(format!(
                                        "field `{fname}`: {msg}"
                                    )))
                                } else {
                                    // Tagged but not Result.Err; extract value
                                    let inner = self
                                        .unwrap_result_ok(&field_result, span)
                                        .map_err(FieldErr::Hard)?;
                                    let inner_id = self.arena.add(inner, span);
                                    acc.insert(fid, inner_id);
                                    Ok(acc)
                                }
                            } else {
                                // Result.Ok case
                                let inner = self
                                    .unwrap_result_ok(&field_result, span)
                                    .map_err(FieldErr::Hard)?;
                                let inner_id = self.arena.add(inner, span);
                                acc.insert(fid, inner_id);
                                Ok(acc)
                            }
                        }
                    }
                });

        match result {
            Ok(obj_fields) => {
                Ok(self.make_result_ok(Value::Object(obj_fields), span))
            }
            Err(FieldErr::Soft(msg)) => Ok(self.make_result_err(&msg, span)),
            Err(FieldErr::Hard(e)) => Err(e),
        }
    }

    /// Read a JSON array into an Array[T].
    fn read_json_to_array(
        &mut self,
        val: &Value,
        elem_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        if let Value::Json(serde_json::Value::Array(arr)) = val {
            let arr = arr.clone();

            // Process each element, propagating errors with index context.
            // We use a local enum to distinguish between:
            // - Continue accumulating elements
            // - Short-circuit with a soft error (Result.Err value)
            // - Short-circuit with a hard error (crate::Error)
            enum Acc {
                Elems(SmallVec<[ValueId; 4]>),
                SoftErr(Value),
            }

            let result = arr.into_iter().enumerate().try_fold(
                Acc::Elems(SmallVec::new()),
                |acc, (i, json_val)| match acc {
                    Acc::SoftErr(_) => Ok::<_, crate::Error>(acc),
                    Acc::Elems(mut elems) => {
                        let elem_result = self.read_value_expr(
                            &Value::Json(json_val),
                            elem_ty,
                            span,
                        )?;

                        // Check if the recursive read returned Result.Err
                        let is_soft_err =
                            matches!(&elem_result, Value::Tagged(ty, 1, _)
                                if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT));

                        if is_soft_err {
                            let err_msg =
                                self.extract_result_err_msg(&elem_result);
                            let msg = format!("at index {i}: {err_msg}");
                            Ok(Acc::SoftErr(self.make_result_err(&msg, span)))
                        } else {
                            let inner =
                                self.unwrap_result_ok(&elem_result, span)?;
                            let inner_id = self.arena.add(inner, span);
                            elems.push(inner_id);
                            Ok(Acc::Elems(elems))
                        }
                    }
                },
            )?;

            match result {
                Acc::Elems(elems) => {
                    Ok(self.make_result_ok(Value::Array(elem_ty, elems), span))
                }
                Acc::SoftErr(v) => Ok(v),
            }
        } else {
            let src_name =
                val.type_name(&self.registry, &self.type_exprs, &self.arena);
            let msg = format!("expected JSON array, got {src_name}");
            Ok(self.make_result_err(&msg, span))
        }
    }

    /// Read a JSON value into an Option[T].
    fn read_json_to_option(
        &mut self,
        val: &Value,
        inner_ty: TypeExprId,
        opt_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        match val {
            Value::Json(serde_json::Value::Null) => {
                // null -> Option.None
                let none = self.make_none_like(opt_ty);
                Ok(self.make_result_ok(none, span))
            }
            _ => {
                // Non-null: read inner value
                let inner_result = self.read_value_expr(val, inner_ty, span)?;

                // Check if the recursive read returned Result.Err
                let is_soft_err = matches!(
                    &inner_result,
                    Value::Tagged(ty, 1, _)
                        if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT)
                );

                if is_soft_err {
                    Ok(inner_result)
                } else {
                    let inner = self.unwrap_result_ok(&inner_result, span)?;
                    let inner_id = self.arena.add(inner, span);
                    let some = self.make_some(inner_id);
                    Ok(self.make_result_ok(some, span))
                }
            }
        }
    }

    /// Extract the Ok value from a Result.
    fn unwrap_result_ok(&self, result: &Value, span: Span) -> Result<Value> {
        match result {
            Value::Tagged(ty, 0, payloads)
                if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT) =>
            {
                payloads
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .ok_or_else(|| {
                        Error::runtime(span, "invalid Result.Ok payload")
                    })
            }
            _ => Err(Error::runtime(span, "expected Result.Ok")),
        }
    }

    /// Extract the error message from a Result.Err.
    fn extract_result_err_msg(&self, result: &Value) -> String {
        match result {
            Value::Tagged(ty, 1, payloads)
                if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT) =>
            {
                payloads
                    .first()
                    .and_then(|id| self.arena.get(*id))
                    .and_then(|v| match v {
                        Value::String(sid) => {
                            self.arena.get_str(*sid).map(str::to_owned)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| "unknown error".to_owned())
            }
            _ => "unknown error".to_owned(),
        }
    }

    /// Check if a value matches a simple type (non-variant).
    ///
    /// For union types, the value matches if it matches ANY member type.
    pub(super) fn value_matches_type(
        &mut self,
        val: &Value,
        type_id: TypeId,
    ) -> bool {
        // Check if target is a union type; if so, test each member
        if let Some(members) =
            self.registry.get_def(type_id).and_then(|def| match def {
                crate::value::TypeDef::Union { members, .. } => {
                    Some(members.clone())
                }
                _ => None,
            })
        {
            members
                .iter()
                .any(|&m| self.value_matches_type_expr(val, m))
        } else {
            self.value_matches_type_direct(val, type_id)
        }
    }

    /// Direct type match (non-union types).
    fn value_matches_type_direct(&self, val: &Value, type_id: TypeId) -> bool {
        match val {
            Value::Unit => type_id == TypeId::UNIT,
            Value::Bool(_) => type_id == TypeId::BOOL,
            Value::Int(_) => type_id == TypeId::INT,
            Value::Float(_) => type_id == TypeId::FLOAT,
            Value::Char(_) => type_id == TypeId::CHAR,
            Value::String(_) => type_id == TypeId::STRING,
            Value::FilePath(_) => type_id == TypeId::FILEPATH,
            Value::Array(_, _) => type_id == TypeId::ARRAY,
            Value::Object(_) => type_id == TypeId::OBJECT,
            Value::Tuple(_, _) => type_id == TypeId::TUPLE,
            Value::Map(_, _, _) => type_id == TypeId::MAP,
            Value::Time(_) => type_id == TypeId::TIME,
            Value::Json(_) => type_id == TypeId::JSON,
            Value::Tagged(ty_expr, _, _) => self
                .type_exprs
                .base_type(*ty_expr)
                .is_some_and(|t| t == type_id),
            // Closures, functions, and module functions don't have a simple TypeId
            Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. } => false,
            Value::Range { .. } => type_id == TypeId::RANGE,
        }
    }

    /// Check if an object matches resolved struct field requirements.
    ///
    /// Takes already-resolved field types (after type parameter substitution).
    pub(super) fn object_matches_resolved_fields(
        &mut self,
        obj: &IndexMap<StringId, ValueId>,
        fields: &IndexMap<StringId, TypeExprId>,
    ) -> bool {
        let fields = fields.clone();
        let obj = obj.clone();
        fields.iter().all(|(fname_id, &resolved_ty)| {
            obj.get(fname_id)
                .and_then(|&val_id| self.arena.get(val_id).cloned())
                .is_some_and(|val| self.field_matches_type(&val, resolved_ty))
        })
    }

    /// Check if a field value matches its expected type (recursive for nested structs).
    pub(super) fn field_matches_type(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
    ) -> bool {
        // Get resolved nested struct fields if this is a struct type
        let nested = self.get_struct_fields_resolved(expected_ty);
        if let Some(nested_fields) = nested {
            match val {
                Value::Object(nested_obj) => self
                    .object_matches_resolved_fields(nested_obj, &nested_fields),
                _ => false,
            }
        } else {
            self.value_matches_type_expr(val, expected_ty)
        }
    }

    /// Build a substitution map from type params to type args.
    fn build_subst(
        type_params: &SmallVec<[StringId; 2]>,
        type_args: Option<&SmallVec<[TypeExprId; 2]>>,
    ) -> IndexMap<StringId, TypeExprId> {
        type_args.map_or_else(IndexMap::new, |args| {
            type_params
                .iter()
                .zip(args.iter())
                .map(|(&p, &a)| (p, a))
                .collect()
        })
    }

    /// Resolve an AST type expression with type parameter substitution.
    fn resolve_ast_type_with_subst(
        &mut self,
        ast_id: AstTypeExprId,
        subst: &IndexMap<StringId, TypeExprId>,
    ) -> Result<TypeExprId> {
        let span = self.ast.type_expr_span(ast_id).unwrap_or_default();
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().ok_or_else(|| {
                Error::runtime(span, "invalid type expression id")
            })?;

        match ast_ty {
            AstTypeExpr::Named(name) => {
                // Check if it's a type parameter
                let name_id = self.arena.intern(&name);
                if let Some(&ty) = subst.get(&name_id) {
                    Ok(ty)
                } else {
                    // Regular type lookup
                    let ty_id =
                        self.registry.lookup(name_id).ok_or_else(|| {
                            Error::runtime_type(
                                span,
                                format!("unknown type: `{name}`"),
                            )
                        })?;
                    Ok(self.type_exprs.named(ty_id))
                }
            }
            AstTypeExpr::App(name, params) => {
                let name_id = self.arena.intern(&name);
                let ty_id = self.registry.lookup(name_id).ok_or_else(|| {
                    Error::runtime_type(span, format!("unknown type: `{name}`"))
                })?;
                let resolved: Result<SmallVec<[TypeExprId; 2]>> = params
                    .iter()
                    .map(|&p| self.resolve_ast_type_with_subst(p, subst))
                    .collect();
                Ok(self.type_exprs.app(ty_id, resolved?))
            }
            AstTypeExpr::Fn(params, ret) => {
                let resolved_params: Result<SmallVec<[TypeExprId; 4]>> = params
                    .iter()
                    .map(|&p| self.resolve_ast_type_with_subst(p, subst))
                    .collect();
                let resolved_ret =
                    self.resolve_ast_type_with_subst(ret, subst)?;
                Ok(self.type_exprs.fn_type(resolved_params?, resolved_ret))
            }
            AstTypeExpr::Tuple(elems) => {
                let resolved: Result<SmallVec<[TypeExprId; 4]>> = elems
                    .iter()
                    .map(|&e| self.resolve_ast_type_with_subst(e, subst))
                    .collect();
                Ok(self.type_exprs.tuple(resolved?))
            }
            AstTypeExpr::Union(members) => {
                let resolved: Result<SmallVec<[TypeExprId; 4]>> = members
                    .iter()
                    .map(|&m| self.resolve_ast_type_with_subst(m, subst))
                    .collect();
                Ok(self.type_exprs.union(resolved?))
            }
            AstTypeExpr::Object(fields) => {
                let resolved: Result<IndexMap<StringId, TypeExprId>> = fields
                    .iter()
                    .map(|(name, ty_id)| {
                        let name_id = self.arena.intern(name);
                        self.resolve_ast_type_with_subst(*ty_id, subst)
                            .map(|ty| (name_id, ty))
                    })
                    .collect();
                Ok(self.type_exprs.object(resolved?))
            }
        }
    }

    /// Check if a value matches a type expression.
    ///
    /// For simple types, delegates to `value_matches_type`.
    /// For function types, checks arity and param/return type compatibility.
    /// For union types, checks if value matches ANY member.
    /// For tuple types, checks element-wise matching.
    /// For struct types, checks field presence and types with substitution.
    pub(super) fn value_matches_type_expr(
        &mut self,
        val: &Value,
        ty: TypeExprId,
    ) -> bool {
        // Check for union type expression first
        if let Some(members) = self.type_exprs.union_members(ty).cloned() {
            members
                .iter()
                .any(|&m| self.value_matches_type_expr(val, m))
        } else if let Some(expected_elems) =
            self.type_exprs.tuple_elems(ty).cloned()
        {
            // Tuple type: check element-wise matching
            match val {
                Value::Tuple(_, actual_elems) => {
                    let actual_elems = actual_elems.clone();
                    expected_elems.len() == actual_elems.len()
                        && expected_elems.iter().zip(actual_elems.iter()).all(
                            |(&exp_ty, &val_id)| {
                                self.arena.get(val_id).cloned().is_some_and(
                                    |v| {
                                        self.value_matches_type_expr(&v, exp_ty)
                                    },
                                )
                            },
                        )
                }
                _ => false,
            }
        } else if let Some(fields) = self.type_exprs.object_fields(ty).cloned()
        {
            // Structural object type: check field presence and types (extensible)
            match val {
                Value::Object(obj) => {
                    let obj = obj.clone();
                    fields.iter().all(|(field_name, field_ty)| {
                        obj.get(field_name).is_some_and(|&val_id| {
                            self.arena.get(val_id).cloned().is_some_and(|v| {
                                self.value_matches_type_expr(&v, *field_ty)
                            })
                        })
                    })
                }
                _ => false,
            }
        } else if let Some(resolved_fields) =
            self.get_struct_fields_resolved(ty)
        {
            // Named struct type: check field presence and types
            match val {
                Value::Object(obj) => {
                    self.object_matches_resolved_fields(obj, &resolved_fields)
                }
                _ => false,
            }
        } else if let Some((params, ret)) = self.type_exprs.fn_parts(ty) {
            // Function type: check if value is a function/closure
            let params = params.clone();
            self.fn_value_matches(val, &params, ret)
        } else if let Some(type_id) = self.type_exprs.base_type(ty) {
            // Parameterized types: compare stored type args with expected
            match (
                type_id,
                self.type_exprs.type_args(ty).map(SmallVec::as_slice),
            ) {
                (TypeId::ARRAY, Some(&[expected_elem])) => match val {
                    Value::Array(actual_elem, _) => {
                        self.type_exprs.eq(*actual_elem, expected_elem)
                    }
                    _ => false,
                },
                (TypeId::MAP, Some(&[expected_k, expected_v])) => match val {
                    Value::Map(actual_k, actual_v, _) => {
                        self.type_exprs.eq(*actual_k, expected_k)
                            && self.type_exprs.eq(*actual_v, expected_v)
                    }
                    _ => false,
                },
                _ => self.value_matches_type(val, type_id),
            }
        } else {
            false
        }
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
            .format(
                ty,
                |tid| {
                    self.registry
                        .type_name(tid, &self.arena)
                        .unwrap_or("?")
                        .to_owned()
                },
                |sid| self.arena.get_str(sid).unwrap_or("?").to_owned(),
            )
            .unwrap_or_else(|| "?".to_owned())
    }

    /// Extract struct field definitions from a type expression.
    ///
    /// Returns `Some((&fields, &type_params, type_args))` if `ty` resolves to
    /// a struct type, `None` otherwise. The type_args are from the type expression
    /// (e.g., `[Int]` for `Box[Int]`).
    pub(super) fn get_struct_def(
        &self,
        ty: TypeExprId,
    ) -> Option<(
        &IndexMap<StringId, AstTypeExprId>,
        &SmallVec<[StringId; 2]>,
        Option<&SmallVec<[TypeExprId; 2]>>,
    )> {
        let base_ty = self.type_exprs.base_type(ty)?;
        let type_args = self.type_exprs.type_args(ty);
        self.registry.get_def(base_ty).and_then(|def| match def {
            crate::value::TypeDef::Struct {
                fields,
                type_params,
                ..
            } => Some((fields, type_params, type_args)),
            _ => None,
        })
    }

    /// Get resolved struct fields for a type expression.
    ///
    /// Resolves AST field types with type parameter substitution.
    /// Returns `None` if `ty` is not a struct type.
    pub(super) fn get_struct_fields_resolved(
        &mut self,
        ty: TypeExprId,
    ) -> Option<IndexMap<StringId, TypeExprId>> {
        // Get struct definition and extract what we need
        let (fields, type_params, type_args) = {
            let def = self.get_struct_def(ty)?;
            (def.0.clone(), def.1.clone(), def.2.cloned())
        };

        // Build substitution map
        let subst = Self::build_subst(&type_params, type_args.as_ref());

        // Resolve each field type with substitution
        fields
            .iter()
            .map(|(&fname_id, &ast_ty)| {
                self.resolve_ast_type_with_subst(ast_ty, &subst)
                    .ok()
                    .map(|resolved| (fname_id, resolved))
            })
            .collect()
    }

    /// Validate an object against struct field requirements.
    ///
    /// Checks both field presence AND field types recursively.
    /// Extensible-record style: extra fields in the object are allowed.
    pub(super) fn validate_object_fields(
        &mut self,
        obj: &IndexMap<StringId, ValueId>,
        expected_fields: &IndexMap<StringId, TypeExprId>,
        span: Span,
        ctx: Option<&str>,
    ) -> Result<()> {
        let expected_fields = expected_fields.clone();
        let obj = obj.clone();
        expected_fields.iter().try_for_each(|(fname_id, fty)| {
            let fname = self.arena.get_str(*fname_id).unwrap_or("?").to_owned();
            obj.get(fname_id).map_or_else(
                || {
                    let msg = ctx.map_or_else(
                        || format!("missing required field `{fname}`"),
                        |p| format!("parameter `{p}`: missing field `{fname}`"),
                    );
                    Err(Error::runtime_type(span, msg))
                },
                |val_id| {
                    self.arena.get(*val_id).cloned().map_or(Ok(()), |val| {
                        self.validate_field_type(&val, *fty, &fname, span, ctx)
                    })
                },
            )
        })
    }

    /// Validate a single field value against its expected type.
    pub(super) fn validate_field_type(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
        fname: &str,
        span: Span,
        ctx: Option<&str>,
    ) -> Result<()> {
        // Check for nested struct (get resolved fields)
        let nested = self.get_struct_fields_resolved(expected_ty);
        if let Some(nested_fields) = nested {
            // Nested struct: recursively validate
            match val {
                Value::Object(nested_obj) => self.validate_object_fields(
                    nested_obj,
                    &nested_fields,
                    span,
                    ctx,
                ),
                _ => {
                    let expected = self.format_type_expr(expected_ty);
                    let actual = val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena,
                    );
                    let msg = ctx.map_or_else(
                        || format!("field `{fname}`: expected `{expected}`, got `{actual}`"),
                        |p| {
                            format!(
                                "parameter `{p}`: field `{fname}` expected `{expected}`, got `{actual}`"
                            )
                        },
                    );
                    Err(Error::runtime_type(span, msg))
                }
            }
        } else {
            // Non-struct: use standard type matching
            if self.value_matches_type_expr(val, expected_ty) {
                Ok(())
            } else {
                let expected = self.format_type_expr(expected_ty);
                let actual = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let msg = ctx.map_or_else(
                    || format!("field `{fname}`: expected `{expected}`, got `{actual}`"),
                    |p| {
                        format!(
                            "parameter `{p}`: field `{fname}` expected `{expected}`, got `{actual}`"
                        )
                    },
                );
                Err(Error::runtime_type(span, msg))
            }
        }
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
        // Check if this is a struct type and get resolved fields
        let resolved_fields = self.get_struct_fields_resolved(expected_ty);

        if let Some(fields) = resolved_fields {
            match val {
                Value::Object(obj) => {
                    self.validate_object_fields(obj, &fields, span, None)
                }
                _ => {
                    let actual = val.type_name(
                        &self.registry,
                        &self.type_exprs,
                        &self.arena,
                    );
                    Err(Error::runtime_type(
                        span,
                        format!("expected struct (Object), got {actual}"),
                    ))
                }
            }
        } else {
            // Non-struct type: use semantic type matching (handles unions)
            if self.value_matches_type_expr(val, expected_ty) {
                Ok(())
            } else {
                let expected = self.type_expr_name(expected_ty);
                let actual = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                Err(Error::runtime_type(
                    span,
                    format!("type mismatch: expected {expected}, got {actual}"),
                ))
            }
        }
    }
}

/// Get a human-readable name for a JSON value type (for error messages).
fn json_type_name(j: &serde_json::Value) -> &'static str {
    match j {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "Bool",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Array(_) => "Array",
        serde_json::Value::Object(_) => "Object",
    }
}
