use super::*;

/// Multiplicative identity and multiplication for `Int`, `Word`, `Float`.
pub(crate) struct Multiplicative;

impl Class for Multiplicative {
    const ID: ClassId = ClassId::MULTIPLICATIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "one", MethodFn::Nullary(Self::one));
        Self::register(methods, i, "mul", MethodFn::Binary(Self::mul));
    }
}

impl Multiplicative {
    pub(crate) fn one(_: &mut ClassCtx<'_>, ty: &Ty) -> Result<Payload> {
        Ok(match ty {
            Ty::Int => Payload::Int(1),
            Ty::Word => Payload::Word(1),
            Ty::Float => Payload::Float(OrderedFloat(1.0)),
            _ => typechecked!("one", "Multiplicative type"),
        })
    }

    pub(crate) fn mul(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_mul(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_mul(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 * b.0))
            }
            _ => typechecked!("*", "same Numeric type"),
        })
    }
}
