use super::*;

/// Fallible conversion: `T: TryInto[U]` means `T` might convert to `U`.
///
/// External `read` into a private `newtype` representation requires an
/// explicit `TryInto` instance in the defining module. `type visibility`
/// controls the `newtype` name, while `repr visibility` controls automatic
/// representation access.
pub(crate) struct TryInto;

impl Class for TryInto {
    const ID: ClassId = ClassId::TRY_INTO;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "try-into",
            MethodAbi::Convert,
            Builtin::Fixed(Impl::Sync(Self::try_into)),
        );
    }
}

impl TryInto {
    pub(crate) fn try_into(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let id = args[0];
        let target = ctx.convert_target()?;
        let edge = ctx.approved_edge();
        let mut vals = ctx.vals();
        match edge {
            Some(meta) => {
                let id = vals.id_with_meta(id, meta);
                Ok(vals.result_ok(id))
            }
            None => {
                let target_ty = vals.ty(target);
                let v = vals.value(id)?.clone();
                Self::try_value(&mut vals, id, &v, target, &target_ty)
            }
        }
    }

    /// Check if a value's runtime type matches the target type.
    fn types_match(val: &Payload, target: &Ty) -> bool {
        matches!(
            (val, target),
            (Payload::Bool(_), Ty::Bool)
                | (Payload::Int(_), Ty::Int)
                | (Payload::Word(_), Ty::Word)
                | (Payload::Float(_), Ty::Float)
                | (Payload::Char(_), Ty::Char)
                | (Payload::String(_), Ty::String)
                | (Payload::FilePath(_), Ty::FilePath)
                | (Payload::Json(_), Ty::Json)
                | (Payload::Unit, Ty::Unit)
        )
    }

    /// Get human-readable JSON type name.
    fn json_type_name(j: &serde_json::Value) -> &'static str {
        match j {
            serde_json::Value::Null => "null",
            serde_json::Value::Bool(_) => "boolean",
            serde_json::Value::Number(_) => "number",
            serde_json::Value::String(_) => "string",
            serde_json::Value::Array(_) => "array",
            serde_json::Value::Object(_) => "object",
        }
    }

    fn try_value(
        vals: &mut Values<'_, '_, '_, '_>,
        id: ValueId,
        v: &Value,
        target: RuntimeTyId,
        target_ty: &Ty,
    ) -> Result<ValueId> {
        match (&v.payload, target_ty) {
            _ if Self::types_match(&v.payload, target_ty) => {
                Ok(vals.result_ok(id))
            }
            (Payload::String(sid), Ty::Int) => {
                let s = vals.str(*sid)?.to_owned();
                Ok(match s.parse::<i64>() {
                    Ok(n) => {
                        let id = vals.add_typed(
                            Payload::Int(n),
                            RuntimeTyId::from(TyArena::INT),
                        );
                        vals.result_ok(id)
                    }
                    Err(_) => Self::result_err(
                        vals,
                        format!("invalid integer: {s}"),
                    ),
                })
            }
            (Payload::String(sid), Ty::Float) => {
                let s = vals.str(*sid)?.to_owned();
                Ok(match s.parse::<f64>() {
                    Ok(n) => {
                        let id = vals.add_typed(
                            Payload::Float(OrderedFloat(n)),
                            RuntimeTyId::from(TyArena::FLOAT),
                        );
                        vals.result_ok(id)
                    }
                    Err(_) => Self::result_err(
                        vals,
                        format!("invalid float: {s}"),
                    ),
                })
            }
            (Payload::String(sid), Ty::Word) => {
                let s = vals.str(*sid)?.to_owned();
                Ok(match s.parse::<usize>() {
                    Ok(n) => {
                        let id = vals.add_typed(
                            Payload::Word(n),
                            RuntimeTyId::from(TyArena::WORD),
                        );
                        vals.result_ok(id)
                    }
                    Err(_) => Self::result_err(
                        vals,
                        format!("invalid unsigned integer: {s}"),
                    ),
                })
            }
            (Payload::Int(n), Ty::Bool) => Ok(match *n {
                0 => {
                    let id = vals.add_typed(
                        Payload::Bool(false),
                        RuntimeTyId::from(TyArena::BOOL),
                    );
                    vals.result_ok(id)
                }
                1 => {
                    let id = vals.add_typed(
                        Payload::Bool(true),
                        RuntimeTyId::from(TyArena::BOOL),
                    );
                    vals.result_ok(id)
                }
                _ => Self::result_err(
                    vals,
                    format!("expected 0 or 1 for Bool, got {n}"),
                ),
            }),
            (Payload::Int(n), Ty::Word) => Ok(if *n >= 0 {
                let id = vals.add_typed(
                    Payload::Word(*n as usize),
                    RuntimeTyId::from(TyArena::WORD),
                );
                vals.result_ok(id)
            } else {
                Self::result_err(
                    vals,
                    format!("expected non-negative Int for Word, got {n}"),
                )
            }),
            (Payload::Json(j), Ty::Bool) => Ok(match &**j {
                serde_json::Value::Bool(b) => {
                    let id = vals.add_typed(
                        Payload::Bool(*b),
                        RuntimeTyId::from(TyArena::BOOL),
                    );
                    vals.result_ok(id)
                }
                serde_json::Value::Null => {
                    Self::result_err(vals, "expected Bool, got null")
                }
                _ => Self::result_err(
                    vals,
                    format!("expected Bool, got {}", Self::json_type_name(j)),
                ),
            }),
            (Payload::Json(j), Ty::Int) => Ok(match &**j {
                serde_json::Value::Number(n) => match n.as_i64() {
                    Some(n) => {
                        let id = vals.add_typed(
                            Payload::Int(n),
                            RuntimeTyId::from(TyArena::INT),
                        );
                        vals.result_ok(id)
                    }
                    None => Self::result_err(
                        vals,
                        format!("expected Int, got non-integer number {n}"),
                    ),
                },
                serde_json::Value::Null => {
                    Self::result_err(vals, "expected Int, got null")
                }
                _ => Self::result_err(
                    vals,
                    format!("expected Int, got {}", Self::json_type_name(j)),
                ),
            }),
            (Payload::Json(j), Ty::Float) => Ok(match &**j {
                serde_json::Value::Number(n) => match n.as_f64() {
                    Some(n) => {
                        let id = vals.add_typed(
                            Payload::Float(OrderedFloat(n)),
                            RuntimeTyId::from(TyArena::FLOAT),
                        );
                        vals.result_ok(id)
                    }
                    None => Self::result_err(
                        vals,
                        format!("expected Float, got invalid number {n}"),
                    ),
                },
                serde_json::Value::Null => {
                    Self::result_err(vals, "expected Float, got null")
                }
                _ => Self::result_err(
                    vals,
                    format!("expected Float, got {}", Self::json_type_name(j)),
                ),
            }),
            (Payload::Json(j), Ty::String) => Ok(match &**j {
                serde_json::Value::String(s) => {
                    let sid = vals.intern(s);
                    let id = vals.add_typed(
                        Payload::String(sid),
                        RuntimeTyId::from(TyArena::STRING),
                    );
                    vals.result_ok(id)
                }
                serde_json::Value::Null => {
                    Self::result_err(vals, "expected String, got null")
                }
                _ => Self::result_err(
                    vals,
                    format!("expected String, got {}", Self::json_type_name(j)),
                ),
            }),
            (Payload::Int(n), Ty::DataStatus) => Ok(match *n {
                0..=1 | 10..=11 => {
                    let tag = match *n {
                        0 => 0,
                        1 => 1,
                        10 => 2,
                        11 => 3,
                        _ => typechecked!("DataStatus", "valid tag"),
                    };
                    let id = vals.add_typed(
                        Payload::Variant {
                            tag,
                            vals: SmallVec::new(),
                        },
                        target,
                    );
                    vals.result_ok(id)
                }
                _ => Self::result_err(
                    vals,
                    format!(
                        "invalid DataStatus value: {n} (expected 0, 1, 10, or 11)"
                    ),
                ),
            }),
            _ => {
                let src = Into::payload_name(&v.payload);
                let tgt = Into::ty_name(target_ty);
                Ok(Self::result_err(
                    vals,
                    format!("cannot read {src} as {tgt}"),
                ))
            }
        }
    }

    fn result_err(
        vals: &mut Values<'_, '_, '_, '_>,
        msg: impl AsRef<str>,
    ) -> ValueId {
        let sid = vals.intern(msg.as_ref());
        let id = vals.add_typed(
            Payload::String(sid),
            RuntimeTyId::from(TyArena::STRING),
        );
        vals.result_err(id)
    }
}
