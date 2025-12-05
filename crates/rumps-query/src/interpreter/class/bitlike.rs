use super::*;

/// Bitwise operations for `Bool`, `Int`, `Word`.
pub(crate) struct BitLike;

impl Class for BitLike {
    const ID: ClassId = ClassId::BIT_LIKE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "bit-and",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::and)),
        );
        Self::register(
            methods,
            i,
            "bit-or",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::or)),
        );
        Self::register(
            methods,
            i,
            "shl",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::shl)),
        );
        Self::register(
            methods,
            i,
            "shr",
            MethodAbi::Binary,
            Builtin::Fixed(Impl::Sync(Self::shr)),
        );
    }
}

impl BitLike {
    pub(crate) fn and(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a && *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a & b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a & b),
            _ => typechecked!("&", "BitLike instance"),
        };
        Ok(ctx.vals().add(v))
    }

    pub(crate) fn or(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Bool(a), Payload::Bool(b)) => Payload::Bool(*a || *b),
            (Payload::Int(a), Payload::Int(b)) => Payload::Int(a | b),
            (Payload::Word(a), Payload::Word(b)) => Payload::Word(a | b),
            _ => typechecked!("|", "BitLike instance"),
        };
        Ok(ctx.vals().add(v))
    }

    pub(crate) fn shl(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shl((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shl((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!("<<", "BitLike instance"),
        };
        Ok(ctx.vals().add(v))
    }

    pub(crate) fn shr(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let vals = ctx.vals();
        let v = match (vals.payload(args[0])?, vals.payload(args[1])?) {
            (Payload::Bool(_), Payload::Bool(_)) => Payload::Bool(false),
            (Payload::Int(a), Payload::Int(b)) => {
                Payload::Int(a.wrapping_shr((*b as u32) & 63))
            }
            (Payload::Word(a), Payload::Word(b)) => {
                Payload::Word(a.wrapping_shr((*b as u32) & (usize::BITS - 1)))
            }
            _ => typechecked!(">>", "BitLike instance"),
        };
        Ok(ctx.vals().add(v))
    }
}
