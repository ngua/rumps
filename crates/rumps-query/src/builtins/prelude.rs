use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

pub(crate) struct Prelude;

impl Body for Prelude {}

impl Prelude {
    /// `forall T, U, F: Mappable. ((T) -> U, F[T]) -> Unit`
    ///
    /// Invokes `f` for each value in `src` and returns `Unit`.
    /// Returns a `BoxFuture` because invoking `f` may run `async` user code.
    pub(crate) fn foreach<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let f = args[0];
            let a = args[1];
            let val = ctx.vals().value(a)?.clone();
            let ty = ctx.vals().value_variant_base_type(&val);

            match val.payload {
                Payload::Array(elems) => {
                    let xs: SmallVec<[ValueId; 4]> =
                        elems.iter().copied().collect();
                    let mut it = xs.into_iter();

                    while let Some(x) = it.next() {
                        ctx.invoke(f, smallvec![x]).await?;
                    }

                    Ok(ctx.vals().add(Payload::Unit))
                }
                Payload::Variant { tag: 1, vals }
                    if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                {
                    let inner = *vals.first().unwrap_or_else(|| {
                        typechecked!("Prelude.foreach", "Option.Some")
                    });
                    ctx.invoke(f, smallvec![inner]).await?;
                    Ok(ctx.vals().add(Payload::Unit))
                }
                Payload::Variant { tag: 0, .. }
                    if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                {
                    Ok(ctx.vals().add(Payload::Unit))
                }
                Payload::Variant { tag: 0, vals }
                    if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                {
                    let inner = *vals.first().unwrap_or_else(|| {
                        typechecked!("Prelude.foreach", "Result.Ok")
                    });
                    ctx.invoke(f, smallvec![inner]).await?;
                    Ok(ctx.vals().add(Payload::Unit))
                }
                Payload::Variant { tag: 1, .. }
                    if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                {
                    Ok(ctx.vals().add(Payload::Unit))
                }
                _ => typechecked!("Prelude.foreach", "Mappable"),
            }
        })
    }

    /// `forall A. (A) -> A`
    pub(crate) fn identity(
        _ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let a = args[0];
        Ok(a)
    }
}
