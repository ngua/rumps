use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Range};

impl Environment {
    pub(super) fn register_range_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let range_id = self.consts.strings.intern("Range");
        self.modules.insert(
            range_id,
            Module::from_defs(
                &[
                    Def {
                        name: "collect",
                        imp: Impl::Sync(Range::collect),
                        ty: scheme!(a, (Range) -> Array[Int]),
                    },
                    Def {
                        name: "contains",
                        imp: Impl::Sync(Range::contains),
                        ty: scheme!(a, (Int, Range) -> Bool),
                    },
                    Def {
                        name: "extend",
                        imp: Impl::Sync(Range::extend),
                        ty: scheme!(a, (Word, Range) -> Range),
                    },
                    Def {
                        name: "is-empty",
                        imp: Impl::Sync(Range::is_empty),
                        ty: scheme!(a, (Range) -> Bool),
                    },
                    Def {
                        name: "first",
                        imp: Impl::Sync(Range::first),
                        ty: scheme!(a, (Range) -> Option[Int]),
                    },
                    Def {
                        name: "last",
                        imp: Impl::Sync(Range::last),
                        ty: scheme!(a, (Range) -> Option[Int]),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
