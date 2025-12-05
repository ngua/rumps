use super::*;

/// Unary negation for `Int`, `Float`.
pub(crate) struct Negatable;

impl Class for Negatable {
    const ID: ClassId = ClassId::NEGATABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "neg",
            MethodAbi::Unary,
            Builtin::Fixed(Impl::Sync(Self::neg)),
        );
    }
}

impl Negatable {
    pub(crate) fn neg(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match vals.payload(args[0])? {
            Payload::Int(n) => Payload::Int(-n),
            Payload::Float(f) => Payload::Float(OrderedFloat(-f.0)),
            _ => typechecked!("-", "Negatable instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
