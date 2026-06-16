use super::*;

pub(crate) struct Wrappable;

impl Class for Wrappable {
    const ID: ClassId = ClassId::WRAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "wrap", MethodFn::Convert(Self::wrap));
    }
}

impl Wrappable {
    /// Wrap a value in a `Wrappable` container (`Option.Some` or `Result.Ok`).
    ///
    /// The target type determines whether to produce:
    /// - `Option[T]` -> `Option.Some(v)`
    /// - `Result[T, E]` -> `Result.Ok(v)`
    pub(crate) fn wrap(
        ctx: &mut ClassCtx<'_>,
        v: &Payload,
        target: &Ty,
    ) -> Result<Payload> {
        match target {
            Ty::Option(inner) => {
                let meta = ctx.runtime_types.meta(RuntimeTyId::from(*inner));
                let v_id = ctx.arena.add_typed(v.clone(), meta, ctx.span);
                Ok(Payload::some(v_id))
            }
            Ty::Result(ok, _) => {
                let meta = ctx.runtime_types.meta(RuntimeTyId::from(*ok));
                let v_id = ctx.arena.add_typed(v.clone(), meta, ctx.span);
                Ok(Payload::ok(v_id))
            }
            _ => typechecked!("wrap", "Wrappable (Option or Result)"),
        }
    }
}
