use super::*;

/// Unwrap for `Option` and `Result`.
pub(crate) struct Unwrappable;

impl Class for Unwrappable {
    const ID: ClassId = ClassId::UNWRAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "unwrap",
            MethodAbi::Unary,
            Builtin::Fixed(Impl::Sync(Self::unwrap)),
        );
    }
}

impl Unwrappable {
    pub(crate) fn unwrap(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let span = ctx.span();
        let id = args[0];
        let vals = ctx.vals();
        let v = vals.value(id)?.clone();
        let ty = vals.value_variant_base_type(&v);
        match &v.payload {
            Payload::Variant { tag: 1, vals }
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                vals.first().copied().ok_or_else(|| {
                    typechecked!("unwrap", "Option.Some payload")
                })
            }
            Payload::Variant { tag: 0, .. }
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            Payload::Variant { tag: 0, vals }
                if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                vals.first()
                    .copied()
                    .ok_or_else(|| typechecked!("unwrap", "Result.Ok payload"))
            }
            Payload::Variant { tag: 1, .. }
                if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Err(Error::runtime(span, "cannot unwrap Result.Err"))
            }
            Payload::Variant { tag: 1, vals } => vals
                .first()
                .copied()
                .ok_or_else(|| typechecked!("unwrap", "Option.Some payload")),
            Payload::Variant { tag: 0, .. } => {
                Err(Error::runtime(span, "cannot unwrap Option.None"))
            }
            _ => typechecked!("unwrap", "Unwrappable instance"),
        }
    }
}
