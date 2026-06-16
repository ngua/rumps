use super::*;

/// Default values for builtin `Default` instances.
pub(crate) struct DefaultClass;

impl Class for DefaultClass {
    const ID: ClassId = ClassId::DEFAULT;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "default", MethodFn::Nullary(Self::default));
    }
}

impl DefaultClass {
    /// The default value for a given type.
    ///
    /// Produces `()`, `false`, `""`, `[]`, `{}`, `Option.None`,
    /// `Ordering.Eq`, or an empty `FilePath`.
    pub(crate) fn default(ctx: &mut ClassCtx<'_>, ty: &Ty) -> Result<Payload> {
        Ok(match ty {
            Ty::Unit => Payload::Unit,
            Ty::Bool => Payload::Bool(false),
            Ty::String => Payload::String(ctx.arena.intern("")),
            Ty::Array(_) => Payload::Array(Arc::new(SmallVec::new())),
            Ty::Map(_, _) => Payload::Map(Arc::new(Map::new())),
            Ty::Option(_) => Payload::none(),
            Ty::Ordering => Payload::eq_ord(),
            Ty::FilePath => Payload::FilePath(ctx.arena.intern("")),
            _ => typechecked!("default", "Default type"),
        })
    }
}
