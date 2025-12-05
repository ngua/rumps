use super::*;

/// `Iterable` class: `length`, `reverse` methods.
pub(crate) struct Iterable;

impl Class for Iterable {
    const ID: ClassId = ClassId::ITERABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "length",
            MethodAbi::Unary,
            Builtin::Fixed(Impl::Sync(Self::length)),
        );
        Self::register(
            methods,
            i,
            "reverse",
            MethodAbi::Unary,
            Builtin::Fixed(Impl::Sync(Self::reverse)),
        );
    }
}

impl Iterable {
    /// `Iterable:length`; returns the number of elements.
    pub(crate) fn length(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match vals.payload(args[0])? {
            Payload::Array(elems) => Payload::Int(elems.len() as i64),
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                let len = Range::len(*start, *end, *inclusive);
                Payload::Int(len)
            }
            _ => typechecked!("Iterable:length", "Iterable instance"),
        };
        Ok(ctx.vals().add(v))
    }

    /// `Iterable:reverse`; preserves the input type.
    pub(crate) fn reverse(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match vals.payload(args[0])? {
            Payload::Array(elems) => {
                let rev: SmallVec<[ValueId; 4]> =
                    elems.iter().rev().copied().collect();
                Payload::Array(Arc::new(rev))
            }
            Payload::Range {
                start,
                end,
                inclusive,
            } => Range::rev(*start, *end, *inclusive),
            _ => typechecked!("Iterable:reverse", "Iterable instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
