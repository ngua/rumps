use super::*;

/// `Filterable` class: `filter` method.
pub(crate) struct Filterable;

impl Class for Filterable {
    const ID: ClassId = ClassId::FILTERABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "filter", MethodFn::Hof(Self::filter));
    }
}

impl Filterable {
    /// Start `Filterable:filter`; returns first invocation or done for empty.
    pub(crate) fn filter(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let f = args[0];
        let a = args[1];

        match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => {
                Ok(hof::Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Some(Payload::Array(elems)) => {
                let first = *elems
                    .first()
                    .unwrap_or_else(|| invariant!("Array has first elem"));
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![first],
                    state: hof::State::FilterArray {
                        source: a,
                        idx: 0,
                        acc: SmallVec::new(),
                        pending: first,
                    },
                }))
            }
            _ => typechecked!("Filterable:filter", "Array"),
        }
    }
}
