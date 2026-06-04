use smallvec::smallvec;

use super::{Continuation, Registry, ResultMode, State, Step};
use crate::intern::StringInterner;
use crate::interpreter::class::ClassCtx;
use crate::typecheck::RuntimeTyId;
use crate::value::{Payload, TypeId, ValueId};
use crate::Result;

/// HoF starters for `Result` module functions.
pub(super) struct Fns;

impl Fns {
    pub(super) fn register(reg: &mut Registry, i: &mut StringInterner) {
        let result = i.intern("Result");
        let map_err = i.intern("map-err");
        reg.register(result, map_err, Self::map_err, ResultMode::Keep);
    }

    /// `Result.map-err(res, fn)` - maps the error if Err, passes through Ok.
    fn map_err(ctx: &mut ClassCtx<'_>, args: &[ValueId]) -> Result<Step> {
        let res = *args
            .first()
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));
        let fn_id = *args
            .get(1)
            .unwrap_or_else(|| typechecked!("Result.map-err", "2 args"));

        enum Kind {
            Ok(ValueId, RuntimeTyId, RuntimeTyId),
            Err(ValueId, RuntimeTyId),
            Other,
        }
        let out_ty = ctx.callable_ret_ty(fn_id, "Result.map-err");
        let tys = ctx.result_tys(res);
        let res_ty = ctx.arena.meta(res).and_then(|m| {
            ctx.runtime_types
                .to_type_id(m.repr)
                .or_else(|| ctx.runtime_types.to_type_id(m.ty))
        });
        let kind = match (tys, ctx.arena.payload(res)) {
            // Result.Ok(v) -> return unchanged
            (Some((ok_ty, _)), Some(Payload::Variant { tag: 0, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                Kind::Ok(
                    *vals
                        .first()
                        .unwrap_or_else(|| invariant!("Ok has payload")),
                    ok_ty,
                    out_ty,
                )
            }
            // Result.Err(e) -> map error
            (Some((ok_ty, _)), Some(Payload::Variant { tag: 1, vals }))
                if res_ty.is_some_and(|ty| ty == TypeId::RESULT) =>
            {
                let inner = *vals
                    .first()
                    .unwrap_or_else(|| invariant!("Err has payload"));
                Kind::Err(inner, ok_ty)
            }
            _ => Kind::Other,
        };

        match kind {
            Kind::Ok(ok, ok_ty, err_ty) => {
                let ty = ctx.runtime_types.result(ok_ty, err_ty);
                let id = ctx.arena.add_typed(
                    Payload::ok(ok),
                    ctx.runtime_types.meta(ty),
                    ctx.span,
                );
                Ok(Step::DoneValue(id))
            }
            Kind::Err(inner, ok_ty) => Ok(Step::Invoke(Continuation {
                callee: fn_id,
                args: smallvec![inner],
                state: State::ResultMapErr { ok_ty },
            })),
            Kind::Other => typechecked!("Result.map-err", "Result"),
        }
    }
}
