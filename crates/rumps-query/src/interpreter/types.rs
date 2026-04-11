//! Type checking, matching, and validation.

use std::sync::Arc;

use indexmap::IndexMap;
use smallvec::SmallVec;

use super::Interpreter;
use crate::ast::{AstTypeExpr, AstTypeExprId};
use crate::intern::StringId;
use crate::io::IoContext;
use crate::typecheck::{Ty, TyArena, TyId};
use crate::value::{TypeExprId, TypeId, Value, ValueId};
use crate::{ClassId, Error, Result, Span};

/// Helper enum for `coerce()` to avoid borrow checker issues.
#[derive(Copy, Clone)]
enum DefKind {
    Union,
    Alias,
    Other,
}

impl<I: IoContext> Interpreter<'_, I> {
    /// Get the type expression for a runtime value.
    pub(super) fn value_type_expr(&mut self, v: &Value) -> TypeExprId {
        match v {
            Value::Unit => self.type_exprs.named(TypeId::UNIT),
            Value::Bool(_) => self.type_exprs.named(TypeId::BOOL),
            Value::Int(_) => self.type_exprs.named(TypeId::INT),
            Value::Word(_) => self.type_exprs.named(TypeId::WORD),
            Value::Float(_) => self.type_exprs.named(TypeId::FLOAT),
            Value::Char(_) => self.type_exprs.named(TypeId::CHAR),
            Value::String(_) => self.type_exprs.named(TypeId::STRING),
            Value::FilePath(_) => self.type_exprs.named(TypeId::FILEPATH),
            Value::Regex(_) => self.type_exprs.named(TypeId::REGEX),
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
            Value::Tagged(ty_expr, _, _)
            | Value::Union(ty_expr, _)
            | Value::Newtype(ty_expr, _) => *ty_expr,
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
            // Module functions/consts/class methods/partial apps don't have a simple type expression
            Value::ModuleFn { .. }
            | Value::ModuleConst { .. }
            | Value::ClassMethodFn { .. }
            | Value::PartialApp { .. } => {
                self.type_exprs.named(TypeId::UNKNOWN)
            }
            Value::Range { .. } => self.type_exprs.named(TypeId::RANGE),
            // Internal loop control types; not exposed to users
            Value::ForeverContinuation | Value::LoopContinue(_) => {
                self.type_exprs.named(TypeId::UNIT)
            }
            Value::Ref(..) => self.type_exprs.named(TypeId::REF),
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
        self.try_resolve_type_expr(ast_id, span)?
            .ok_or_else(|| typechecked!("type expression", "resolved"))
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
            self.ast.get_type_expr(ast_id).cloned().unwrap_or_else(|| {
                typechecked!("type expr", "valid AstTypeExprId")
            });

        match ast_ty {
            // Wildcard is unresolved (used for type matching with unknown params)
            AstTypeExpr::Wildcard => Ok(None),
            AstTypeExpr::Named(name) => {
                // If the type is not in the registry, it's likely a type parameter
                // from a generic function; return None to indicate unresolved
                Ok(self
                    .registry
                    .lookup(&name)
                    .map(|ty_id| self.type_exprs.named(ty_id)))
            }
            AstTypeExpr::App(name, params) => {
                // If base type is not in registry, it's a type parameter
                let ty_id = match self.registry.lookup(&name) {
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
                // Resolve each field's type; field names already `StringId`
                let resolved: Option<IndexMap<StringId, TypeExprId>> = fields
                    .iter()
                    .map(|(name, ty_id)| {
                        self.try_resolve_type_expr(*ty_id, span)
                            .map(|opt| opt.map(|ty| (*name, ty)))
                    })
                    .collect::<Result<Option<IndexMap<_, _>>>>()?;
                Ok(resolved.map(|r| self.type_exprs.object(r)))
            }
            // VarApp is a type variable application (`F[T]`); unresolvable
            // at runtime (type params are erased). Return `None`.
            AstTypeExpr::VarApp(..) => Ok(None),
            // Associated types (`:Index`, `Indexable:Index`) cannot be resolved
            // at runtime because the AST doesn't store their resolved types.
            // The typechecker has already verified correctness; return `None`
            // to indicate the type annotation should be skipped at runtime.
            AstTypeExpr::AssocType { .. } => Ok(None),
        }
    }

    /// Perform type coercion for `AS` casts via `Into[T]` dispatch.
    ///
    /// When the target is a named union or newtype, wraps the value appropriately.
    /// EXCEPTION: `Storable AS T` must remain a runtime error (handled by `TryInto`).
    pub(super) fn coerce(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        // Check if target is a union or alias type; copy the kind to avoid borrow issues
        let def_kind = self.registry.get_def(target).map(|def| match def {
            crate::value::TypeDef::Union { .. } => DefKind::Union,
            crate::value::TypeDef::Alias { .. } => DefKind::Alias,
            _ => DefKind::Other,
        });

        match def_kind {
            Some(DefKind::Union) => {
                let inner_id = self.arena.add(val.clone(), span);
                let ty_expr = self.type_exprs.named(target);
                Ok(Value::Union(ty_expr, inner_id))
            }
            Some(DefKind::Alias) => {
                let inner_id = self.arena.add(val.clone(), span);
                let ty_expr = self.type_exprs.named(target);
                Ok(Value::Newtype(ty_expr, inner_id))
            }
            _ => {
                let ty_id = Self::type_id_to_ty_id(target, &mut self.ty_arena);
                let ty = self.ty_arena.get(ty_id).clone();
                let mid = self.arena.intern("into");
                self.dispatch_convert(ClassId::INTO, mid, val, &ty, span)
            }
        }
    }

    /// Convert a runtime `TypeId` to the corresponding `TyId` in the type arena.
    fn type_id_to_ty_id(id: TypeId, ta: &mut TyArena) -> TyId {
        match id {
            TypeId::UNIT => TyArena::UNIT,
            TypeId::BOOL => TyArena::BOOL,
            TypeId::INT => TyArena::INT,
            TypeId::WORD => TyArena::WORD,
            TypeId::FLOAT => TyArena::FLOAT,
            TypeId::CHAR => TyArena::CHAR,
            TypeId::STRING => TyArena::STRING,
            TypeId::FILEPATH => TyArena::FILEPATH,
            TypeId::JSON => TyArena::JSON,
            TypeId::TIME => TyArena::TIME,
            TypeId::RANGE => TyArena::RANGE,
            TypeId::ORDERING => TyArena::ORDERING,
            TypeId::DATA_STATUS => TyArena::DATA_STATUS,
            TypeId::PATH => TyArena::PATH,
            TypeId::REGEX => TyArena::REGEX,
            TypeId::LOCAL => TyArena::LOCAL,
            TypeId::GLOBAL => TyArena::GLOBAL,
            other => ta.alloc(Ty::Named(other, smallvec::smallvec![])),
        }
    }

    /// Perform fallible type conversion for `read` via `TryInto[T]` dispatch.
    ///
    /// This is the runtime helper for `expr READ Type` syntax.
    /// Returns a RUMPS `Result[T, String]` value (not `crate::Result`).
    pub(super) fn read_value(
        &mut self,
        val: &Value,
        target: TypeId,
        span: Span,
    ) -> Result<Value> {
        let ty_id = Self::type_id_to_ty_id(target, &mut self.ty_arena);
        let ty = self.ty_arena.get(ty_id).clone();
        let mid = self.arena.intern("try-into");
        self.dispatch_convert(ClassId::TRY_INTO, mid, val, &ty, span)
    }

    /// Perform typed conversion for `read` with full type expression support.
    ///
    /// Handles object alias types (with type parameters), arrays, options, and
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
        } else if let Some(fields) = self.resolve_object_alias_fields(target) {
            // Object alias type: read into object
            self.read_to_object(val, &fields, target, span)
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
            // Reuse read_to_object with the structural fields.
            self.read_to_object(val, &fields, target, span)
        } else {
            let tgt_name = self.format_type_expr(target);
            let msg = format!("cannot read into type `{tgt_name}`");
            Ok(self.make_result_err(&msg, span))
        }
    }

    /// Read an object (JSON or native) into an object alias type.
    ///
    /// Accepts both `Value::Json(Object)` and `Value::Object`. The object must
    /// have all required fields with compatible types (extra fields are allowed).
    fn read_to_object(
        &mut self,
        val: &Value,
        fields: &IndexMap<StringId, TypeExprId>,
        obj_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        // Extract object source; soft error if not an object
        let objs = match val {
            Value::Json(j) => match j.as_ref() {
                serde_json::Value::Object(obj) => {
                    Some((Some(obj.clone()), None))
                }
                _ => None,
            },
            Value::Object(obj) => Some((None, Some(Arc::clone(obj)))),
            _ => None,
        };

        match objs {
            None => {
                let src = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let tgt = self.format_type_expr(obj_ty);
                let msg = format!("cannot read `{src}` as `{tgt}`");
                Ok(self.make_result_err(&msg, span))
            }
            Some((json_obj, native_obj)) => {
                let fields = fields.clone();

                // Use a local enum to distinguish soft errors (Result.Err) from hard errors
                #[allow(clippy::large_enum_variant)]
                enum FieldErr {
                    Soft(String),
                    Hard(Error),
                }

                // Process each required field
                let result: std::result::Result<
                    IndexMap<StringId, ValueId>,
                    FieldErr,
                > = fields.iter().try_fold(
                    IndexMap::new(),
                    |mut acc, (&fid, &fty)| {
                        let fname = self
                            .arena
                            .get_str(fid)
                            .map(str::to_owned)
                            .unwrap_or_else(|| "?".to_owned());

                        // Get field value from JSON or native object
                        let field_val = json_obj
                            .as_ref()
                            .and_then(|obj| {
                                obj.get(&fname)
                                    .map(|v| Value::Json(Arc::new(v.clone())))
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
                                        let msg = self.extract_result_err_msg(
                                            &field_result,
                                        );
                                        Err(FieldErr::Soft(format!(
                                            "field `{fname}`: {msg}"
                                        )))
                                    } else {
                                        // Tagged but not Result.Err; extract value
                                        let inner = self
                                            .unwrap_result_ok(
                                                &field_result,
                                                span,
                                            )
                                            .map_err(FieldErr::Hard)?;
                                        let inner_id =
                                            self.arena.add(inner, span);
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
                    },
                );

                match result {
                    Ok(obj_fields) => Ok(self.make_result_ok(
                        Value::Object(Arc::new(obj_fields)),
                        span,
                    )),
                    Err(FieldErr::Soft(msg)) => {
                        Ok(self.make_result_err(&msg, span))
                    }
                    Err(FieldErr::Hard(e)) => Err(e),
                }
            }
        }
    }

    /// Read a JSON array into an Array[T].
    fn read_json_to_array(
        &mut self,
        val: &Value,
        elem_ty: TypeExprId,
        span: Span,
    ) -> Result<Value> {
        let arr = match val {
            Value::Json(j) => match j.as_ref() {
                serde_json::Value::Array(arr) => Some(arr.clone()),
                _ => None,
            },
            _ => None,
        };

        match arr {
            Some(arr) => {
                // Process each element, propagating errors with index context.
                // We use a local enum to distinguish between:
                // - Continue accumulating elements
                // - Short-circuit with a soft error (`Result.Err` value)
                // - Short-circuit with a hard error (`crate::Error`)
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
                                &Value::Json(Arc::new(json_val)),
                                elem_ty,
                                span,
                            )?;

                            // Check if the recursive read returned `Result.Err`
                            let is_soft_err =
                                matches!(&elem_result, Value::Tagged(ty, 1, _)
                                    if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT));

                            if is_soft_err {
                                let err_msg =
                                    self.extract_result_err_msg(&elem_result);
                                let msg = format!("at index {i}: {err_msg}");
                                Ok(Acc::SoftErr(
                                    self.make_result_err(&msg, span),
                                ))
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
                    Acc::Elems(elems) => Ok(self.make_result_ok(
                        Value::Array(elem_ty, Arc::new(elems)),
                        span,
                    )),
                    Acc::SoftErr(v) => Ok(v),
                }
            }
            None => {
                let src_name = val.type_name(
                    &self.registry,
                    &self.type_exprs,
                    &self.arena,
                );
                let msg = format!("expected JSON array, got {src_name}");
                Ok(self.make_result_err(&msg, span))
            }
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
            Value::Json(j) if matches!(j.as_ref(), serde_json::Value::Null) => {
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
    ///
    /// Callers must ensure this is only invoked on `Result.Ok` values.
    fn unwrap_result_ok(&self, result: &Value, _span: Span) -> Result<Value> {
        match result {
            Value::Tagged(ty, 0, payloads)
                if self.type_exprs.base_type(*ty) == Some(TypeId::RESULT) =>
            {
                Ok(payloads
                    .first()
                    .and_then(|id| self.arena.get(*id).cloned())
                    .unwrap_or_else(|| typechecked!("Result.Ok", "payload")))
            }
            _ => typechecked!("unwrap_result_ok", "Result.Ok"),
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
        // Delegate to expand_alias_and_match with no type args
        self.expand_alias_and_match(val, type_id, None)
    }

    /// Check if a type expression represents a union type.
    ///
    /// Returns `true` for both anonymous unions (`Int | String`) and named
    /// unions (`Storable`, `Subscript`).
    pub(super) fn is_union_type(&self, ty: TypeExprId) -> bool {
        // Check for anonymous union
        self.type_exprs.union_members(ty).is_some()
            || self
                .type_exprs
                .base_type(ty)
                .and_then(|id| self.registry.get_def(id))
                .is_some_and(|def| {
                    matches!(def, crate::value::TypeDef::Union { .. })
                })
    }

    /// Direct type match (non-union types).
    fn value_matches_type_direct(&self, val: &Value, type_id: TypeId) -> bool {
        match val {
            Value::Unit => type_id == TypeId::UNIT,
            Value::Bool(_) => type_id == TypeId::BOOL,
            Value::Int(_) => type_id == TypeId::INT,
            Value::Word(_) => type_id == TypeId::WORD,
            Value::Float(_) => type_id == TypeId::FLOAT,
            Value::Char(_) => type_id == TypeId::CHAR,
            Value::String(_) => type_id == TypeId::STRING,
            Value::FilePath(_) => type_id == TypeId::FILEPATH,
            Value::Regex(_) => type_id == TypeId::REGEX,
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
            // Closures, functions, module functions/consts, and class methods
            // don't have a simple TypeId
            Value::Closure { .. }
            | Value::Function { .. }
            | Value::ModuleFn { .. }
            | Value::ModuleConst { .. }
            | Value::ClassMethodFn { .. }
            | Value::PartialApp { .. } => false,
            Value::Range { .. } => type_id == TypeId::RANGE,
            // Internal loop control types; don't match user types
            Value::ForeverContinuation | Value::LoopContinue(_) => false,
            // Ref values match Local or Global based on the is_global flag
            Value::Ref(is_global, ..) => {
                if *is_global {
                    type_id == TypeId::GLOBAL
                } else {
                    type_id == TypeId::LOCAL
                }
            }
            // Check if wrapper type matches, or recursively check inner value
            Value::Union(ty_expr, inner_id)
            | Value::Newtype(ty_expr, inner_id) => {
                // First check if the wrapper type itself matches
                self.type_exprs
                    .base_type(*ty_expr)
                    .is_some_and(|t| t == type_id)
                    || self.arena.get(*inner_id).cloned().is_some_and(|inner| {
                        self.value_matches_type_direct(&inner, type_id)
                    })
            }
        }
    }

    /// Expand an alias with type args and match, or fall back to simple match.
    fn expand_alias_and_match(
        &mut self,
        val: &Value,
        type_id: TypeId,
        type_args: Option<SmallVec<[TypeExprId; 2]>>,
    ) -> bool {
        match self.registry.get_def(type_id) {
            Some(crate::value::TypeDef::Alias {
                type_params,
                target,
                ..
            }) => {
                let type_params = type_params.clone();
                let target = *target;
                // Build substitution from type params to type args
                let subst = Self::build_subst(&type_params, type_args.as_ref());
                self.resolve_ast_type_with_subst(target, &subst)
                    .ok()
                    .is_some_and(|ty| self.value_matches_type_expr(val, ty))
            }
            Some(crate::value::TypeDef::Union { members, .. }) => {
                let members = members.clone();
                members
                    .iter()
                    .any(|&m| self.value_matches_type_expr(val, m))
            }
            _ => self.value_matches_type_direct(val, type_id),
        }
    }

    /// Check if an object matches resolved object alias field requirements.
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

    /// Check if a field value matches its expected type (recursive for nested objects).
    pub(super) fn field_matches_type(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
    ) -> bool {
        // Get resolved nested object fields if this is an object alias type
        let nested = self.resolve_object_alias_fields(expected_ty);
        if let Some(nested_fields) = nested {
            match val {
                Value::Object(nested_obj) => self
                    .object_matches_resolved_fields(
                        nested_obj.as_ref(),
                        &nested_fields,
                    ),
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
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().unwrap_or_else(|| {
                typechecked!("type expr", "valid AstTypeExprId")
            });

        match ast_ty {
            // Wildcard shouldn't appear in runtime type resolution
            AstTypeExpr::Wildcard => {
                typechecked!("type resolution", "no wildcard at runtime")
            }
            AstTypeExpr::Named(name) => {
                // Check if it's a type parameter
                if let Some(&ty) = subst.get(&name.local_name()) {
                    Ok(ty)
                } else {
                    // Regular type lookup; type checker guarantees it exists
                    let ty_id =
                        self.registry.lookup(&name).unwrap_or_else(|| {
                            typechecked!("type lookup", "known type")
                        });
                    Ok(self.type_exprs.named(ty_id))
                }
            }
            AstTypeExpr::App(name, params) => {
                // Type checker guarantees the type exists
                let ty_id = self.registry.lookup(&name).unwrap_or_else(|| {
                    typechecked!("type lookup", "known type")
                });
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
                        self.resolve_ast_type_with_subst(*ty_id, subst)
                            .map(|ty| (*name, ty))
                    })
                    .collect();
                Ok(self.type_exprs.object(resolved?))
            }
            // VarApp (`F[T]`) contains a type variable; check substitution
            AstTypeExpr::VarApp(name, params) => {
                if let Some(&ty) = subst.get(&name.local_name()) {
                    // Substituted to concrete base; resolve args and apply
                    let base = self.type_exprs.base_type(ty);
                    let resolved: Result<SmallVec<[TypeExprId; 2]>> = params
                        .iter()
                        .map(|&p| self.resolve_ast_type_with_subst(p, subst))
                        .collect();
                    base.map_or_else(
                        || Ok(ty),
                        |b| Ok(self.type_exprs.app(b, resolved?)),
                    )
                } else {
                    // Unresolved type param; typechecker already validated
                    typechecked!("VarApp substitution", "type param in subst")
                }
            }
            // Associated types should be resolved by typechecker before runtime
            AstTypeExpr::AssocType { .. } => {
                typechecked!("type resolution", "associated types resolved")
            }
        }
    }

    /// Check if a value matches a type expression.
    ///
    /// For simple types, delegates to `value_matches_type`.
    /// For function types, checks arity and param/return type compatibility.
    /// For union types, checks if value matches ANY member.
    /// For tuple types, checks element-wise matching.
    /// For object alias types, checks field presence and types with substitution.
    pub(super) fn value_matches_type_expr(
        &mut self,
        val: &Value,
        ty: TypeExprId,
    ) -> bool {
        // First check if the value is wrapped in Union/Newtype
        match val {
            Value::Union(val_ty, inner_id) => {
                self.type_exprs.eq(*val_ty, ty)
                    || self.arena.get(*inner_id).cloned().is_some_and(|inner| {
                        self.value_matches_type_expr(&inner, ty)
                    })
            }
            Value::Newtype(val_ty, inner_id) => {
                self.type_exprs.eq(*val_ty, ty)
                    || self.type_exprs.base_type(*val_ty).is_some_and(|base| {
                        self.type_exprs.base_type(ty) == Some(base)
                    })
                    || self.arena.get(*inner_id).cloned().is_some_and(|inner| {
                        self.value_matches_type_expr(&inner, ty)
                    })
            }
            _ => {
                // Check for union type expression first
                if let Some(members) =
                    self.type_exprs.union_members(ty).cloned()
                {
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
                                && expected_elems
                                    .iter()
                                    .zip(actual_elems.iter())
                                    .all(|(&exp_ty, &val_id)| {
                                        self.arena
                                            .get(val_id)
                                            .cloned()
                                            .is_some_and(|v| {
                                                self.value_matches_type_expr(
                                                    &v, exp_ty,
                                                )
                                            })
                                    })
                        }
                        _ => false,
                    }
                } else if let Some(fields) =
                    self.type_exprs.object_fields(ty).cloned()
                {
                    // Structural object type: check field presence and types (extensible)
                    match val {
                        Value::Object(obj) => {
                            let obj = Arc::clone(obj);
                            fields.iter().all(|(field_name, field_ty)| {
                                obj.get(field_name).is_some_and(|&val_id| {
                                    self.arena.get(val_id).cloned().is_some_and(
                                        |v| {
                                            self.value_matches_type_expr(
                                                &v, *field_ty,
                                            )
                                        },
                                    )
                                })
                            })
                        }
                        _ => false,
                    }
                } else if let Some(resolved_fields) =
                    self.resolve_object_alias_fields(ty)
                {
                    // Named object alias type: check field presence and types
                    match val {
                        Value::Object(obj) => self
                            .object_matches_resolved_fields(
                                obj.as_ref(),
                                &resolved_fields,
                            ),
                        _ => false,
                    }
                } else if let Some((params, ret)) = self.type_exprs.fn_parts(ty)
                {
                    // Function type: check if value is a function/closure
                    let params = params.clone();
                    self.fn_value_matches(val, &params, ret)
                } else if let Some(type_id) = self.type_exprs.base_type(ty) {
                    // Parameterized types: compare stored type args with expected
                    let type_args = self.type_exprs.type_args(ty).cloned();
                    match (type_id, type_args.as_ref().map(SmallVec::as_slice))
                    {
                        (TypeId::ARRAY, Some(&[expected_elem])) => match val {
                            Value::Array(actual_elem, _) => {
                                self.type_exprs.eq(*actual_elem, expected_elem)
                            }
                            _ => false,
                        },
                        (TypeId::MAP, Some(&[expected_k, expected_v])) => {
                            match val {
                                Value::Map(actual_k, actual_v, _) => {
                                    self.type_exprs.eq(*actual_k, expected_k)
                                        && self
                                            .type_exprs
                                            .eq(*actual_v, expected_v)
                                }
                                _ => false,
                            }
                        }
                        _ => {
                            // Check if this is an alias; if so, expand with type args
                            self.expand_alias_and_match(val, type_id, type_args)
                        }
                    }
                } else {
                    false
                }
            }
        }
    }

    /// Check if a function/closure value matches a function type.
    ///
    /// For `Closure` and `Function` values, we check arity and any annotated
    /// param/return types. For opaque callables (`ClassMethodFn`, `ModuleFn`,
    /// `ModuleConst`, `PartialApp`), we trust the typechecker, which has
    /// already validated the signature at the declaration/return/call site.
    ///
    /// Every call site of this helper is a static guarantee position:
    /// function types cannot appear in `is` patterns (the typechecker emits
    /// `TypeError::FnTypeInPattern`), so runtime type narrowing never needs
    /// to introspect an opaque callable's signature.
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
            Value::PartialApp { .. }
            | Value::ModuleFn { .. }
            | Value::ModuleConst { .. }
            | Value::ClassMethodFn { .. } => true,
            _ => typechecked!("fn_value_matches", "Callable"),
        }
    }

    /// Check if a value matches an AST type expression containing wildcards.
    ///
    /// Supports nested wildcards like `Array[Option[_]]` by recursively
    /// checking type arguments.
    pub(super) fn value_matches_ast_type_with_wildcards(
        &mut self,
        val: &Value,
        ast_id: AstTypeExprId,
    ) -> Result<bool> {
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().unwrap_or_else(|| {
                typechecked!("type expr", "valid AstTypeExprId")
            });
        match ast_ty {
            // Wildcard alone matches anything
            AstTypeExpr::Wildcard => Ok(true),
            // App(name, args): check base type and recursively check type args
            AstTypeExpr::App(name, ast_args) => {
                let type_id = match self.registry.lookup(&name) {
                    Some(id) => id,
                    None => invariant!("AST type to be known"),
                };
                // Extract value's type expression based on container type
                Ok(match (type_id, val) {
                    (TypeId::ARRAY, Value::Array(elem_ty, _)) => {
                        ast_args.first().is_none_or(|&arg| {
                            self.type_expr_matches_ast_with_wildcards(
                                *elem_ty, arg,
                            )
                        })
                    }
                    (TypeId::MAP, Value::Map(k_ty, v_ty, _)) => {
                        ast_args.first().is_none_or(|&k_arg| {
                            self.type_expr_matches_ast_with_wildcards(
                                *k_ty, k_arg,
                            )
                        }) && ast_args.get(1).is_none_or(|&v_arg| {
                            self.type_expr_matches_ast_with_wildcards(
                                *v_ty, v_arg,
                            )
                        })
                    }
                    (TypeId::TUPLE, Value::Tuple(ty_expr, _)) => self
                        .type_expr_matches_ast_with_wildcards(*ty_expr, ast_id),
                    (_, Value::Tagged(ty_expr, ..)) => {
                        self.type_exprs.base_type(*ty_expr) == Some(type_id)
                            && self.type_args_match_ast(*ty_expr, &ast_args)
                    }
                    _ => false,
                })
            }
            // Named type without args shouldn't have wildcards
            AstTypeExpr::Named(_) => {
                invariant!("named type should resolve without wildcards")
            }
            // VarApp with wildcards: type param application; can't match at runtime
            AstTypeExpr::VarApp(..) => Ok(false),
            // Other cases shouldn't have unresolvable wildcards
            _ => invariant!("unexpected AST type with wildcards"),
        }
    }

    /// Check if a runtime type expression matches an AST type with wildcards.
    ///
    /// Returns `true` if `ty` matches the AST type, treating `_` as "any type".
    fn type_expr_matches_ast_with_wildcards(
        &self,
        ty: TypeExprId,
        ast_id: AstTypeExprId,
    ) -> bool {
        let ast_ty =
            self.ast.get_type_expr(ast_id).cloned().unwrap_or_else(|| {
                typechecked!("type expr", "valid AstTypeExprId")
            });
        match ast_ty {
            AstTypeExpr::Wildcard => true,
            AstTypeExpr::Named(name) => {
                self.registry.lookup(&name).is_some_and(|expected| {
                    self.type_exprs.base_type(ty) == Some(expected)
                        && self.type_exprs.type_args(ty).is_none()
                })
            }
            AstTypeExpr::App(name, ast_args) => {
                self.registry.lookup(&name).is_some_and(|expected| {
                    self.type_exprs.base_type(ty) == Some(expected)
                        && self.type_args_match_ast(ty, &ast_args)
                })
            }
            // Tuple, Union, Fn, Object: not supported with wildcards yet
            _ => false,
        }
    }

    /// Check if a type expression's arguments match AST type arguments with wildcards.
    fn type_args_match_ast(
        &self,
        ty: TypeExprId,
        ast_args: &SmallVec<[AstTypeExprId; 2]>,
    ) -> bool {
        self.type_exprs
            .type_args(ty)
            .map_or(ast_args.is_empty(), |args| {
                args.len() == ast_args.len()
                    && args.iter().zip(ast_args.iter()).all(
                        |(&ty_arg, &ast_arg)| {
                            self.type_expr_matches_ast_with_wildcards(
                                ty_arg, ast_arg,
                            )
                        },
                    )
            })
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

    /// Get object fields from an alias type expression.
    ///
    /// Returns `Some((fields, type_params, type_args))` if `ty` resolves to
    /// an object alias type, `None` otherwise. The type_args are from the type
    /// expression (e.g., `[Int]` for `Box[Int]`).
    fn get_alias_object_fields(
        &self,
        ty: TypeExprId,
    ) -> Option<(
        SmallVec<[(StringId, AstTypeExprId); 4]>,
        SmallVec<[StringId; 2]>,
        Option<SmallVec<[TypeExprId; 2]>>,
    )> {
        let base_ty = self.type_exprs.base_type(ty)?;
        let type_args = self.type_exprs.type_args(ty).cloned();
        self.registry.get_def(base_ty).and_then(|def| match def {
            crate::value::TypeDef::Alias {
                target,
                type_params,
                ..
            } => {
                // Check if the target is an object type
                let target_expr = self.ast.get_type_expr(*target)?;
                match target_expr {
                    crate::ast::AstTypeExpr::Object(fields) => {
                        Some((fields.clone(), type_params.clone(), type_args))
                    }
                    _ => None,
                }
            }
            _ => None,
        })
    }

    /// Get resolved object fields for a type expression.
    ///
    /// Resolves AST field types with type parameter substitution.
    /// Returns `None` if `ty` is not an alias to an object type.
    pub(super) fn resolve_object_alias_fields(
        &mut self,
        ty: TypeExprId,
    ) -> Option<IndexMap<StringId, TypeExprId>> {
        // Get alias object definition and extract what we need
        let (fields, type_params, type_args) =
            self.get_alias_object_fields(ty)?;

        // Build substitution map
        let subst = Self::build_subst(&type_params, type_args.as_ref());

        // Resolve each field type with substitution
        fields
            .iter()
            .map(|(fname, ast_ty)| {
                self.resolve_ast_type_with_subst(*ast_ty, &subst)
                    .ok()
                    .map(|resolved| (*fname, resolved))
            })
            .collect()
    }

    /// Validate an object against object alias field requirements.
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
            obj.get(fname_id).map_or_else(
                || typechecked!("object field", "present"),
                |val_id| {
                    self.arena.get(*val_id).cloned().map_or(Ok(()), |val| {
                        self.validate_field_type(&val, *fty, span, ctx)
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
        span: Span,
        ctx: Option<&str>,
    ) -> Result<()> {
        // Check for nested object alias (get resolved fields)
        let nested = self.resolve_object_alias_fields(expected_ty);
        if let Some(nested_fields) = nested {
            // Nested object: recursively validate
            match val {
                Value::Object(nested_obj) => self.validate_object_fields(
                    nested_obj.as_ref(),
                    &nested_fields,
                    span,
                    ctx,
                ),
                _ => typechecked!("object field", "Object"),
            }
        } else if self.value_matches_type_expr(val, expected_ty) {
            Ok(())
        } else {
            typechecked!("field type", "matches declaration")
        }
    }

    /// Validate that a value conforms to an expected type.
    ///
    /// For object alias types, validates extensible-record style: the object must
    /// have at least the declared fields with correct types (extra fields OK).
    pub(super) fn validate_type(
        &mut self,
        val: &Value,
        expected_ty: TypeExprId,
        span: Span,
    ) -> Result<()> {
        // Check if this is an object alias type and get resolved fields
        let resolved_fields = self.resolve_object_alias_fields(expected_ty);

        if let Some(fields) = resolved_fields {
            match val {
                Value::Object(obj) => self.validate_object_fields(
                    obj.as_ref(),
                    &fields,
                    span,
                    None,
                ),
                _ => typechecked!("object value", "Object"),
            }
        } else if self.value_matches_type_expr(val, expected_ty) {
            Ok(())
        // Allow Int -> Word coercion (type checker validates non-negative)
        } else if let (Value::Int(_), Some(TypeId::WORD)) =
            (val, self.type_exprs.base_type(expected_ty))
        {
            Ok(())
        // Allow Int -> Float coercion (widening)
        } else if let (Value::Int(_), Some(TypeId::FLOAT)) =
            (val, self.type_exprs.base_type(expected_ty))
        {
            Ok(())
        } else {
            typechecked!("value type", "matches declaration")
        }
    }
}
