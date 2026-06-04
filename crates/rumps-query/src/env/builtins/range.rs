use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Range;

impl Environment {
    pub(super) fn register_range_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let range_id = self.consts.strings.intern("Range");
        self.modules.insert(
            range_id,
            Module::from_prims(
                &[PrimDef {
                    name: "collect",
                    f: Range::collect,
                    ty: scheme!(a, (Range) -> Array[Int]),
                }],
                &mut self.consts.strings,
            ),
        );
    }
}
