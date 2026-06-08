use smallvec::SmallVec;

use super::Prim;
use crate::env::{PrimCtx, PrimResult};
use crate::value::ValueId;

pub(crate) struct Prelude;

impl Prim for Prelude {}

impl Prelude {
    /// `forall A. (A) -> A`
    pub(crate) fn identity<'a>(
        _ctx: &'a mut PrimCtx<'a>,
        args: SmallVec<[ValueId; 4]>,
    ) -> PrimResult<'a> {
        Box::pin(async move { Ok(args[0]) })
    }
}
