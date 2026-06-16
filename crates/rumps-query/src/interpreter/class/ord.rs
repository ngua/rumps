use super::*;

pub(crate) struct Ord;

impl Class for Ord {
    const ID: ClassId = ClassId::ORD;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "compare", MethodFn::Binary(Self::compare));
    }
}

impl Ord {
    pub(crate) fn compare(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        let ord = Self::cmp_values(ctx, l, r);
        Ok(Payload::Int(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        }))
    }

    pub(crate) fn compare_values(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Payload {
        let ord = Self::cmp_runtime_values(ctx, l, r);
        Payload::Int(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        })
    }

    /// Recursive comparison helper returning `Ordering`.
    fn cmp_values(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Ordering {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => a.cmp(b),
            (Payload::Word(a), Payload::Word(b)) => a.cmp(b),
            (Payload::Float(a), Payload::Float(b)) => a.cmp(b),
            (Payload::String(a), Payload::String(b)) => {
                let sa = ctx.arena.get_str(*a).unwrap_or("");
                let sb = ctx.arena.get_str(*b).unwrap_or("");
                sa.cmp(sb)
            }
            (Payload::Char(a), Payload::Char(b)) => a.cmp(b),
            (Payload::Bool(a), Payload::Bool(b)) => a.cmp(b),
            (Payload::Time(a), Payload::Time(b)) => a.cmp(b),
            // Arrays: lexicographic comparison
            (Payload::Array(a), Payload::Array(b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            // Tuples: lexicographic comparison
            (Payload::Tuple(a), Payload::Tuple(b)) => {
                Self::cmp_seqs(ctx, a.as_slice(), b.as_slice())
            }
            (Payload::Map(_), Payload::Map(_)) => {
                typechecked!("compare", "async Map compare")
            }
            // Variants compare by tag, then payload.
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) => {
                let idx_ord = i1.cmp(i2);
                match idx_ord {
                    Ordering::Equal => Self::cmp_seqs(ctx, p1, p2),
                    ord => ord,
                }
            }
            _ => typechecked!("compare", "same Ord type"),
        }
    }

    fn cmp_runtime_values(
        ctx: &mut ClassCtx<'_>,
        l: &Value,
        r: &Value,
    ) -> Ordering {
        match (&l.payload, &r.payload) {
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) => {
                let l_ty = ctx.value_variant_base_type(l);
                let r_ty = ctx.value_variant_base_type(r);
                match l_ty.cmp(&r_ty) {
                    Ordering::Equal => {
                        let idx_ord = if l_ty == Some(TypeId::RESULT) {
                            i2.cmp(i1)
                        } else {
                            i1.cmp(i2)
                        };
                        match idx_ord {
                            Ordering::Equal => Self::cmp_seqs(ctx, p1, p2),
                            ord => ord,
                        }
                    }
                    ord => ord,
                }
            }
            _ => Self::cmp_values(ctx, &l.payload, &r.payload),
        }
    }

    fn cmp_value_ids(
        ctx: &mut ClassCtx<'_>,
        l: ValueId,
        r: ValueId,
    ) -> Ordering {
        let lv = ctx.arena.value(l).cloned();
        let rv = ctx.arena.value(r).cloned();
        match (lv, rv) {
            (Some(lv), Some(rv)) => Self::cmp_runtime_values(ctx, &lv, &rv),
            _ => Ordering::Equal,
        }
    }

    /// Lexicographic comparison of sequences of `ValueId`s.
    fn cmp_seqs(
        ctx: &mut ClassCtx<'_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> Ordering {
        a.iter()
            .zip(b.iter())
            .map(|(ai, bi)| Self::cmp_value_ids(ctx, *ai, *bi))
            .find(|o| *o != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len()))
    }
}
