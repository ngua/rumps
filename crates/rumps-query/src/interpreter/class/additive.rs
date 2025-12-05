use super::*;

/// Additive identity and addition for `Int`, `Word`, `Float`.
pub(crate) struct Additive;

impl Class for Additive {
    const ID: ClassId = ClassId::ADDITIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "zero",
            MethodAbi::Nullary,
            Builtin::Fixed(Impl::Sync(Self::zero)),
        );
        Self::register(
            methods,
            i,
            "add",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::add)),
        );
    }
}

impl Additive {
    pub(crate) fn zero(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let target = ctx.nullary_ty()?;
        let mut vals = ctx.vals();
        let ty = vals.ty(target);
        let v = match ty {
            Ty::Int => Payload::Int(0),
            Ty::Word => Payload::Word(0),
            Ty::Float => Payload::Float(OrderedFloat(0.0)),
            _ => typechecked!("zero", "Additive instance"),
        };
        Ok(vals.add_typed(v, target))
    }

    pub(crate) fn add(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_add(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_add(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 + b.0))
            }
            _ => typechecked!("+", "Additive instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
