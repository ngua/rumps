use futures::future::BoxFuture;

use super::*;

/// `Foldable` class: `fold`, `fold-map` methods.
pub(crate) struct Foldable;

impl Class for Foldable {
    const ID: ClassId = ClassId::FOLDABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "fold",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::fold)),
        );
        Self::register(
            methods,
            i,
            "fold-map",
            MethodAbi::Hkt,
            Builtin::Fixed(Impl::Async(Self::fold_map)),
        );
    }
}

impl Foldable {
    pub(crate) fn fold<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let mut acc = args[1];
            let a = args[2];
            let xs = ctx.vals().array_ids(a, "Foldable:fold")?;
            let mut it = xs.iter().copied();

            while let Some(x) = it.next() {
                acc = ctx.invoke(f, smallvec![acc, x]).await?;
            }

            Ok(acc)
        })
    }

    pub(crate) fn fold_map<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let out = ctx.vals().callable_ret_ty(f).unwrap_or_else(|| {
                typechecked!("Foldable:fold-map", "callable metadata")
            });
            let xs = ctx.vals().array_ids(a, "Foldable:fold-map")?;
            let default = ctx.vals().intern("default");
            let concat = ctx.vals().intern("concat");
            let mut acc = ctx
                .class_call(
                    ClassId::DEFAULT,
                    default,
                    SmallVec::new(),
                    Some(out),
                )
                .await?;
            let mut it = xs.iter().copied();

            while let Some(x) = it.next() {
                let mapped = ctx.invoke(f, smallvec![x]).await?;
                let ty = ctx
                    .vals()
                    .meta(mapped)
                    .map(|meta| meta.ty)
                    .unwrap_or_else(|| {
                        typechecked!("Foldable:fold-map", "mapper result meta")
                    });
                acc = ctx
                    .class_call(
                        ClassId::CONCATABLE,
                        concat,
                        smallvec![acc, mapped],
                        Some(ty),
                    )
                    .await?;
            }

            Ok(acc)
        })
    }
}
