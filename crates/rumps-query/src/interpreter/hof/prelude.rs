use smallvec::smallvec;

use super::{Continuation, Registry, ResultMode, State, Step};
use crate::intern::StringInterner;
use crate::interpreter::class::ClassCtx;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// HoF starters for `Prelude` module functions.
pub(super) struct Fns;

impl Fns {
    pub(super) fn register(reg: &mut Registry, i: &mut StringInterner) {
        let prelude = i.intern("Prelude");
        let foreach = i.intern("foreach");
        reg.register(prelude, foreach, Self::foreach, ResultMode::Discard);
    }

    /// `Prelude.foreach(fn, src)`; invokes `fn` for effects and returns `Unit`.
    fn foreach(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        enum Kind {
            Empty,
            Array(ValueId),
            Once(ValueId),
            Other,
        }

        let f = args[0];
        let a = args[1];

        let ty = ctx.arena.meta(a).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match ctx.arena.payload(a) {
            Some(Payload::Array(elems)) => match elems.first() {
                Some(first) => Kind::Array(*first),
                None => Kind::Empty,
            },
            Some(Payload::Variant { tag: 1, vals })
                if ty.is_some_and(|t| t == TypeId::OPTION) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            Some(Payload::Variant { tag: 0, .. })
                if ty.is_some_and(|t| t == TypeId::OPTION) =>
            {
                Kind::Empty
            }
            Some(Payload::Variant { tag: 0, vals })
                if ty.is_some_and(|t| t == TypeId::RESULT) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                )
            }
            Some(Payload::Variant { tag: 1, .. })
                if ty.is_some_and(|t| t == TypeId::RESULT) =>
            {
                Kind::Empty
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Empty => Ok(Step::Done(Payload::Unit)),
            Kind::Array(first) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: smallvec![first],
                state: State::PreludeForeachArray { source: a, idx: 0 },
            })),
            Kind::Once(inner) => Ok(Step::Invoke(Continuation {
                callee: f,
                args: smallvec![inner],
                state: State::PreludeForeachOnce,
            })),
            Kind::Other => typechecked!("Prelude.foreach", "Mappable"),
        }
    }
}
