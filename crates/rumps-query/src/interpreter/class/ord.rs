use futures::future::BoxFuture;

use super::*;

pub(crate) struct Ord;

impl Class for Ord {
    const ID: ClassId = ClassId::ORD;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "compare",
            MethodAbi::Binary,
            Builtin::Selected(Self::select),
        );
    }
}

impl Ord {
    pub(crate) fn select(
        interp: &mut Interpreter<'_, '_>,
        d: &Dispatch,
    ) -> Result<builtins::Call> {
        let imp = if interp
            .arena
            .payload(d.args[0])
            .is_some_and(|v| matches!(v, Payload::Map(_)))
        {
            Impl::Async(Self::map_compare)
        } else {
            Impl::Sync(Self::compare)
        };
        Ok(builtins::Call {
            imp,
            args: d.args.clone(),
            span: d.span,
            output: interp.class_output_meta(
                d.output_expr_id,
                d.dispatch_expr_id,
                d.output_ty,
                d.class,
            ),
            meta: None,
        })
    }

    pub(crate) fn compare(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let l = args[0];
        let r = args[1];
        let (l, r) = {
            let vals = ctx.vals();
            (vals.value(l)?.clone(), vals.value(r)?.clone())
        };
        let ord = Self::value_cmp(&mut ctx.vals(), &l, &r)?;
        Ok(ctx.vals().add(Payload::Int(match ord {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        })))
    }

    pub(crate) fn map_compare<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let l = args[0];
            let r = args[1];
            let (l, r) = {
                let vals = ctx.vals();
                (vals.value(l)?.clone(), vals.value(r)?.clone())
            };
            let ord = match (&l.payload, &r.payload) {
                (Payload::Map(l), Payload::Map(r)) => {
                    ctx.maps().cmp(l, r).await?
                }
                (Payload::Map(_), _) => typechecked!("compare", "Map"),
                _ => typechecked!("compare", "Map"),
            };
            Ok(ctx.vals().add(Payload::Int(match ord {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            })))
        })
    }

    fn payload_cmp(
        vals: &mut Values<'_, '_, '_, '_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Ordering> {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => Ok(a.cmp(b)),
            (Payload::Word(a), Payload::Word(b)) => Ok(a.cmp(b)),
            (Payload::Float(a), Payload::Float(b)) => Ok(a.cmp(b)),
            (Payload::String(a), Payload::String(b)) => {
                Ok(vals.str(*a)?.cmp(vals.str(*b)?))
            }
            (Payload::Char(a), Payload::Char(b)) => Ok(a.cmp(b)),
            (Payload::Bool(a), Payload::Bool(b)) => Ok(a.cmp(b)),
            (Payload::Time(a), Payload::Time(b)) => Ok(a.cmp(b)),
            (Payload::Array(a), Payload::Array(b)) => {
                Self::seq_cmp(vals, a.as_slice(), b.as_slice())
            }
            (Payload::Tuple(a), Payload::Tuple(b)) => {
                Self::seq_cmp(vals, a.as_slice(), b.as_slice())
            }
            (Payload::Map(_), Payload::Map(_)) => {
                typechecked!("compare", "async Map compare")
            }
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) => match i1.cmp(i2) {
                Ordering::Equal => Self::seq_cmp(vals, p1, p2),
                ord => Ok(ord),
            },
            _ => typechecked!("compare", "Ord instance"),
        }
    }

    fn value_cmp(
        vals: &mut Values<'_, '_, '_, '_>,
        l: &Value,
        r: &Value,
    ) -> Result<Ordering> {
        match (&l.payload, &r.payload) {
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) => {
                let l_ty = vals.value_variant_base_type(l);
                let r_ty = vals.value_variant_base_type(r);
                match l_ty.cmp(&r_ty) {
                    Ordering::Equal => {
                        let idx_ord = if l_ty == Some(TypeId::RESULT) {
                            i2.cmp(i1)
                        } else {
                            i1.cmp(i2)
                        };
                        match idx_ord {
                            Ordering::Equal => Self::seq_cmp(vals, p1, p2),
                            ord => Ok(ord),
                        }
                    }
                    ord => Ok(ord),
                }
            }
            _ => Self::payload_cmp(vals, &l.payload, &r.payload),
        }
    }

    fn id_cmp(
        vals: &mut Values<'_, '_, '_, '_>,
        l: ValueId,
        r: ValueId,
    ) -> Result<Ordering> {
        let lv = vals.value(l)?.clone();
        let rv = vals.value(r)?.clone();
        Self::value_cmp(vals, &lv, &rv)
    }

    fn seq_cmp(
        vals: &mut Values<'_, '_, '_, '_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> Result<Ordering> {
        a.iter()
            .zip(b.iter())
            .try_fold(Ordering::Equal, |ord, (ai, bi)| match ord {
                Ordering::Equal => Self::id_cmp(vals, *ai, *bi),
                ord => Ok(ord),
            })
            .map(|ord| match ord {
                Ordering::Equal => a.len().cmp(&b.len()),
                ord => ord,
            })
    }
}
