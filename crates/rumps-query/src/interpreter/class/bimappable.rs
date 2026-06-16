use super::*;

/// `Bimappable` class: `bimap` method.
pub(crate) struct Bimappable;

impl Class for Bimappable {
    const ID: ClassId = ClassId::BIMAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "bimap", MethodFn::Hof(Self::bimap));
    }
}

impl Bimappable {
    pub(crate) fn bimap(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let f = args[0];
        let g = args[1];
        let a = args[2];

        enum Kind {
            ResultOk(ValueId),
            ResultErr(ValueId),
            Tuple(ValueId, ValueId),
            Other,
        }

        let ty = ctx.value_base_type(a);
        let kind = match ctx.arena.payload(a) {
            Some(Payload::Variant {
                tag: 0,
                vals: payloads,
            }) if ty == Some(TypeId::RESULT) => Kind::ResultOk(
                *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Ok has payload")),
            ),
            Some(Payload::Variant {
                tag: 1,
                vals: payloads,
            }) if ty == Some(TypeId::RESULT) => Kind::ResultErr(
                *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Err has payload")),
            ),
            Some(Payload::Tuple(elems)) => {
                let a = *elems
                    .first()
                    .unwrap_or_else(|| invariant!("bimap tuple has 2 elems"));
                let b = *elems
                    .get(1)
                    .unwrap_or_else(|| invariant!("bimap tuple has 2 elems"));
                Kind::Tuple(a, b)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::ResultOk(inner) => Ok(hof::Step::Invoke(hof::Continuation {
                callee: f,
                args: smallvec![inner],
                state: hof::State::BimapResult { tag: 0 },
            })),
            Kind::ResultErr(inner) => {
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: g,
                    args: smallvec![inner],
                    state: hof::State::BimapResult { tag: 1 },
                }))
            }
            Kind::Tuple(a, b) => Ok(hof::Step::Invoke(hof::Continuation {
                callee: f,
                args: smallvec![a],
                state: hof::State::BimapTuple {
                    second_fn: g,
                    second_elem: b,
                    first_result: None,
                },
            })),
            Kind::Other => typechecked!("Bimappable:bimap", "Bimappable"),
        }
    }
}
