use super::*;

/// `Mappable` class: `map` method.
pub(crate) struct Mappable;

impl Class for Mappable {
    const ID: ClassId = ClassId::MAPPABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "map", MethodFn::Hof(Self::map));
    }
}

impl Mappable {
    /// Start `Mappable:map`; returns first invocation or done for empty.
    pub(crate) fn map(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let f = args[0];
        let a = args[1];

        // Extract data to avoid borrow conflicts
        enum Kind {
            EmptyArray,
            Array(ValueId),
            Tuple(ValueId, ValueId),
            OptionSome(ValueId),
            OptionNone,
            ResultOk(ValueId),
            ResultErr(Payload),
            Other,
        }
        let ty = ctx.value_base_type(a);
        let kind = match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) if elems.is_empty() => Kind::EmptyArray,
            Some(Payload::Array(elems)) => Kind::Array(
                *elems
                    .first()
                    .unwrap_or_else(|| invariant!("Array has first elem")),
            ),
            Some(Payload::Tuple(elems)) if elems.len() == 2 => {
                let first = *elems
                    .first()
                    .unwrap_or_else(|| invariant!("Tuple has first elem"));
                let second = *elems
                    .get(1)
                    .unwrap_or_else(|| invariant!("Tuple has second elem"));
                Kind::Tuple(first, second)
            }
            // Option.Some(v) -> map inner
            Some(Payload::Variant {
                tag: 1,
                vals: payloads,
            }) if ty == Some(TypeId::OPTION) => Kind::OptionSome(
                *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Some has payload")),
            ),
            // Option.None -> return None
            Some(Payload::Variant { tag: 0, .. })
                if ty == Some(TypeId::OPTION) =>
            {
                Kind::OptionNone
            }
            // Result.Ok(v) -> map inner
            Some(Payload::Variant {
                tag: 0,
                vals: payloads,
            }) if ty == Some(TypeId::RESULT) => {
                let inner = *payloads
                    .first()
                    .unwrap_or_else(|| invariant!("Ok has payload"));
                Kind::ResultOk(inner)
            }
            // Result.Err(e) -> return unchanged
            Some(v @ Payload::Variant { tag: 1, .. })
                if ty == Some(TypeId::RESULT) =>
            {
                Kind::ResultErr(v.clone())
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::EmptyArray => {
                Ok(hof::Step::Done(Payload::Array(Arc::new(SmallVec::new()))))
            }
            Kind::Array(first) => Ok(hof::Step::Invoke(hof::Continuation {
                callee: f,
                args: smallvec![first],
                state: hof::State::MapIter {
                    kind: hof::IterKind::Array { source: a, idx: 0 },
                    acc: SmallVec::new(),
                },
            })),
            Kind::Tuple(first, second) => {
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![second],
                    state: hof::State::MapTuple { first },
                }))
            }
            Kind::OptionSome(inner) => {
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![inner],
                    state: hof::State::MapContainer {
                        ctor_ty: TypeId::OPTION,
                        tag: 1, // Some
                    },
                }))
            }
            Kind::OptionNone => Ok(hof::Step::Done(Payload::none())),
            Kind::ResultOk(inner) => {
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![inner],
                    state: hof::State::MapContainer {
                        ctor_ty: TypeId::RESULT,
                        tag: 0, // Ok
                    },
                }))
            }
            Kind::ResultErr(v) => Ok(hof::Step::Done(v)),
            Kind::Other => typechecked!("Mappable:map", "Mappable"),
        }
    }
}
