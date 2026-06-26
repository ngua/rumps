use super::*;

pub(crate) struct Formattable;

impl Class for Formattable {
    const ID: ClassId = ClassId::FORMATTABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "format",
            MethodAbi::Unary,
            Builtin::Fixed(Impl::Sync(Self::format)),
        );
    }
}

impl Formattable {
    pub(crate) fn format(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let id = args[0];
        let mut vals = ctx.vals();
        let v = vals.value(id)?.clone();
        let s = Self::fmt_value(&mut vals, &v)?;
        let id = vals.intern(&s);
        Ok(vals.add(Payload::String(id)))
    }

    fn fmt(vals: &mut Values<'_, '_, '_, '_>, v: &Payload) -> Result<String> {
        match v {
            Payload::Unit => Ok("Unit".into()),
            Payload::Bool(b) => Ok(b.to_string()),
            Payload::Int(n) => Ok(n.to_string()),
            Payload::Word(n) => Ok(n.to_string()),
            Payload::Float(f) => {
                let s = f.to_string();
                Ok(if s.contains('.') || s.contains('e') || s.contains('E') {
                    s
                } else {
                    format!("{s}.0")
                })
            }
            Payload::Char(c) => Ok(format!("'{c}'")),
            Payload::String(id) => Ok(vals.str(*id)?.to_owned()),
            Payload::FilePath(id) => Ok(vals.str(*id)?.to_owned()),
            Payload::Regex(idx) => {
                let re = vals.regex_pattern(*idx).unwrap_or_else(|| {
                    typechecked!("Formattable", "valid Regex cache index")
                });
                Ok(format!("/{re}/"))
            }
            Payload::Array(elems) => {
                let items = elems
                    .iter()
                    .try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::fmt_value(vals, &v)?);
                        Ok::<Vec<String>, Error>(acc)
                    })?
                    .into_iter()
                    .join(", ");
                Ok(format!("[ {items} ]"))
            }
            Payload::Tuple(elems) => {
                let items = elems
                    .iter()
                    .try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::fmt_value(vals, &v)?);
                        Ok::<Vec<String>, Error>(acc)
                    })?
                    .into_iter()
                    .join(", ");
                let trail = if elems.len() == 1 { "," } else { "" };
                Ok(format!("({items}{trail})"))
            }
            Payload::Object(obj) => {
                let fields = obj
                    .iter()
                    .map(|(k, vid)| {
                        let key = vals.str(*k)?.to_owned();
                        let val = vals.value(*vid)?.clone();
                        Self::fmt_value(vals, &val)
                            .map(|val| format!("{key}: {val}"))
                    })
                    .process_results(|mut iter| iter.join(", "))?;
                Ok(format!("{{ {fields} }}"))
            }
            Payload::Map(entries) => {
                let items = entries
                    .entries()
                    .into_iter()
                    .map(|(k, vid)| {
                        let key = vals.value(k)?.clone();
                        let val = vals.value(vid)?.clone();
                        let key = Self::fmt_value(vals, &key)?;
                        let val = Self::fmt_value(vals, &val)?;
                        Ok::<String, Error>(format!("{key} => {val}"))
                    })
                    .process_results(|mut iter| iter.join(", "))?;
                Ok(format!("{{ {items} }}"))
            }
            Payload::Time(t) => Ok(t.to_rfc3339()),
            Payload::Json(j) => Ok(j.to_string()),
            Payload::Variant { tag, vals: ids } => {
                Self::fmt_variant(vals, None, *tag, ids)
            }
            Payload::VariantCtor { .. } => {
                typechecked!("Formattable", "Formattable (not VariantCtor)")
            }
            Payload::Closure { .. } => {
                typechecked!("Formattable", "Formattable (not Closure)")
            }
            Payload::Function { .. } => {
                typechecked!("Formattable", "Formattable (not Function)")
            }
            Payload::ModuleFn { .. } => {
                typechecked!("Formattable", "Formattable (not ModuleFn)")
            }
            Payload::ClassMethodFn { .. } => {
                typechecked!("Formattable", "Formattable (not ClassMethodFn)")
            }
            Payload::PartialApp { .. } => {
                typechecked!("Formattable", "Formattable (not PartialApp)")
            }
            Payload::ModuleConst { path } => {
                let path_str = path
                    .iter()
                    .map(|id| vals.str(*id).map(ToOwned::to_owned))
                    .process_results(|mut iter| iter.join("."))?;
                Ok(format!("<{path_str}>"))
            }
            Payload::Range {
                start,
                end,
                inclusive,
            } => Ok(if *inclusive {
                format!("{start} ..= {end}")
            } else {
                format!("{start} .. {end}")
            }),
            Payload::LoopContinuation => Ok("<continuation>".into()),
            Payload::LoopContinue(_) => Ok("<loop-continue>".into()),
            Payload::Ref(is_global, name_id, sub_ids) => {
                let prefix = if *is_global { "^" } else { "" };
                let name = vals.str(*name_id)?.to_owned();
                let subs = sub_ids
                    .iter()
                    .try_fold(Vec::new(), |mut acc, id| {
                        let v = vals.value(*id)?.clone();
                        acc.push(Self::fmt_value(vals, &v)?);
                        Ok::<Vec<String>, Error>(acc)
                    })?
                    .into_iter()
                    .join(", ");
                Ok(format!("{prefix}{name}{{{subs}}}"))
            }
        }
    }

    fn fmt_value(
        vals: &mut Values<'_, '_, '_, '_>,
        v: &Value,
    ) -> Result<String> {
        match &v.payload {
            Payload::Variant { tag, vals: ids } => Self::fmt_variant(
                vals,
                vals.value_variant_base_type(v),
                *tag,
                ids,
            ),
            payload => Self::fmt(vals, payload),
        }
    }

    fn fmt_variant(
        vals: &mut Values<'_, '_, '_, '_>,
        type_id: Option<TypeId>,
        tag: u8,
        ids: &[ValueId],
    ) -> Result<String> {
        let ty_name = type_id
            .and_then(|type_id| vals.type_name(type_id))
            .unwrap_or("Variant")
            .to_owned();
        let var_name = type_id
            .and_then(|type_id| vals.variant_name(type_id, tag))
            .map_or_else(|| tag.to_string(), ToOwned::to_owned);
        if ids.is_empty() {
            Ok(format!("{ty_name}.{var_name}"))
        } else {
            let args = ids
                .iter()
                .try_fold(Vec::new(), |mut acc, id| {
                    let v = vals.value(*id)?.clone();
                    acc.push(Self::fmt_value(vals, &v)?);
                    Ok::<Vec<String>, Error>(acc)
                })?
                .into_iter()
                .join(", ");
            Ok(format!("{ty_name}.{var_name}({args})"))
        }
    }
}
