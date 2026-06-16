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
            MethodFn::Convert(Self::try_into),
        );
    }
}

impl TryInto {
    /// Try to convert a value to the target type.
    ///
    /// Returns `Result[U, String]` where `Err` contains an error message.
    /// Mirrors the `read` operator behavior exactly.
    pub(crate) fn try_into(
        ctx: &mut ClassCtx<'_>,
        val: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        match (val, target) {
            // Same type: identity conversion always succeeds
            _ if Self::types_match(val, target) => {
                Ok(Self::make_result_ok(ctx, val.clone()))
            }

            // String -> Int
            (Payload::String(sid), Ty::Int) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<i64>() {
                    Ok(n) => Self::make_result_ok(ctx, Payload::Int(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid integer: {s}"),
                    ),
                })
            }

            // String -> Float
            (Payload::String(sid), Ty::Float) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<f64>() {
                    Ok(n) => Self::make_result_ok(
                        ctx,
                        Payload::Float(OrderedFloat(n)),
                    ),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid float: {s}"),
                    ),
                })
            }

            // String -> Word
            (Payload::String(sid), Ty::Word) => {
                let s = ctx.arena.get_str(*sid).unwrap_or("").to_owned();
                Ok(match s.parse::<usize>() {
                    Ok(n) => Self::make_result_ok(ctx, Payload::Word(n)),
                    Err(_) => Self::make_result_err(
                        ctx,
                        &format!("invalid unsigned integer: {s}"),
                    ),
                })
            }

            // Int -> Bool (strict: only 0 and 1)
            (Payload::Int(n), Ty::Bool) => Ok(match *n {
                0 => Self::make_result_ok(ctx, Payload::Bool(false)),
                1 => Self::make_result_ok(ctx, Payload::Bool(true)),
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected 0 or 1 for Bool, got {n}"),
                ),
            }),

            // Int -> Word (must be non-negative)
            (Payload::Int(n), Ty::Word) => Ok(if *n >= 0 {
                Self::make_result_ok(ctx, Payload::Word(*n as usize))
            } else {
                Self::make_result_err(
                    ctx,
                    &format!("expected non-negative Int for Word, got {n}"),
                )
            }),

            // Json -> Bool
            (Payload::Json(j), Ty::Bool) => Ok(match &**j {
                serde_json::Value::Bool(b) => {
                    Self::make_result_ok(ctx, Payload::Bool(*b))
                }
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Bool, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Bool, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> Int
            (Payload::Json(j), Ty::Int) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_i64()
                    .map(|i| Self::make_result_ok(ctx, Payload::Int(i)))
                    .unwrap_or_else(|| {
                        Self::make_result_err(
                            ctx,
                            &format!(
                                "expected Int, got non-integer number {n}"
                            ),
                        )
                    }),
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Int, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Int, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> Float
            (Payload::Json(j), Ty::Float) => Ok(match &**j {
                serde_json::Value::Number(n) => n
                    .as_f64()
                    .map(|f| {
                        Self::make_result_ok(
                            ctx,
                            Payload::Float(OrderedFloat(f)),
                        )
                    })
                    .unwrap_or_else(|| {
                        Self::make_result_err(
                            ctx,
                            &format!("expected Float, got invalid number {n}"),
                        )
                    }),
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected Float, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!("expected Float, got {}", Self::json_type_name(j)),
                ),
            }),

            // Json -> String
            (Payload::Json(j), Ty::String) => Ok(match &**j {
                serde_json::Value::String(s) => {
                    let id = ctx.arena.intern(s);
                    Self::make_result_ok(ctx, Payload::String(id))
                }
                serde_json::Value::Null => {
                    Self::make_result_err(ctx, "expected String, got null")
                }
                _ => Self::make_result_err(
                    ctx,
                    &format!(
                        "expected String, got {}",
                        Self::json_type_name(j)
                    ),
                ),
            }),

            // T -> Json (jsonify)
            (_, Ty::Json) => Ok(Self::make_result_ok(
                ctx,
                Payload::Json(Arc::new(Into::jsonify(ctx, val))),
            )),

            // Int -> DataStatus (MUMPS @data values: 0, 1, 10, 11 -> variants)
            (Payload::Int(n), Ty::DataStatus) => {
                let (variant_idx, valid) = match *n {
                    0 => (0, true),  // NoData
                    1 => (1, true),  // HasValue
                    10 => (2, true), // HasDescendants
                    11 => (3, true), // Both
                    _ => (0, false),
                };

                Ok(if valid {
                    let status_id = ctx.arena.add_typed(
                        Payload::Variant {
                            tag: variant_idx,
                            vals: SmallVec::new(),
                        },
                        ctx.runtime_types.meta_data_status(),
                        ctx.span,
                    );
                    Self::make_result_ok_id(status_id)
                } else {
                    Self::make_result_err(
                        ctx,
                        &format!(
                            "invalid DataStatus value: {n} (expected 0, 1, 10, or 11)"
                        ),
                    )
                })
            }

            // Unsupported conversion: return Result.Err
            _ => {
                let src = Into::value_type_name(ctx, val);
                let tgt = Into::ty_name(target);
                let msg = format!("cannot read {src} as {tgt}");
                Ok(Self::make_result_err(ctx, &msg))
            }
        }
    }

    pub(crate) fn try_into_value(
        ctx: &mut ClassCtx<'_>,
        val: &Value,
        target: &Ty,
    ) -> Result<Payload> {
        match target {
            Ty::Json => Ok(Self::make_result_ok(
                ctx,
                Payload::Json(Arc::new(Into::jsonify_value(ctx, val))),
            )),
            _ => Self::try_into(ctx, &val.payload, target),
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

    /// Create a `Result.Ok(val)` value.
    fn make_result_ok(ctx: &mut ClassCtx<'_>, val: Payload) -> Payload {
        let val_id = ctx.add(val);
        Self::make_result_ok_id(val_id)
    }

    fn make_result_ok_id(val_id: ValueId) -> Payload {
        Payload::ok(val_id)
    }

    /// Create a `Result.Err(msg)` value.
    fn make_result_err(ctx: &mut ClassCtx<'_>, msg: &str) -> Payload {
        let msg_id = ctx.arena.intern(msg);
        let msg_val_id = ctx.arena.add_typed(
            Payload::String(msg_id),
            ctx.runtime_types.meta_string(),
            ctx.span,
        );
        Payload::err(msg_val_id)
    }
}
