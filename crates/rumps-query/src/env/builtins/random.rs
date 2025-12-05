use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Random};

impl Environment {
    pub(super) fn register_random_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let rand_id = self.consts.strings.intern("Random");
        self.modules.insert(
            rand_id,
            Module::from_defs(
                &[
                    Def {
                        name: "random",
                        imp: Impl::Sync(Random::random),
                        ty: scheme!(a, () -> Float),
                    },
                    Def {
                        name: "range",
                        imp: Impl::Sync(Random::range),
                        ty: scheme!(a, (Float, Float) -> Float),
                    },
                    Def {
                        name: "int",
                        imp: Impl::Sync(Random::int),
                        ty: scheme!(a, (Int, Int) -> Int),
                    },
                    Def {
                        name: "bool",
                        imp: Impl::Sync(Random::bool),
                        ty: scheme!(a, () -> Bool),
                    },
                    Def {
                        name: "choice",
                        imp: Impl::Sync(Random::choice),
                        ty: scheme!(a, forall T. (Array[T]) -> Option[T]),
                    },
                    Def {
                        name: "shuffle",
                        imp: Impl::Sync(Random::shuffle),
                        ty: scheme!(a, forall T. (Array[T]) -> Array[T]),
                    },
                    Def {
                        name: "sample",
                        imp: Impl::Sync(Random::sample),
                        ty: scheme!(
                            a,
                            forall T. (Array[T], Int) -> Result[Array[T], String]
                        ),
                    },
                    Def {
                        name: "uuid",
                        imp: Impl::Sync(Random::uuid),
                        ty: scheme!(a, () -> String),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
