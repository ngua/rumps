use super::*;

/// Bitwise operations for `Bool`, `Int`, `Word`.
pub(crate) struct BitLike;

impl Class for BitLike {
    const ID: ClassId = ClassId::BIT_LIKE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "bit-and", MethodFn::Binary(Self::and));
        Self::register(methods, i, "bit-or", MethodFn::Binary(Self::or));
        Self::register(methods, i, "shl", MethodFn::Binary(Self::shl));
        Self::register(methods, i, "shr", MethodFn::Binary(Self::shr));
    }
}

impl BitLike {
    pub(crate) fn and(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a && *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a & b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a & b),
            _ => typechecked!("&", "BitLike"),
        })
    }

    pub(crate) fn or(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a || *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a | b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a | b),
            _ => typechecked!("|", "BitLike"),
        })
    }

    pub(crate) fn shl(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shl((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shl((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!("<<", "BitLike"),
        })
    }

    pub(crate) fn shr(
        _: &mut ClassCtx<'_>,
        l: &Payload,
        r: &Payload,
    ) -> Result<Payload> {
        Ok(match (l, r) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shr((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shr((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!(">>", "BitLike"),
        })
    }
}
