use super::*;

/// Division for `Int`, `Word`, `Float`.
pub(crate) struct Divisible;

impl Class for Divisible {
    const ID: ClassId = ClassId::DIVISIBLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "div", MethodFn::Binary(Self::div));
    }
}

impl Divisible {
    pub(crate) fn div(
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
                    Ok(Payload::Float(OrderedFloat(a.0 / b.0)))
                }
            }
            _ => typechecked!("/", "same Numeric type"),
        }
    }
}
