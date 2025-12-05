use futures::future::BoxFuture;

use super::*;

/// `Mappable` class: `map` method.
pub(crate) struct Mappable;

impl Class for Mappable {
    const ID: ClassId = ClassId::MAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "map",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::map)),
        );
    }
}

impl Mappable {
    pub(crate) fn map<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            enum Target {
                Array(Arc<SmallVec<[ValueId; 4]>>),
                Tuple(ValueId, ValueId),
                OptionSome(ValueId),
                OptionNone,
                ResultOk(ValueId),
                ResultErr,
            }

            let f = args[0];
            let a = args[1];
            let out = ctx.output_ty();
            let target: Result<Target> = {
                let vals = ctx.vals();
                let v = vals.value(a)?;
                let ty = vals.value_variant_base_type(v);

                match &v.payload {
                    Payload::Array(elems) => Ok(Target::Array(elems.clone())),
                    Payload::Tuple(elems) if elems.len() == 2 => {
                        let fst = *elems.first().unwrap_or_else(|| {
                            invariant!("Tuple has first elem")
                        });
                        let snd = *elems.get(1).unwrap_or_else(|| {
                            invariant!("Tuple has second elem")
                        });
                        Ok(Target::Tuple(fst, snd))
                    }
                    Payload::Variant { tag: 1, vals }
                        if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Mappable:map", "Option.Some")
                        });
                        Ok(Target::OptionSome(inner))
                    }
                    Payload::Variant { tag: 0, .. }
                        if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        Ok(Target::OptionNone)
                    }
                    Payload::Variant { tag: 0, vals }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Mappable:map", "Result.Ok")
                        });
                        Ok(Target::ResultOk(inner))
                    }
                    Payload::Variant { tag: 1, .. }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        Ok(Target::ResultErr)
                    }
                    _ => typechecked!("Mappable:map", "Mappable instance"),
                }
            };
            let target = target?;

            match target {
                Target::Array(xs) => {
                    let mut acc = SmallVec::new();
                    let mut it = xs.iter().copied();

                    while let Some(x) = it.next() {
                        acc.push(ctx.invoke(f, smallvec![x]).await?);
                    }

                    let v = Payload::Array(Arc::new(acc));
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(v, ty),
                        None => ctx.vals().add(v),
                    })
                }
                Target::Tuple(fst, snd) => {
                    let res = ctx.invoke(f, smallvec![snd]).await?;
                    let v = Payload::Tuple(Arc::new(smallvec![fst, res]));
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(v, ty),
                        None => ctx.vals().add(v),
                    })
                }
                Target::OptionSome(inner) => {
                    let res = ctx.invoke(f, smallvec![inner]).await?;
                    Ok(match out {
                        Some(ty) => {
                            ctx.vals().add_typed(Payload::some(res), ty)
                        }
                        None => ctx.vals().option_some(res),
                    })
                }
                Target::OptionNone => Ok(match out {
                    Some(ty) => ctx.vals().add_typed(Payload::none(), ty),
                    None => a,
                }),
                Target::ResultOk(inner) => {
                    let res = ctx.invoke(f, smallvec![inner]).await?;
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(Payload::ok(res), ty),
                        None => ctx.vals().result_ok(res),
                    })
                }
                Target::ResultErr => Ok(a),
            }
        })
    }
}
