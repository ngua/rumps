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

    /// `Prelude.foreach(fn, src)` - invokes `fn` for effects and returns `Unit`.
    fn foreach(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        enum Kind {
            Empty,
            Array(ValueId),
            Once(ValueId),
            Other,
        }

        let fn_id = *args
            .first()
            .unwrap_or_else(|| typechecked!("Prelude.foreach", "2 args"));
        let src = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Prelude.foreach", "2 args"));

        let src_ty = ctx.arena.meta(src).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match ctx.arena.payload(src) {
            Some(Payload::Array(elems)) => match elems.first() {
                Some(first) => Kind::Array(*first),
                None => Kind::Empty,
            },
            Some(Payload::Variant { tag: 1, vals })
                if src_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Some has payload")),
                )
            }
            Some(Payload::Variant { tag: 0, .. })
                if src_ty.is_some_and(|ty| ty == TypeId::OPTION) =>
            {
                Kind::Empty
            }
            Some(Payload::Variant { tag: 0, vals })
                if src_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Once(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                )
            }
            Some(Payload::Variant { tag: 1, .. })
                if src_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Empty
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Empty => Ok(Step::Done(Payload::Unit)),
            Kind::Array(first) => Ok(Step::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![first],
                state: State::PreludeForeachArray {
                    source: src,
                    idx: 0,
                },
            })),
            Kind::Once(inner) => Ok(Step::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: State::PreludeForeachOnce,
            })),
            Kind::Other => typechecked!("Prelude.foreach", "Mappable"),
        }
    }
}
