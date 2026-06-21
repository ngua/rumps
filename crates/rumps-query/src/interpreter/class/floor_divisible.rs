use super::*;

/// Floor division and modulo for `Int`, `Word`, `Float`.
pub(crate) struct FloorDivisible;

impl Class for FloorDivisible {
    const ID: ClassId = ClassId::FLOOR_DIVISIBLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "floor-div",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::floor_div)),
        );
        Self::register(
            methods,
            i,
            "mod",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::modulo)),
        );
    }
}

impl FloorDivisible {
    pub(crate) fn floor_div(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let span = ctx.span();
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Payload::Int(a.div_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Payload::Word(a / b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "division by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat((a.0 / b.0).floor())))
                }
            }
            _ => typechecked!("//", "FloorDivisible instance"),
        }?;
        Ok(ctx.vals().add(v))
    }

    pub(crate) fn modulo(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let span = ctx.span();
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Payload::Int(a.rem_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Payload::Word(a % b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(span, "modulo by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat(a.0 % b.0)))
                }
            }
            _ => typechecked!("%", "FloorDivisible instance"),
        }?;
        Ok(ctx.vals().add(v))
    }
}
