use super::*;

/// Unary negation for `Int`, `Float`.
pub(crate) struct Negatable;

impl Class for Negatable {
    const ID: ClassId = ClassId::NEGATABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "neg", MethodFn::Unary(Self::neg));
    }
}

impl Negatable {
    pub(crate) fn neg(_: &mut ClassCtx<'_>, v: &Payload) -> Result<Payload> {
        Ok(match v {
            Payload::Int(n) => Payload::Int(-n),
            Payload::Float(f) => Payload::Float(OrderedFloat(-f.0)),
            _ => typechecked!("-", "Negatable"),
        })
    }
}
