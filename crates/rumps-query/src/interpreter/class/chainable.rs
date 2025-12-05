use futures::future::BoxFuture;

use super::*;

/// Monadic chaining for `Option` and `Result`.
pub(crate) struct Chainable;

impl Class for Chainable {
    const ID: ClassId = ClassId::CHAINABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "chain",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::chain)),
        );
    }
}

impl Chainable {
    pub(crate) fn chain<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            enum Target {
                Invoke(ValueId),
                Keep,
            }

            let a = args[0];
            let f = args[1];
            let target: Result<Target> = {
                let vals = ctx.vals();
                let v = vals.value(a)?;
                let ty = vals.value_variant_base_type(v);

                match &v.payload {
                    Payload::Variant { tag: 0, .. }
                        if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        Ok(Target::Keep)
                    }
                    Payload::Variant { tag: 1, vals }
                        if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Chainable:chain", "Option.Some")
                        });
                        Ok(Target::Invoke(inner))
                    }
                    Payload::Variant { tag: 0, vals }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        let inner = *vals.first().unwrap_or_else(|| {
                            typechecked!("Chainable:chain", "Result.Ok")
                        });
                        Ok(Target::Invoke(inner))
                    }
                    Payload::Variant { tag: 1, .. }
                        if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        Ok(Target::Keep)
                    }
                    _ => typechecked!("Chainable:chain", "Chainable instance"),
                }
            };
            let target = target?;

            match target {
                Target::Invoke(inner) => ctx.invoke(f, smallvec![inner]).await,
                Target::Keep => Ok(a),
            }
        })
    }
}
