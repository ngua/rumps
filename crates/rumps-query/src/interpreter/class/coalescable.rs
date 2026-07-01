use futures::future::BoxFuture;

use super::*;

pub(crate) struct Coalescable;

impl Class for Coalescable {
    const ID: ClassId = ClassId::COALESCABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "coalesce",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Async(Self::coalesce)),
        );
    }
}

impl Coalescable {
    pub(crate) fn coalesce<'a>(
        ctx: &'a mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> BoxFuture<'a, Result<ValueId>> {
        Box::pin(async move {
            enum Target {
                Keep(ValueId),
                Force(ValueId),
            }

            let lhs_id = args
                .first()
                .copied()
                .unwrap_or_else(|| typechecked!("coalesce", "lhs argument"));
            let rhs_id = args
                .get(1)
                .copied()
                .unwrap_or_else(|| typechecked!("coalesce", "rhs argument"));
            let target: Result<Target> = {
                let vals = ctx.vals();
                let lhs = vals.value(lhs_id)?;
                let lhs_ty = vals.value_variant_base_type(lhs);

                match &lhs.payload {
                    Payload::Variant { tag: 1, vals }
                        if lhs_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        vals.first().copied().map(Target::Keep).ok_or_else(
                            || typechecked!("coalesce", "Option.Some payload"),
                        )
                    }
                    Payload::Variant { tag: 0, .. }
                        if lhs_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
                    {
                        let rhs = vals.value(rhs_id)?;
                        match &rhs.payload {
                            Payload::Variant { tag: 0, vals: ps }
                                if vals
                                    .value_variant_base_type(rhs)
                                    .is_some_and(|ty| ty == TypeId::LAZY) =>
                            {
                                ps.first()
                                    .copied()
                                    .map(Target::Force)
                                    .ok_or_else(|| {
                                        typechecked!(
                                            "coalesce",
                                            "Lazy.Lazy payload"
                                        )
                                    })
                            }
                            _ => typechecked!("coalesce", "Lazy.Lazy rhs"),
                        }
                    }
                    Payload::Variant { tag: 0, vals }
                        if lhs_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        vals.first().copied().map(Target::Keep).ok_or_else(
                            || typechecked!("coalesce", "Result.Ok payload"),
                        )
                    }
                    Payload::Variant { tag: 1, .. }
                        if lhs_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
                    {
                        let rhs = vals.value(rhs_id)?;
                        match &rhs.payload {
                            Payload::Variant { tag: 0, vals: ps }
                                if vals
                                    .value_variant_base_type(rhs)
                                    .is_some_and(|ty| ty == TypeId::LAZY) =>
                            {
                                ps.first()
                                    .copied()
                                    .map(Target::Force)
                                    .ok_or_else(|| {
                                        typechecked!(
                                            "coalesce",
                                            "Lazy.Lazy payload"
                                        )
                                    })
                            }
                            _ => typechecked!("coalesce", "Lazy.Lazy rhs"),
                        }
                    }
                    _ => typechecked!("coalesce", "Coalescable instance"),
                }
            };

            match target? {
                Target::Keep(id) => Ok(id),
                Target::Force(f) => ctx.invoke(f, smallvec![]).await,
            }
        })
    }
}
