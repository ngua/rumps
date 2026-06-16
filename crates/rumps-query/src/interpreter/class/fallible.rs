use super::*;

/// Unwrap for `Option` and `Result`.
pub(crate) struct Fallible;

impl Class for Fallible {
    const ID: ClassId = ClassId::FALLIBLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "unwrap", MethodFn::Unary(Self::unwrap));
    }
}

impl Fallible {
    /// Unwrap an `Option.Some` or `Result.Ok` value.
    pub(crate) fn unwrap(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        match v {
            Payload::Variant { tag: 1, vals } => {
                let val = vals
                    .first()
                    .and_then(|id| ctx.arena.payload(*id).cloned())
                    .unwrap_or_else(|| {
                        typechecked!("unwrap", "Option.Some payload")
                    });
                Ok(val)
            }
            Payload::Variant { tag: 0, .. } => {
                Err(Error::runtime(ctx.span, "cannot unwrap Option.None"))
            }
            _ => typechecked!("unwrap", "Fallible"),
        }
    }

    pub(crate) fn unwrap_value(
        ctx: &mut ClassCtx<'_>,
        v: &Value,
    ) -> Result<Payload> {
        let ty = ctx.value_variant_base_type(v);
        match &v.payload {
            Payload::Variant { tag: 1, vals }
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                vals.first()
                    .and_then(|id| ctx.arena.payload(*id).cloned())
                    .ok_or_else(|| {
                        typechecked!("unwrap", "Option.Some payload")
                    })
            }
            Payload::Variant { tag: 0, .. }
                if ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Err(Error::runtime(ctx.span, "cannot unwrap Option.None"))
            }
            Payload::Variant { tag: 0, vals }
                if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                vals.first()
                    .and_then(|id| ctx.arena.payload(*id).cloned())
                    .ok_or_else(|| typechecked!("unwrap", "Result.Ok payload"))
            }
            Payload::Variant { tag: 1, .. }
                if ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Err(Error::runtime(ctx.span, "cannot unwrap Result.Err"))
            }
            _ => typechecked!("unwrap", "Fallible"),
        }
    }
}
