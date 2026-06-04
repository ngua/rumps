use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Random;

impl Environment {
    pub(super) fn register_random_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let rand_id = self.consts.strings.intern("Random");
        self.modules.insert(
            rand_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "random",
                        f: Random::random,
                        ty: scheme!(a, () -> Float),
                    },
                    PrimDef {
                        name: "range",
                        f: Random::range,
                        ty: scheme!(a, (Float, Float) -> Float),
                    },
                    PrimDef {
                        name: "int",
                        f: Random::int,
                        ty: scheme!(a, (Int, Int) -> Int),
                    },
                    PrimDef {
                        name: "bool",
                        f: Random::bool,
                        ty: scheme!(a, () -> Bool),
                    },
                    PrimDef {
                        name: "choice",
                        f: Random::choice,
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    PrimDef {
                        name: "shuffle",
                        f: Random::shuffle,
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    PrimDef {
                        name: "sample",
                        f: Random::sample,
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int) -> Result[Array[T], String]
                        ),
                    },
                    PrimDef {
                        name: "uuid",
                        f: Random::uuid,
                        ty: scheme!(a, () -> String),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
