use super::*;

/// Exponentiation for `Int`, `Word`, `Float`.
pub(crate) struct Powerable;

impl Class for Powerable {
    const ID: ClassId = ClassId::POWERABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "pow", MethodFn::Binary(Self::pow));
    }
}

impl Powerable {
    pub(crate) fn pow(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Int(base), Payload::Int(exp)) => {
                if *exp < 0 {
                    Payload::Float(OrderedFloat(
                        (*base as f64).powf(*exp as f64),
                    ))
                } else {
                    u32::try_from(*exp)
                        .ok()
                        .and_then(|e| base.checked_pow(e))
                        .map_or_else(
                            || {
                                Payload::Float(OrderedFloat(
                                    (*base as f64).powf(*exp as f64),
                                ))
                            },
                            Payload::Int,
                        )
                }
            }
            (Payload::Word(base), Payload::Word(exp)) => Payload::Word(
                u32::try_from(*exp)
                    .ok()
                    .and_then(|e| base.checked_pow(e))
                    .unwrap_or(usize::MAX),
            ),
            (Payload::Float(a), Payload::Float(b)) => {
                Payload::Float(OrderedFloat(a.0.powf(b.0)))
            }
            _ => typechecked!("**", "same Numeric type"),
        })
    }
}
