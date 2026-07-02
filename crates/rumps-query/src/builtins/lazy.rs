use futures::future::BoxFuture;
use smallvec::{smallvec, SmallVec};

use super::Body;
use crate::builtins::BuiltinCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

pub(crate) struct Lazy;

impl Body for Lazy {}

impl Lazy {
    /// `forall T. (Lazy[T]) -> T`
    pub(crate) fn force<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            let id = args
                .first()
                .copied()
                .unwrap_or_else(|| typechecked!("Lazy.force", "argument"));
            let f = {
                let vals = ctx.vals();
                let val = vals.value(id)?;

                match &val.payload {
                    Payload::Variant { tag: 0, vals: ps }
                        if vals
                            .value_variant_base_type(val)
                            .is_some_and(|ty| ty == TypeId::LAZY) =>
                    {
                        ps.first().copied().unwrap_or_else(|| {
                            typechecked!("Lazy.force", "Lazy.Lazy payload")
                        })
                    }
                    _ => typechecked!("Lazy.force", "Lazy.Lazy"),
                }
            };

            ctx.invoke(f, smallvec![]).await
        })
    }
}
