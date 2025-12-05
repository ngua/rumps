use futures::future::BoxFuture;

use super::*;

/// Equality comparison.
pub(crate) struct Eq;

impl Class for Eq {
    const ID: ClassId = ClassId::EQ;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "eq",
            MethodAbi::Binary,
            Builtin::Selected(Self::select),
        );
    }
}

impl Eq {
    pub(crate) fn select(
        interp: &mut Interpreter<'_, '_>,
        d: &Dispatch,
    ) -> Result<builtins::Call> {
        let imp = if interp
            .arena
            .payload(d.args[0])
            .is_some_and(|v| matches!(v, Payload::Map(_)))
        {
            Impl::Async(Self::map_eq)
        } else {
            Impl::Sync(Self::eq)
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

    pub(crate) fn eq(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let l = args[0];
        let r = args[1];
        let (l, r) = {
            let vals = ctx.vals();
            (vals.value(l)?.clone(), vals.value(r)?.clone())
        };
        let eq = Self::values_eq(&mut ctx.vals(), &l, &r)?;
        Ok(ctx.vals().add(Payload::Bool(eq)))
    }

    pub(crate) fn map_eq<'a>(
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
            let eq = match (&l.payload, &r.payload) {
                (Payload::Map(l), Payload::Map(r)) => {
                    ctx.maps().eq(l, r).await?
                }
                (Payload::Map(_), _) => typechecked!("==", "Map"),
                _ => typechecked!("==", "Map"),
            };
            Ok(ctx.vals().add(Payload::Bool(eq)))
        })
    }

    fn payloads_eq(
        vals: &mut Values<'_, '_, '_, '_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<bool> {
        match (l, r) {
            (Payload::Unit, Payload::Unit) => Ok(true),
            (Payload::Bool(a), Payload::Bool(b)) => Ok(a == b),
            (Payload::Int(a), Payload::Int(b)) => Ok(a == b),
            (Payload::Word(a), Payload::Word(b)) => Ok(a == b),
            (Payload::Float(a), Payload::Float(b)) => Ok(a == b),
            (Payload::Char(a), Payload::Char(b)) => Ok(a == b),
            (Payload::String(a), Payload::String(b)) => {
                Ok(vals.str(*a)? == vals.str(*b)?)
            }
            (Payload::Time(a), Payload::Time(b)) => Ok(a == b),
            (Payload::FilePath(a), Payload::FilePath(b)) => {
                Ok(vals.str(*a)? == vals.str(*b)?)
            }
            (Payload::Json(a), Payload::Json(b)) => Ok(a == b),
            (Payload::Array(a), Payload::Array(b)) => Ok(a.len() == b.len()
                && Self::seqs_eq(vals, a.as_slice(), b.as_slice())?),
            (Payload::Tuple(a), Payload::Tuple(b)) => Ok(a.len() == b.len()
                && Self::seqs_eq(vals, a.as_slice(), b.as_slice())?),
            (Payload::Object(a), Payload::Object(b)) => {
                Ok(a.len() == b.len() && Self::objects_eq(vals, a, b)?)
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
            ) => Ok(idx1 == idx2
                && p1.len() == p2.len()
                && Self::seqs_eq(vals, p1, p2)?),
            (
                Payload::Ref(g1, name1, subs1),
                Payload::Ref(g2, name2, subs2),
            ) => Ok(g1 == g2
                && name1 == name2
                && subs1.len() == subs2.len()
                && Self::seqs_eq(vals, subs1, subs2)?),
            _ => typechecked!("==", "Eq instance"),
        }
    }

    fn values_eq(
        vals: &mut Values<'_, '_, '_, '_>,
        l: &Value,
        r: &Value,
    ) -> Result<bool> {
        match (&l.payload, &r.payload) {
            (Payload::Int(n), Payload::Variant { tag, .. })
                if vals.value_variant_base_type(r)
                    == Some(TypeId::ORDERING) =>
            {
                Ok(*n
                    == match *tag {
                        0 => -1,
                        1 => 0,
                        2 => 1,
                        _ => typechecked!("Ordering", "valid tag"),
                    })
            }
            (Payload::Variant { tag, .. }, Payload::Int(n))
                if vals.value_variant_base_type(l)
                    == Some(TypeId::ORDERING) =>
            {
                Ok(match *tag {
                    0 => -1,
                    1 => 0,
                    2 => 1,
                    _ => typechecked!("Ordering", "valid tag"),
                } == *n)
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
            ) => Ok(vals.value_variant_base_type(l)
                == vals.value_variant_base_type(r)
                && idx1 == idx2
                && p1.len() == p2.len()
                && Self::seqs_eq(vals, p1, p2)?),
            _ => Self::payloads_eq(vals, &l.payload, &r.payload),
        }
    }

    fn value_ids_eq(
        vals: &mut Values<'_, '_, '_, '_>,
        l: ValueId,
        r: ValueId,
    ) -> Result<bool> {
        let lv = vals.value(l)?.clone();
        let rv = vals.value(r)?.clone();
        Self::values_eq(vals, &lv, &rv)
    }

    fn seqs_eq(
        vals: &mut Values<'_, '_, '_, '_>,
        a: &[ValueId],
        b: &[ValueId],
    ) -> Result<bool> {
        a.iter().zip(b.iter()).try_fold(true, |ok, (ai, bi)| {
            if ok {
                Self::value_ids_eq(vals, *ai, *bi)
            } else {
                Ok(false)
            }
        })
    }

    fn objects_eq(
        vals: &mut Values<'_, '_, '_, '_>,
        a: &IndexMap<StringId, ValueId>,
        b: &IndexMap<StringId, ValueId>,
    ) -> Result<bool> {
        a.iter().try_fold(true, |ok, (k, av)| {
            if ok {
                b.get(k)
                    .map_or(Ok(false), |bv| Self::value_ids_eq(vals, *av, *bv))
            } else {
                Ok(false)
            }
        })
    }
}
