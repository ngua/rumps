use super::*;

/// Infallible conversion: `T: Into[U]` means `T` can be converted to `U`.
///
/// User `Into` instances must not overlap a public `newtype` representation
/// edge. They also cannot expose a private representation edge; use
/// `repr visibility` on the defining `newtype` to control automatic access.
pub(crate) struct Into;

impl Class for Into {
    const ID: ClassId = ClassId::INTO;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "into", MethodFn::Convert(Self::into));
    }
}

impl Into {
    /// Convert a value to the target type.
    ///
    /// Handles all conversions supported by `AS`:
    /// - Numeric widening (`Int -> Float`, `Word -> Int`, etc.)
    /// - `T -> String` (stringify)
    /// - `T -> Json` (jsonify)
    /// - `String -> FilePath`
    /// - `Path -> FilePath`
    /// - `DataStatus -> Int`
    pub(crate) fn into(
        ctx: &mut ClassCtx<'_>,
        val: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        match (val, target) {
            // Identity casts
            (Payload::Int(_), Ty::Int)
            | (Payload::Word(_), Ty::Word)
            | (Payload::Float(_), Ty::Float)
            | (Payload::Bool(_), Ty::Bool)
            | (Payload::Char(_), Ty::Char)
            | (Payload::String(_), Ty::String)
            | (Payload::FilePath(_), Ty::FilePath) => Ok(val.clone()),

            // Int -> Float (widen)
            (Payload::Int(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }

            // Word -> Int (always safe)
            (Payload::Word(n), Ty::Int) => Ok(Payload::Int(*n as i64)),

            // Word -> Float (widen)
            (Payload::Word(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }

            // Float -> Int (truncate)
            (Payload::Float(f), Ty::Int) => Ok(Payload::Int(f.0 as i64)),

            // Bool -> Int
            (Payload::Bool(b), Ty::Int) => {
                Ok(Payload::Int(if *b { 1 } else { 0 }))
            }

            // T -> String (stringify)
            (_, Ty::String) => {
                let s = Self::stringify(ctx, val);
                let id = ctx.arena.intern(&s);
                Ok(Payload::String(id))
            }

            // T -> Json (jsonify)
            (_, Ty::Json) => {
                Ok(Payload::Json(Arc::new(Self::jsonify(ctx, val))))
            }

            // String -> FilePath
            (Payload::String(sid), Ty::FilePath) => Ok(Payload::FilePath(*sid)),

            // Range -> Array[Int]
            (
                Payload::Range {
                    start,
                    end,
                    inclusive,
                },
                Ty::Array(elem),
            ) if *elem == TyArena::INT => {
                let elems = Range::vals(*start, *end, *inclusive)
                    .map(|n| {
                        ctx.arena.add_typed(
                            Payload::Int(n),
                            ctx.runtime_types.meta_int(),
                            ctx.span,
                        )
                    })
                    .collect();
                Ok(Payload::Array(Arc::new(elems)))
            }

            // Storable narrowing: `Storable AS T` where T is a Storable member.
            // This is the ONLY case that requires runtime type checking; all other
            // casts are validated by the type checker. If the value doesn't match
            // the target type, we return a runtime error.
            _ if matches!(
                target,
                Ty::Bool
                    | Ty::Int
                    | Ty::Float
                    | Ty::Char
                    | Ty::String
                    | Ty::Json
            ) && Self::is_storable_mismatch(val, target) =>
            {
                let src_name = Self::value_type_name(ctx, val);
                let tgt_name = Self::ty_name(target);
                Err(Error::runtime_type(
                    ctx.span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }

            // Compound types: identity cast only
            (_, _) => Ok(val.clone()),
        }
    }

    pub(crate) fn into_value(
        ctx: &mut ClassCtx<'_>,
        val: &Value,
        target: &Ty,
    ) -> Result<Payload> {
        match (&val.payload, target) {
            (_, Ty::String) => {
                let s = Self::coerce_to_str_value(ctx, val);
                let id = ctx.arena.intern(&s);
                Ok(Payload::String(id))
            }
            (_, Ty::Json) => {
                Ok(Payload::Json(Arc::new(Self::jsonify_value(ctx, val))))
            }
            (Payload::Variant { tag: idx, .. }, Ty::Int)
                if ctx
                    .value_variant_base_type(val)
                    .is_some_and(|ty| ty == TypeId::DATA_STATUS) =>
            {
                let mumps_val = match idx {
                    0 => 0,
                    1 => 1,
                    2 => 10,
                    3 => 11,
                    _ => typechecked!("DataStatus AS Int", "valid variant"),
                };
                Ok(Payload::Int(mumps_val))
            }
            (Payload::Variant { .. }, Ty::Int) => {
                typechecked!("DataStatus AS Int", "DataStatus")
            }
            (Payload::Variant { vals, .. }, Ty::FilePath)
                if ctx
                    .value_variant_base_type(val)
                    .is_some_and(|ty| ty == TypeId::PATH) =>
            {
                Ok(vals
                    .first()
                    .and_then(|id| ctx.arena.payload(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("Path AS FilePath", "valid Path")
                    }))
            }
            (Payload::Variant { .. }, Ty::FilePath) => {
                typechecked!("Path AS FilePath", "Path")
            }
            _ => Self::into(ctx, &val.payload, target),
        }
    }

    /// Check if a value is a Storable that doesn't match the target type.
    fn is_storable_mismatch(val: &Payload, target: &Ty) -> bool {
        match (val, target) {
            (Payload::Bool(_), Ty::Bool)
            | (Payload::Int(_), Ty::Int)
            | (Payload::Float(_), Ty::Float)
            | (Payload::Char(_), Ty::Char)
            | (Payload::String(_), Ty::String)
            | (Payload::Json(_), Ty::Json) => false,
            // Payload is a Storable type but doesn't match target
            (
                Payload::Bool(_)
                | Payload::Int(_)
                | Payload::Float(_)
                | Payload::Char(_)
                | Payload::String(_)
                | Payload::Json(_),
                _,
            ) => true,
            // Not a Storable type at all; don't trigger this branch
            _ => false,
        }
    }

    /// Get a human-readable name for a value's type.
    pub(super) fn value_type_name(
        _ctx: &ClassCtx<'_>,
        val: &Payload,
    ) -> String {
        match val {
            Payload::Unit => "Unit".to_owned(),
            Payload::Bool(_) => "Bool".to_owned(),
            Payload::Int(_) => "Int".to_owned(),
            Payload::Word(_) => "Word".to_owned(),
            Payload::Float(_) => "Float".to_owned(),
            Payload::Char(_) => "Char".to_owned(),
            Payload::String(_) => "String".to_owned(),
            Payload::FilePath(_) => "FilePath".to_owned(),
            Payload::Json(_) => "Json".to_owned(),
            Payload::Array(_) => "Array".to_owned(),
            Payload::Tuple(_) => "Tuple".to_owned(),
            Payload::Object(_) => "Object".to_owned(),
            Payload::Map(_) => "Map".to_owned(),
            Payload::Time(_) => "Time".to_owned(),
            Payload::Regex(_) => "Regex".to_owned(),
            Payload::Range { .. } => "Range".to_owned(),
            Payload::Variant { .. } => "Variant".to_owned(),
            Payload::VariantCtor { .. } => "VariantCtor".to_owned(),
            Payload::Closure { .. } => "Closure".to_owned(),
            Payload::Function { .. } => "Function".to_owned(),
            Payload::ModuleFn { .. } => "ModuleFn".to_owned(),
            Payload::ClassMethodFn { .. } => "ClassMethodFn".to_owned(),
            Payload::PartialApp { .. } => "PartialApp".to_owned(),
            Payload::ModuleConst { .. } => "ModuleConst".to_owned(),
            Payload::LoopContinuation => "Continuation".to_owned(),
            Payload::LoopContinue(_) => "LoopContinue".to_owned(),
            Payload::Ref(is_global, _, _) => {
                if *is_global { "Global" } else { "Local" }.to_owned()
            }
        }
    }

    /// Get a human-readable name for a type.
    pub(super) fn ty_name(ty: &Ty) -> &'static str {
        match ty {
            Ty::Unit => "Unit",
            Ty::Bool => "Bool",
            Ty::Int => "Int",
            Ty::Word => "Word",
            Ty::Float => "Float",
            Ty::Char => "Char",
            Ty::String => "String",
            Ty::FilePath => "FilePath",
            Ty::Json => "Json",
            Ty::Named(_, _) => "Named",
            Ty::Tuple(_) => "Tuple",
            Ty::Object(_) => "Object",
            Ty::Fn(_, _) => "Function",
            Ty::Array(_) => "Array",
            Ty::Option(_) => "Option",
            Ty::Result(_, _) => "Result",
            Ty::Map(_, _) => "Map",
            Ty::Time => "Time",
            Ty::Range => "Range",
            Ty::Ordering => "Ordering",
            Ty::DataStatus => "DataStatus",
            Ty::Path => "Path",
            Ty::Regex => "Regex",
            Ty::RuntimeError => "RuntimeError",
            Ty::Local => "Local",
            Ty::Global => "Global",
            Ty::Union(_, _) => "Union",
            Ty::Var(_) => "Var",
            Ty::Apply(_, _) => "Apply",
            Ty::AssocType(_, _, _) => "AssocType",
            Ty::Unknown => "Unknown",
            Ty::Error => "Error",
        }
    }

    /// Stringify a value to produce raw string content (not quoted).
    fn coerce_to_str(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        match v {
            Payload::String(id) | Payload::FilePath(id) => {
                ctx.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => Display::format(ctx, v),
        }
    }

    /// Stringify a value for `AS String` conversion.
    fn stringify(ctx: &ClassCtx<'_>, v: &Payload) -> String {
        Self::coerce_to_str(ctx, v)
    }

    fn coerce_to_str_value(ctx: &ClassCtx<'_>, v: &Value) -> String {
        match &v.payload {
            Payload::String(id) | Payload::FilePath(id) => {
                ctx.arena.get_str(*id).unwrap_or("").to_owned()
            }
            _ => Display::format_value(ctx, v),
        }
    }

    /// Convert a value to JSON.
    ///
    /// Returns the JSON directly; for the class method wrapper that returns
    /// `Payload::Json`, dispatch to `Into[Json]` via `Into::into`.
    pub(crate) fn jsonify(
        ctx: &ClassCtx<'_>,
        v: &Payload,
    ) -> serde_json::Value {
        match v {
            Payload::Unit => serde_json::Value::Null,
            Payload::Bool(b) => serde_json::Value::Bool(*b),
            Payload::Int(n) => serde_json::json!(*n),
            Payload::Word(n) => serde_json::json!(*n),
            Payload::Float(f) => serde_json::json!(f.0),
            Payload::Char(c) => serde_json::Value::String(c.to_string()),
            Payload::String(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Payload::FilePath(id) => {
                let s = ctx
                    .arena
                    .get_str(*id)
                    .unwrap_or_else(|| invariant!("StringId in arena"));
                serde_json::Value::String(s.to_owned())
            }
            Payload::Array(arr) => {
                let elems: Vec<_> = arr
                    .iter()
                    .map(|id| {
                        ctx.arena
                            .value(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| Self::jsonify_value(ctx, v))
                    .collect();
                serde_json::Value::Array(elems)
            }
            Payload::Tuple(elems) => {
                let items: Vec<_> = elems
                    .iter()
                    .map(|id| {
                        ctx.arena
                            .value(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| Self::jsonify_value(ctx, v))
                    .collect();
                serde_json::Value::Array(items)
            }
            Payload::Object(obj) => {
                let map: serde_json::Map<_, _> = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = ctx
                            .arena
                            .get_str(*k)
                            .unwrap_or_else(|| invariant!("StringId in arena"));
                        let val = ctx
                            .arena
                            .value(*vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key.to_owned(), Self::jsonify_value(ctx, val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            Payload::Variant { tag, vals } => {
                Self::jsonify_variant(ctx, None, *tag, vals)
            }
            Payload::Map(entries) => {
                let map: serde_json::Map<_, _> = entries
                    .entries()
                    .into_iter()
                    .map(|(k, vid)| {
                        let key = ctx
                            .arena
                            .value(k)
                            .map(|v| Self::jsonify_value(ctx, v).to_string())
                            .unwrap_or_else(|| "?".to_owned());
                        let val = ctx
                            .arena
                            .value(vid)
                            .unwrap_or_else(|| invariant!("ValueId in arena"));
                        (key, Self::jsonify_value(ctx, val))
                    })
                    .collect();
                serde_json::Value::Object(map)
            }
            Payload::Time(t) => serde_json::Value::String(t.to_rfc3339()),
            Payload::Json(j) => j.as_ref().clone(),
            Payload::Regex(idx) => {
                let pattern = ctx
                    .regex_cache
                    .get(*idx as usize)
                    .map(|r| r.as_str())
                    .unwrap_or("?");
                serde_json::Value::String(pattern.to_owned())
            }
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                serde_json::json!({
                    "start": *start,
                    "end": *end,
                    "inclusive": *inclusive
                })
            }
            Payload::Closure { .. }
            | Payload::Function { .. }
            | Payload::VariantCtor { .. }
            | Payload::ModuleFn { .. }
            | Payload::ClassMethodFn { .. }
            | Payload::ModuleConst { .. }
            | Payload::PartialApp { .. }
            | Payload::LoopContinuation
            | Payload::LoopContinue(_) => serde_json::Value::Null,
            Payload::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = ctx.arena.get_str(*name_id).unwrap_or("?");
                let subs: Vec<_> = sub_ids
                    .iter()
                    .filter_map(|id| ctx.arena.value(*id))
                    .map(|v| Self::jsonify_value(ctx, v))
                    .collect();
                serde_json::json!({
                    "ref": format!("{prefix}{name}"),
                    "subscripts": subs
                })
            }
        }
    }

    pub(crate) fn jsonify_value(
        ctx: &ClassCtx<'_>,
        v: &Value,
    ) -> serde_json::Value {
        match &v.payload {
            Payload::Variant { tag, vals } => Self::jsonify_variant(
                ctx,
                ctx.value_variant_base_type(v),
                *tag,
                vals,
            ),
            payload => Self::jsonify(ctx, payload),
        }
    }

    fn jsonify_variant(
        ctx: &ClassCtx<'_>,
        type_id: Option<TypeId>,
        tag: u8,
        vals: &[ValueId],
    ) -> serde_json::Value {
        if type_id == Some(TypeId::OPTION) && tag == 0 {
            serde_json::Value::Null
        } else if type_id == Some(TypeId::OPTION) && tag == 1 {
            vals.first()
                .and_then(|id| ctx.arena.value(*id))
                .map(|v| Self::jsonify_value(ctx, v))
                .unwrap_or(serde_json::Value::Null)
        } else {
            let payload_json = if vals.is_empty() {
                serde_json::Value::Null
            } else if vals.len() == 1 {
                vals.first()
                    .and_then(|id| ctx.arena.value(*id))
                    .map(|v| Self::jsonify_value(ctx, v))
                    .unwrap_or(serde_json::Value::Null)
            } else {
                let items: Vec<_> = vals
                    .iter()
                    .map(|id| {
                        ctx.arena
                            .value(*id)
                            .unwrap_or_else(|| invariant!("ValueId in arena"))
                    })
                    .map(|v| Self::jsonify_value(ctx, v))
                    .collect();
                serde_json::Value::Array(items)
            };
            let ty_name = type_id
                .and_then(|type_id| {
                    ctx.registry
                        .type_name(type_id, ctx.arena)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "Variant".to_owned());
            let variant = type_id
                .and_then(|type_id| {
                    ctx.registry
                        .variant_name(type_id, tag, ctx.arena)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| tag.to_string());

            serde_json::json!({
                "type": ty_name,
                "variant": variant,
                "payload": payload_json
            })
        }
    }
}
