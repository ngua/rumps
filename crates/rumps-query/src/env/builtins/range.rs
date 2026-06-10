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
                &[
                    PrimDef {
                        name: "collect",
                        f: Range::collect,
                        ty: scheme!(a, (Range) -> Array[Int]),
                    },
                    PrimDef {
                        name: "contains",
                        f: Range::contains,
                        ty: scheme!(a, (Int, Range) -> Bool),
                    },
                    PrimDef {
                        name: "extend",
                        f: Range::extend,
                        ty: scheme!(a, (Word, Range) -> Range),
                    },
                    PrimDef {
                        name: "is-empty",
                        f: Range::is_empty,
                        ty: scheme!(a, (Range) -> Bool),
                    },
                    PrimDef {
                        name: "first",
                        f: Range::first,
                        ty: scheme!(a, (Range) -> Option[Int]),
                    },
                    PrimDef {
                        name: "last",
                        f: Range::last,
                        ty: scheme!(a, (Range) -> Option[Int]),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
