use futures::future::BoxFuture;

use super::*;

/// Concatenation for `String`, `Array`, `Map`, `Option`.
pub(crate) struct Concatable;

impl Class for Concatable {
    const ID: ClassId = ClassId::CONCATABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "concat",
            MethodAbi::Binary,
            Builtin::Selected(Self::select),
        );
    }
}

impl Concatable {
    pub(crate) fn select(
        interp: &mut Interpreter<'_, '_>,
        d: &Dispatch,
    ) -> Result<builtins::Call> {
        let imp = if interp
            .arena
            .payload(d.args[0])
            .is_some_and(|v| matches!(v, Payload::Map(_)))
        {
            Impl::Async(Self::map_concat)
        } else {
            Impl::Sync(Self::concat)
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

    pub(crate) fn concat(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let l_id = args[0];
        let r_id = args[1];
        let (l, r) = {
            let vals = ctx.vals();
            (vals.value(l_id)?.clone(), vals.value(r_id)?.clone())
        };
        let v = match (&l.payload, &r.payload) {
            (
                Payload::Variant { tag: i1, vals: p1 },
                Payload::Variant { tag: i2, vals: p2 },
            ) if ctx
                .vals()
                .value_variant_base_type(&l)
                .is_none_or(|ty| ty == TypeId::OPTION)
                && ctx
                    .vals()
                    .value_variant_base_type(&r)
                    .is_none_or(|ty| ty == TypeId::OPTION) =>
            {
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
            (Payload::Variant { .. }, Payload::Variant { .. }) => {
                typechecked!("++", "Concatable instance")
            }
            _ => {
                Self::concat_payloads(&mut ctx.vals(), &l.payload, &r.payload)?
            }
        };
        Ok(match v {
            Payload::Variant { .. } => {
                let ty = ctx.output_ty().unwrap_or(l.ty);
                ctx.vals().add_typed(v, ty)
            }
            _ => ctx.vals().add(v),
        })
    }

    pub(crate) fn map_concat<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let l_id = args[0];
            let r_id = args[1];
            let (l, r) = {
                let vals = ctx.vals();
                (vals.value(l_id)?.clone(), vals.value(r_id)?.clone())
            };
            let v = match (&l.payload, &r.payload) {
                (Payload::Map(l), Payload::Map(r)) => {
                    Payload::Map(Arc::new(ctx.maps().merge(l, r).await?))
                }
                (Payload::Map(_), _) => typechecked!("concat", "Map"),
                _ => typechecked!("concat", "Map"),
            };
            Ok(ctx.vals().add(v))
        })
    }

    fn concat_payloads(
        vals: &mut Values<'_, '_, '_, '_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::String(ls), Payload::String(rs)) => {
                let s = format!("{}{}", vals.str(*ls)?, vals.str(*rs)?);
                Payload::String(vals.intern(&s))
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
            _ => typechecked!("++", "Concatable instance"),
        })
    }
}
