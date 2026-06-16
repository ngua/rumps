use super::*;

/// Equality comparison.
pub(crate) struct Eq;

impl Class for Eq {
    const ID: ClassId = ClassId::EQ;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "eq", MethodFn::Binary(Self::eq));
    }
}

impl Eq {
    pub(crate) fn eq(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(Payload::Bool(Self::values_equal(ctx, l, r)))
    }

    pub(crate) fn eq_values(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Payload {
        Payload::Bool(Self::runtime_values_equal(ctx, l, r))
    }

    /// Recursive equality helper.
    fn values_equal(ctx: &mut ClassCtx<'_>, l: &Payload, r: &Payload) -> bool {
        match (l, r) {
            (Payload::Unit, Payload::Unit) => true,
            (Payload::Bool(a), Payload::Bool(b)) => a == b,
            (Payload::Int(a), Payload::Int(b)) => a == b,
            (Payload::Word(a), Payload::Word(b)) => a == b,
            (Payload::Float(a), Payload::Float(b)) => a == b,
            (Payload::Char(a), Payload::Char(b)) => a == b,
            (Payload::String(a), Payload::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Payload::Time(a), Payload::Time(b)) => a == b,
            (Payload::FilePath(a), Payload::FilePath(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa == sb
            }
            (Payload::Json(a), Payload::Json(b)) => a == b,
            (Payload::Array(a), Payload::Array(b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Payload::Tuple(a), Payload::Tuple(b)) => {
                a.len() == b.len()
                    && Self::seqs_equal(ctx, a.as_slice(), b.as_slice())
            }
            (Payload::Object(a), Payload::Object(b)) => {
                a.len() == b.len() && Self::objects_equal(ctx, a, b)
            }
            (Payload::Map(_), Payload::Map(_)) => {
                typechecked!("==", "async Map equality")
            }
            (
                Payload::Variant {
                    tag: idx1,
                    vals: p1,
                },
                Payload::Variant {
                    tag: idx2,
                    vals: p2,
                },
            ) => {
                idx1 == idx2
                    && p1.len() == p2.len()
                    && Self::seqs_equal(ctx, p1, p2)
            }
            (
                Payload::Ref(g1, name1, subs1),
                Payload::Ref(g2, name2, subs2),
            ) => {
                g1 == g2
                    && name1 == name2
                    && subs1.len() == subs2.len()
                    && Self::seqs_equal(ctx, subs1, subs2)
            }
            _ => typechecked!("==", "same Eq type"),
        }
    }

    fn runtime_values_equal(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> bool {
        match (&l.payload, &r.payload) {
            (
                Payload::Variant {
                    tag: idx1,
                    vals: p1,
                },
                Payload::Variant {
                    tag: idx2,
                    vals: p2,
                },
            ) => {
                ctx.value_variant_base_type(l) == ctx.value_variant_base_type(r)
                    && idx1 == idx2
                    && p1.len() == p2.len()
                    && Self::seqs_equal(ctx, p1, p2)
            }
            _ => Self::values_equal(ctx, &l.payload, &r.payload),
        }
    }

    fn value_ids_equal(ctx: &mut ClassCtx<'_>, l: ValueId, r: ValueId) -> bool {
        let lv = ctx.arena.value(l).cloned();
        let rv = ctx.arena.value(r).cloned();
        match (lv, rv) {
            (Some(lv), Some(rv)) => Self::runtime_values_equal(ctx, &lv, &rv),
            _ => false,
        }
    }

    /// Element-wise equality for sequences (arrays, tuples, payloads).
    fn seqs_equal(
        ctx: &mut ClassCtx<'_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> bool {
        a.iter()
            .zip(b.iter())
            .all(|(ai, bi)| Self::value_ids_equal(ctx, *ai, *bi))
    }

    /// Field-wise equality for objects.
    fn objects_equal(
        ctx: &mut ClassCtx<'_>,
        a: &IndexMap<StringId, ValueId>,
        b: &IndexMap<StringId, ValueId>,
    ) -> bool {
        a.iter().all(|(k, av)| {
            b.get(k)
                .map(|bv| Self::value_ids_equal(ctx, *av, *bv))
                .unwrap_or(false)
        })
    }
}
