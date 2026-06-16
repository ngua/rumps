use super::*;

/// `Foldable` class: `fold`, `fold-map` methods.
pub(crate) struct Foldable;

impl Class for Foldable {
    const ID: ClassId = ClassId::FOLDABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "fold", MethodFn::Hof(Self::fold));
        Self::register(methods, i, "fold-map", MethodFn::Hof(Self::fold_map));
    }
}

impl Foldable {
    /// Start `Foldable:fold`; returns first invocation.
    pub(crate) fn fold(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let f = args[0];
        let a = args[1];
        let b = args[2];

        // Extract data before second match to satisfy borrow checker.
        enum Kind {
            EmptyArray,
            Array(ValueId),
            Other,
        }
        let kind = match ctx.arena.payload(b) {
            Some(Payload::Array(elems)) if elems.is_empty() => Kind::EmptyArray,
            Some(Payload::Array(elems)) => Kind::Array(elems[0]),
            _ => Kind::Other,
        };
        match kind {
            Kind::EmptyArray => Ok(hof::Step::DoneValue(a)),
            Kind::Array(first) => Ok(hof::Step::Invoke(hof::Continuation {
                callee: f,
                args: smallvec![a, first],
                state: hof::State::ReduceArray {
                    source: b,
                    idx: 0,
                    acc: a,
                },
            })),
            Kind::Other => typechecked!("Foldable:fold", "Array"),
        }
    }

    /// Start `Foldable:fold-map`; requests the default accumulator first.
    pub(crate) fn fold_map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let f = args[0];
        let source = args[1];
        let out = ctx.callable_ret_ty(f, "Foldable:fold-map");

        match ctx.arena.payload(source) {
            Some(Payload::Array(_)) => {
                let method = ctx.arena.intern("default");
                Ok(hof::Step::ClassCall(hof::ClassCall {
                    class: ClassId::DEFAULT,
                    method,
                    args: SmallVec::new(),
                    output_ty: Some(out),
                    state: hof::State::FoldMapArrayDefault { source, f },
                }))
            }
            _ => typechecked!("Foldable:fold-map", "Array"),
        }
    }
}
