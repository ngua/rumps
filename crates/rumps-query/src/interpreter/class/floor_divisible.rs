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
            MethodFn::Binary(Self::floor_div),
        );
        Self::register(methods, i, "mod", MethodFn::Binary(Self::modulo));
    }
}

impl FloorDivisible {
    pub(crate) fn floor_div(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Int(a.div_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Word(a / b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "division by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat((a.0 / b.0).floor())))
                }
            }
            _ => typechecked!("//", "same Numeric type"),
        }
    }

    pub(crate) fn modulo(
        ctx: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Int(a.rem_euclid(*b)))
                }
            }
            (Payload::Word(a), Payload::Word(b)) => {
                if *b == 0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Word(a % b))
                }
            }
            (Payload::Float(a), Payload::Float(b)) => {
                if b.0 == 0.0 {
                    Err(Error::runtime(ctx.span, "modulo by zero"))
                } else {
                    Ok(Payload::Float(OrderedFloat(a.0 % b.0)))
                }
            }
            _ => typechecked!("%", "same Numeric type"),
        }
    }
}
