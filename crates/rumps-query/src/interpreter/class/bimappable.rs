use futures::future::BoxFuture;

use super::*;

/// `Bimappable` class: `bimap` method.
pub(crate) struct Bimappable;

impl Class for Bimappable {
    const ID: ClassId = ClassId::BIMAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "bimap",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::bimap)),
        );
    }
}

impl Bimappable {
    pub(crate) fn bimap<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            enum Target {
                ResultOk(ValueId),
                ResultErr(ValueId),
                Tuple(ValueId, ValueId),
            }

            let f = args[0];
            let g = args[1];
            let a = args[2];
            let out = ctx.output_ty();
            let target: Result<Target> = {
                let vals = ctx.vals();
                let v = vals.value(a)?;
                let ty = vals.value_variant_base_type(v);

                match &v.payload {
                    Payload::Variant { tag: 0, vals }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Bimappable:bimap", "Result.Ok")
                        });
                        Ok(Target::ResultOk(inner))
                    }
                    Payload::Variant { tag: 1, vals }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Bimappable:bimap", "Result.Err")
                        });
                        Ok(Target::ResultErr(inner))
                    }
                    Payload::Tuple(elems) if elems.len() == 2 => {
                        let fst = *elems.first().unwrap_or_else(|| {
                            invariant!("bimap tuple has first elem")
                        });
                        let snd = *elems.get(1).unwrap_or_else(|| {
                            invariant!("bimap tuple has second elem")
                        });
                        Ok(Target::Tuple(fst, snd))
                    }
                    _ => {
                        typechecked!("Bimappable:bimap", "Bimappable instance")
                    }
                }
            };
            let target = target?;

            match target {
                Target::ResultOk(inner) => {
                    let res = ctx.invoke(f, smallvec![inner]).await?;
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(Payload::ok(res), ty),
                        None => ctx.vals().result_ok(res),
                    })
                }
                Target::ResultErr(inner) => {
                    let res = ctx.invoke(g, smallvec![inner]).await?;
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(Payload::err(res), ty),
                        None => ctx.vals().result_err(res),
                    })
                }
                Target::Tuple(fst, snd) => {
                    let l = ctx.invoke(f, smallvec![fst]).await?;
                    let r = ctx.invoke(g, smallvec![snd]).await?;
                    let v = Payload::Tuple(Arc::new(smallvec![l, r]));
                    Ok(match out {
                        Some(ty) => ctx.vals().add_typed(v, ty),
                        None => ctx.vals().add(v),
                    })
                }
            }
        })
    }
}
