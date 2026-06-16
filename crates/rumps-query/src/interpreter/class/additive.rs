use super::*;

/// Additive identity and addition for `Int`, `Word`, `Float`.
pub(crate) struct Additive;

impl Class for Additive {
    const ID: ClassId = ClassId::ADDITIVE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "zero", MethodFn::Nullary(Self::zero));
        Self::register(methods, i, "add", MethodFn::Binary(Self::add));
    }
}

impl Additive {
    pub(crate) fn zero(_: &mut ClassCtx<'_>, ty: &Ty) -> Result<Payload> {
        Ok(match ty {
            Ty::Int => Payload::Int(0),
            Ty::Word => Payload::Word(0),
            Ty::Float => Payload::Float(OrderedFloat(0.0)),
            _ => typechecked!("zero", "Additive type"),
        })
    }

    pub(crate) fn add(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_add(*b))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.saturating_add(*b))
            }
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0 + b.0))
            }
            _ => typechecked!("+", "same Numeric type"),
        })
    }
}
