use super::*;

/// Subtraction for `Int`, `Word`, `Float`.
pub(crate) struct Subtractive;

impl Class for Subtractive {
    const ID: ClassId = ClassId::SUBTRACTIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "sub", MethodFn::Binary(Self::sub));
    }
}

impl Subtractive {
    pub(crate) fn sub(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_sub(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_sub(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 - b.0))
            }
            _ => typechecked!("-", "same Numeric type"),
        })
    }
}
