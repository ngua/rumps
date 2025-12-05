use super::*;

pub(crate) struct Wrappable;

impl Class for Wrappable {
    const ID: ClassId = ClassId::WRAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "wrap",
            MethodAbi::Convert,
            Builtin::Fixed(Impl::Sync(Self::wrap)),
        );
    }
}

impl Wrappable {
    pub(crate) fn wrap(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        args: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let id = args[0];
        let target = ctx.convert_target()?;
        let edge = ctx.approved_edge();
        let mut vals = ctx.vals();
        let ty = vals.ty(target);
        let id = match edge {
            Some(meta) => vals.id_with_meta(id, meta),
            None => match &ty {
                Ty::Option(inner) | Ty::Result(inner, _) => {
                    let v = vals.payload(id)?.clone();
                    vals.add_typed(v, RuntimeTyId::from(*inner))
                }
                _ => typechecked!("wrap", "Wrappable instance"),
            },
        };
        match ty {
            Ty::Option(_) => Ok(vals.add_typed(Payload::some(id), target)),
            Ty::Result(_, _) => Ok(vals.add_typed(Payload::ok(id), target)),
            _ => typechecked!("wrap", "Wrappable instance"),
        }
    }
}
