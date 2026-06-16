use super::*;

/// Concatenation for `String`, `Array`, `Map`, `Option`.
pub(crate) struct Concatable;

impl Class for Concatable {
    const ID: ClassId = ClassId::CONCATABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "concat", MethodFn::Binary(Self::concat));
    }
}

impl Concatable {
    pub(crate) fn concat(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::String(ls), Payload::String(rs)) => {
                let l = ctx.arena.get_str(*ls).unwrap_or("");
                let r = ctx.arena.get_str(*rs).unwrap_or("");
                Payload::String(ctx.arena.intern(&format!("{l}{r}")))
            }
            (Payload::Array(l), Payload::Array(r)) => {
                let mut elems = Arc::unwrap_or_clone(l.clone());
                elems.extend(r.iter().copied());
                Payload::Array(Arc::new(elems))
            }
            (Payload::Map(_), Payload::Map(_)) => {
                typechecked!("concat", "async Map concat")
            }
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) => {
                if *i1 == 1 {
                    Payload::Variant {
                        tag: *i1,
                        vals: p1.clone(),
                    }
                } else if *i2 == 1 {
                    Payload::Variant {
                        tag: *i2,
                        vals: p2.clone(),
                    }
                } else {
                    Payload::Variant {
                        tag: *i1,
                        vals: SmallVec::new(),
                    }
                }
            }
            _ => typechecked!("++", "Concatable"),
        })
    }

    pub(crate) fn concat_values(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Result<Payload> {
        let l_ty = ctx.value_variant_base_type(l);
        let r_ty = ctx.value_variant_base_type(r);
        match (&l.payload, &r.payload) {
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) if l_ty.is_none_or(|ty| ty == TypeId::OPTION)
                && r_ty.is_none_or(|ty| ty == TypeId::OPTION) =>
            {
                Ok(if *i1 == 1 {
                    Payload::Variant {
                        tag: *i1,
                        vals: p1.clone(),
                    }
                } else if *i2 == 1 {
                    Payload::Variant {
                        tag: *i2,
                        vals: p2.clone(),
                    }
                } else {
                    Payload::Variant {
                        tag: *i1,
                        vals: SmallVec::new(),
                    }
                })
            }
            (Payload::Variant { .. }, Payload::Variant { .. }) => {
                typechecked!("++", "Concatable Option")
            }
            _ => Self::concat(ctx, &l.payload, &r.payload),
        }
    }
}
