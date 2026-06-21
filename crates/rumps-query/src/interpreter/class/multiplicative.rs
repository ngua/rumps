use super::*;

/// Multiplicative identity and multiplication for `Int`, `Word`, `Float`.
pub(crate) struct Multiplicative;

impl Class for Multiplicative {
    const ID: ClassId = ClassId::MULTIPLICATIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "one",
            MethodAbi::Nullary,
            Builtin::Fixed(Impl::Sync(Self::one)),
        );
        Self::register(
            methods,
            i,
            "mul",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::mul)),
        );
    }
}

impl Multiplicative {
    pub(crate) fn one(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let target = ctx.nullary_ty()?;
        let mut vals = ctx.vals();
        let ty = vals.ty(target);
        let v = match ty {
            Ty::Int => Payload::Int(1),
            Ty::Word => Payload::Word(1),
            Ty::Float => Payload::Float(OrderedFloat(1.0)),
            _ => typechecked!("one", "Multiplicative instance"),
        };
        Ok(vals.add_typed(v, target))
    }

    pub(crate) fn mul(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_mul(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_mul(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 * b.0))
            }
            _ => typechecked!("*", "Multiplicative instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
