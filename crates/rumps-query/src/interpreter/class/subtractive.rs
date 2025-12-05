use super::*;

/// Subtraction for `Int`, `Word`, `Float`.
pub(crate) struct Subtractive;

impl Class for Subtractive {
    const ID: ClassId = ClassId::SUBTRACTIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "sub",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::sub)),
        );
    }
}

impl Subtractive {
    pub(crate) fn sub(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_sub(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_sub(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 - b.0))
            }
            _ => typechecked!("-", "Subtractive instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
