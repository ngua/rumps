use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Opt;

impl Environment {
    pub(super) fn register_option_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let opt_id = self.consts.strings.intern("Option");
        self.modules.insert(
            opt_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "unwrap-or",
                        f: Opt::unwrap_or,
                        ty: scheme!(a, forall T. (Option[T], T) -> T),
                    },
                    PrimDef {
                        name: "flatten",
                        f: Opt::flatten,
                        ty: scheme!(
                            a,
                            forall T. (Option[Option[T]]) -> Option[T]
                        ),
                    },
                    PrimDef {
                        name: "note",
                        f: Opt::note,
                        ty: scheme!(
                            a,
                            forall T, U. (U, Option[T]) -> Result[T, U]
                        ),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
