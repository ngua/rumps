use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Res};

impl Environment {
    pub(super) fn register_result_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let res_id = self.consts.strings.intern("Result");
        self.modules.insert(
            res_id,
            Module::from_defs(
                &[
                    Def {
                        name: "map-err",
                        imp: Impl::Async(Res::map_err),
                        ty: scheme!(
                            a,
                            forall T, U, V. (Result[T, U], (U) -> V) -> Result[T, V]
                        ),
                    },
                    Def {
                        name: "unwrap-or",
                        imp: Impl::Sync(Res::unwrap_or),
                        ty: scheme!(a, forall T, U. (Result[T, U], T) -> T),
                    },
                    Def {
                        name: "flatten",
                        imp: Impl::Sync(Res::flatten),
                        ty: scheme!(
                            a,
                            forall T, U. (Result[Result[T, U], U]) -> Result[T, U]
                        ),
                    },
                    Def {
                        name: "hush",
                        imp: Impl::Sync(Res::hush),
                        ty: scheme!(
                            a,
                            forall T, U. (Result[T, U]) -> Option[T]
                        ),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
