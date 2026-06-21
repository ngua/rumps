use super::*;

/// Default values for `Default` instances.
pub(crate) struct DefaultClass;

impl Class for DefaultClass {
    const ID: ClassId = ClassId::DEFAULT;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(
            methods,
            i,
            "default",
            MethodAbi::Nullary,
            Builtin::Fixed(Impl::Sync(Self::default)),
        );
    }
}

impl DefaultClass {
    pub(crate) fn default(
        ctx: &mut BuiltinCtx<'_, '_, '_>,
        _: SmallVec<[ValueId; 4]>,
    ) -> Result<ValueId> {
        let target = ctx.nullary_ty()?;
        let mut vals = ctx.vals();
        let ty = vals.ty(target);
        let v = match ty {
            Ty::Unit => Payload::Unit,
            Ty::Bool => Payload::Bool(false),
            Ty::String => Payload::String(vals.intern("")),
            Ty::Array(_) => Payload::Array(Arc::new(SmallVec::new())),
            Ty::Map(_, _) => Payload::Map(Arc::new(Map::new())),
            Ty::Option(_) => Payload::none(),
            Ty::Ordering => Payload::eq_ord(),
            Ty::FilePath => Payload::FilePath(vals.intern("")),
            _ => typechecked!("default", "Default instance"),
        };
        Ok(vals.add_typed(v, target))
    }
}
