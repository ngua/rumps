use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Prim, Res};

impl Environment {
    pub(super) fn register_result_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let res_id = self.consts.strings.intern("Result");
        self.modules.insert(
            res_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "map-err",
                        f: Res::placeholder,
                        ty: scheme!(
                            a,
                            forall T, U, V. (Result[T, U], (U) -> V) -> Result[T, V]
                        ),
                    },
                    PrimDef {
                        name: "unwrap-or",
                        f: Res::unwrap_or,
                        ty: scheme!(a, forall T, U. (Result[T, U], T) -> T),
                    },
                    PrimDef {
                        name: "flatten",
                        f: Res::flatten,
                        ty: scheme!(
                            a,
                            forall T, U. (Result[Result[T, U], U]) -> Result[T, U]
                        ),
                    },
                    PrimDef {
                        name: "hush",
                        f: Res::hush,
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
