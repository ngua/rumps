use futures::future::BoxFuture;

use super::*;

/// `Filterable` class: `filter` method.
pub(crate) struct Filterable;

impl Class for Filterable {
    const ID: ClassId = ClassId::FILTERABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "filter",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::filter)),
        );
    }
}

impl Filterable {
    pub(crate) fn filter<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let xs = ctx.vals().array_ids(a, "Filterable:filter")?;
            let mut acc = SmallVec::new();
            let mut it = xs.iter().copied();

            while let Some(x) = it.next() {
                let keep = ctx.invoke(f, smallvec![x]).await?;
                acc.extend(
                    ctx.vals()
                        .bool_payload(keep, "Filterable:filter")?
                        .then_some(x),
                );
            }

            Ok(ctx.vals().add(Payload::Array(Arc::new(acc))))
        })
    }
}
