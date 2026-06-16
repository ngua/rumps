use super::*;

/// `Iterable` class: `length`, `reverse` methods.
pub(crate) struct Iterable;

impl Class for Iterable {
    const ID: ClassId = ClassId::ITERABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "length", MethodFn::Unary(Self::length));
        Self::register(methods, i, "reverse", MethodFn::Unary(Self::reverse));
    }
}

impl Iterable {
    /// `Iterable:length`; returns the number of elements.
    pub(crate) fn length(_: &mut ClassCtx<'_>, v: &Payload) -> Result<Payload> {
        Ok(match v {
            Payload::Array(elems) => Payload::Int(elems.len() as i64),
            Payload::Range {
                start,
                end,
                inclusive,
            } => {
                let len = Range::len(*start, *end, *inclusive);
                Payload::Int(len)
            }
            _ => typechecked!("Iterable:length", "Iterable"),
        })
    }

    /// `Iterable:reverse`; preserves the input type.
    pub(crate) fn reverse(
        _: &mut ClassCtx<'_>,
        v: &Payload,
    ) -> Result<Payload> {
        Ok(match v {
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
            _ => typechecked!("Iterable:reverse", "Iterable"),
        })
    }
}
