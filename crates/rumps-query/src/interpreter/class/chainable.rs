use super::*;

/// Monadic chaining for `Option` and `Result`.
pub(crate) struct Chainable;

impl Class for Chainable {
    const ID: ClassId = ClassId::CHAINABLE;

    fn register_all(methods: &mut ClassMethods, i: &mut StringInterner) {
        Self::register(methods, i, "chain", MethodFn::Hof(Self::chain));
    }
}

/// `Chainable` class: `chain` method.
impl Chainable {
    /// Start `Chainable:chain`; single invocation for Some/Ok, or done for None/Err.
    pub(crate) fn chain(
        ctx: &mut ClassCtx<'_>,
        args: &[ValueId],
    ) -> Result<hof::Step> {
        let a = args[0];
        let f = args[1];

        let ty = ctx.value_base_type(a);
        match ctx.arena.payload(a) {
            // Option.None -> None
            Some(Payload::Variant { tag: 0, .. })
                if ty == Some(TypeId::OPTION) =>
            {
                Ok(hof::Step::Done(Payload::none()))
            }
            // Option.Some(v) -> invoke fn(v)
            Some(Payload::Variant {
                tag: 1,
                vals: payloads,
            }) if ty == Some(TypeId::OPTION) => {
                let inner = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Some has payload"));
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![inner],
                    state: hof::State::Chain {
                        wrapper: hof::ChainWrapper::OptionSome,
                    },
                }))
            }
            // Result.Ok(v) -> invoke fn(v)
            Some(Payload::Variant {
                tag: 0,
                vals: payloads,
            }) if ty == Some(TypeId::RESULT) => {
                let inner = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Ok has payload"));
                Ok(hof::Step::Invoke(hof::Continuation {
                    callee: f,
                    args: smallvec![inner],
                    state: hof::State::Chain {
                        wrapper: hof::ChainWrapper::ResultOk,
                    },
                }))
            }
            // Result.Err(e) -> propagate error unchanged
            Some(Payload::Variant {
                tag: 1,
                vals: payloads,
            }) if ty == Some(TypeId::RESULT) => {
                let err = payloads
                    .first()
                    .copied()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                Ok(hof::Step::Done(Payload::Variant {
                    tag: 1,
                    vals: smallvec![err],
                }))
            }
            _ => typechecked!("Chainable:chain", "Option or Result"),
        }
    }
}
