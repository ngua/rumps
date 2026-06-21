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
        Self::register(
            methods,
            i,
            "into",
            MethodAbi::Convert,
            Builtin::Fixed(Impl::Sync(Self::into)),
        );
    }
}

impl Into {
    pub(crate) fn into(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let id = args[0];
        let target = ctx.convert_target()?;
        let edge = ctx.approved_edge();
        let span = ctx.span();
        let mut vals = ctx.vals();
        match edge {
            Some(meta) => Ok(vals.id_with_meta(id, meta)),
            None => {
                let target_ty = vals.ty(target);
                let val = vals.value(id)?.clone();
                Self::conv_value(&mut vals, &val, &target_ty, span)
                    .map(|v| vals.add_typed(v, target))
            }
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

    pub(crate) fn conv_value(
        vals: &mut Values<'_, '_, '_, '_>,
        val: &Value,
        target: &Ty,
        span: Span,
    ) -> Result<Payload> {
        match (&val.payload, target) {
            (_, Ty::String) => {
                let s = Self::str_value(vals, val)?;
                let id = vals.intern(&s);
                Ok(Payload::String(id))
            }
            (_, Ty::Json) => {
                Ok(Payload::Json(Arc::new(Self::json_value(vals, val)?)))
            }
            (Payload::Variant { tag: idx, .. }, Ty::Int)
                if vals
                    .value_variant_base_type(val)
                    .is_some_and(|ty| ty == TypeId::DATA_STATUS) =>
            {
                Ok(Payload::Int(match idx {
                    0 => 0,
                    1 => 1,
                    2 => 10,
                    3 => 11,
                    _ => typechecked!("DataStatus AS Int", "valid variant"),
                }))
            }
            (Payload::Variant { .. }, Ty::Int) => {
                typechecked!("DataStatus AS Int", "DataStatus")
            }
            (Payload::Variant { vals: ids, .. }, Ty::FilePath)
                if vals
                    .value_variant_base_type(val)
                    .is_some_and(|ty| ty == TypeId::PATH) =>
            {
                ids.first()
                    .and_then(|id| vals.payload(*id).ok().cloned())
                    .ok_or_else(|| {
                        typechecked!("Path AS FilePath", "valid Path")
                    })
            }
            (Payload::Variant { .. }, Ty::FilePath) => {
                typechecked!("Path AS FilePath", "Path")
            }
            _ => Self::conv_payload(vals, &val.payload, target, span),
        }
    }

    fn conv_payload(
        vals: &mut Values<'_, '_, '_, '_>,
        val: &Payload,
        target: &Ty,
        span: Span,
    ) -> Result<Payload> {
        match (val, target) {
            (Payload::Int(_), Ty::Int)
            | (Payload::Word(_), Ty::Word)
            | (Payload::Float(_), Ty::Float)
            | (Payload::Bool(_), Ty::Bool)
            | (Payload::Char(_), Ty::Char)
            | (Payload::String(_), Ty::String)
            | (Payload::FilePath(_), Ty::FilePath) => Ok(val.clone()),
            (Payload::Int(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }
            (Payload::Word(n), Ty::Int) => Ok(Payload::Int(*n as i64)),
            (Payload::Word(n), Ty::Float) => {
                Ok(Payload::Float(OrderedFloat(*n as f64)))
            }
            (Payload::Float(f), Ty::Int) => Ok(Payload::Int(f.0 as i64)),
            (Payload::Bool(b), Ty::Int) => {
                Ok(Payload::Int(if *b { 1 } else { 0 }))
            }
            (_, Ty::String) => {
                let s = Self::str_payload(vals, val)?;
                let id = vals.intern(&s);
                Ok(Payload::String(id))
            }
            (_, Ty::Json) => {
                Ok(Payload::Json(Arc::new(Self::json(vals, val)?)))
            }
            (Payload::String(sid), Ty::FilePath) => Ok(Payload::FilePath(*sid)),
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
                        vals.add_typed(
                            Payload::Int(n),
                            RuntimeTyId::from(TyArena::INT),
                        )
                    })
                    .collect();
                Ok(Payload::Array(Arc::new(elems)))
            }
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
                let src_name = Self::payload_name(val);
                let tgt_name = Self::ty_name(target);
                Err(Error::runtime_type(
                    span,
                    format!("cannot cast {src_name} as {tgt_name}"),
                ))
            }
            _ => Ok(val.clone()),
        }
    }

    pub(super) fn payload_name(val: &Payload) -> String {
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

    fn str_payload(
        vals: &mut Values<'_, '_, '_, '_>,
        v: &Payload,
    ) -> Result<String> {
        match v {
            Payload::String(id) | Payload::FilePath(id) => {
                vals.str(*id).map(ToOwned::to_owned)
            }
            _ => Display::fmt(vals, v),
        }
    }

    fn str_value(
        vals: &mut Values<'_, '_, '_, '_>,
        v: &Value,
    ) -> Result<String> {
        match &v.payload {
            Payload::String(id) | Payload::FilePath(id) => {
                vals.str(*id).map(ToOwned::to_owned)
            }
            _ => Display::fmt_value(vals, v),
        }
    }

    pub(crate) fn json_value(
        vals: &mut Values<'_, '_, '_, '_>,
        v: &Value,
    ) -> Result<serde_json::Value> {
        match &v.payload {
            Payload::Variant { tag, vals: ids } => Self::json_variant(
                vals,
                vals.value_variant_base_type(v),
                *tag,
                ids,
            ),
            payload => Self::json(vals, payload),
        }
    }

    pub(crate) fn json(
        vals: &mut Values<'_, '_, '_, '_>,
        v: &Payload,
    ) -> Result<serde_json::Value> {
        Ok(match v {
            Payload::Unit => serde_json::Value::Null,
            Payload::Bool(b) => serde_json::Value::Bool(*b),
            Payload::Int(n) => serde_json::json!(*n),
            Payload::Word(n) => serde_json::json!(*n),
            Payload::Float(f) => serde_json::json!(f.0),
            Payload::Char(c) => serde_json::Value::String(c.to_string()),
            Payload::String(id) | Payload::FilePath(id) => {
                serde_json::Value::String(vals.str(*id)?.to_owned())
            }
            Payload::Array(arr) | Payload::Tuple(arr) => {
                let elems =
                    arr.iter().try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::json_value(vals, &v)?);
                        Ok::<Vec<serde_json::Value>, Error>(acc)
                    })?;
                serde_json::Value::Array(elems)
            }
            Payload::Object(obj) => {
                let map = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = vals.str(*k)?.to_owned();
                        let val = vals.value(*vid)?.clone();
                        Self::json_value(vals, &val).map(|val| (key, val))
                    })
                    .process_results(|iter| iter.collect())?;
                serde_json::Value::Object(map)
            }
            Payload::Variant { tag, vals: ids } => {
                Self::json_variant(vals, None, *tag, ids)?
            }
            Payload::Map(entries) => {
                let map = entries
                    .entries()
                    .into_iter()
                    .map(|(k, vid)| {
                        let key = vals.value(k)?.clone();
                        let val = vals.value(vid)?.clone();
                        let key = Self::json_value(vals, &key)?.to_string();
                        Self::json_value(vals, &val).map(|val| (key, val))
                    })
                    .process_results(|iter| iter.collect())?;
                serde_json::Value::Object(map)
            }
            Payload::Time(t) => serde_json::Value::String(t.to_rfc3339()),
            Payload::Json(j) => j.as_ref().clone(),
            Payload::Regex(idx) => {
                let pattern = vals.regex_pattern(*idx).unwrap_or("?");
                serde_json::Value::String(pattern.to_owned())
            }
            Payload::Range {
                start,
                end,
                inclusive,
            } => serde_json::json!({
                "start": *start,
                "end": *end,
                "inclusive": *inclusive
            }),
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
                let name = vals.str(*name_id)?.to_owned();
                let subs =
                    sub_ids.iter().try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::json_value(vals, &v)?);
                        Ok::<Vec<serde_json::Value>, Error>(acc)
                    })?;
                serde_json::json!({
                    "ref": format!("{prefix}{name}"),
                    "subscripts": subs
                })
            }
        })
    }

    fn json_variant(
        vals: &mut Values<'_, '_, '_, '_>,
        type_id: Option<TypeId>,
        tag: u8,
        ids: &[ValueId],
    ) -> Result<serde_json::Value> {
        if type_id == Some(TypeId::OPTION) && tag == 0 {
            Ok(serde_json::Value::Null)
        } else if type_id == Some(TypeId::OPTION) && tag == 1 {
            ids.first().map_or(Ok(serde_json::Value::Null), |id| {
                let v = vals.value(*id)?.clone();
                Self::json_value(vals, &v)
            })
        } else {
            let payload_json = if ids.is_empty() {
                serde_json::Value::Null
            } else if ids.len() == 1 {
                ids.first().map_or(Ok(serde_json::Value::Null), |id| {
                    let v = vals.value(*id)?.clone();
                    Self::json_value(vals, &v)
                })?
            } else {
                let items =
                    ids.iter().try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::json_value(vals, &v)?);
                        Ok::<Vec<serde_json::Value>, Error>(acc)
                    })?;
                serde_json::Value::Array(items)
            };
            let ty_name = type_id
                .and_then(|type_id| vals.type_name(type_id))
                .unwrap_or("Variant")
                .to_owned();
            let variant = type_id
                .and_then(|type_id| vals.variant_name(type_id, tag))
                .map_or_else(|| tag.to_string(), ToOwned::to_owned);
            Ok(serde_json::json!({
                "type": ty_name,
                "variant": variant,
                "payload": payload_json
            }))
        }
    }
}
