use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Map};

impl Environment {
    pub(super) fn register_map_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let map_id = self.consts.strings.intern("Map");
        self.modules.insert(
            map_id,
            Module::from_defs(
                &[
                    Def {
                        name: "empty",
                        imp: Impl::Sync(Map::empty),
                        ty: scheme!(a, forall T: Ord, U. () -> Map[T, U]),
                    },
                    Def {
                        name: "length",
                        imp: Impl::Sync(Map::length),
                        ty: scheme!(a, forall T: Ord, U. (Map[T, U]) -> Int),
                    },
                    Def {
                        name: "keys",
                        imp: Impl::Sync(Map::keys),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[T]
                        ),
                    },
                    Def {
                        name: "values",
                        imp: Impl::Sync(Map::values),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[U]
                        ),
                    },
                    Def {
                        name: "entries",
                        imp: Impl::Sync(Map::entries),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[(T, U)]
                        ),
                    },
                    Def {
                        name: "map",
                        imp: Impl::Async(Map::map),
                        ty: scheme!(
                            a,
                            forall K: Ord, V, W. ((V) -> W, Map[K, V]) -> Map[K, W]
                        ),
                    },
                    Def {
                        name: "map-with-key",
                        imp: Impl::Async(Map::map_with_key),
                        ty: scheme!(
                            a,
                            forall K: Ord, V, W. ((K, V) -> W, Map[K, V]) -> Map[K, W]
                        ),
                    },
                    Def {
                        name: "foreach",
                        imp: Impl::Async(Map::foreach),
                        ty: scheme!(a, forall K: Ord, V, W. ((V) -> W, Map[K, V]) -> Unit),
                    },
                    Def {
                        name: "foreach-with-key",
                        imp: Impl::Async(Map::foreach_with_key),
                        ty: scheme!(
                            a,
                            forall K: Ord, V, W. ((K, V) -> W, Map[K, V]) -> Unit
                        ),
                    },
                    Def {
                        name: "fold",
                        imp: Impl::Async(Map::fold),
                        ty: scheme!(
                            a,
                            forall K: Ord, V, A. ((A, V) -> A, A, Map[K, V]) -> A
                        ),
                    },
                    Def {
                        name: "fold-with-key",
                        imp: Impl::Async(Map::fold_with_key),
                        ty: scheme!(
                            a,
                            forall K: Ord, V, A. ((A, K, V) -> A, A, Map[K, V]) -> A
                        ),
                    },
                    Def {
                        name: "map-entries",
                        imp: Impl::Async(Map::map_entries),
                        ty: scheme!(
                            a,
                            forall K: Ord, L: Ord, V, W. ((K, V) -> (L, W), Map[K, V])
                                -> Map[L, W]
                        ),
                    },
                    Def {
                        name: "has",
                        imp: Impl::Async(Map::has),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Bool
                        ),
                    },
                    Def {
                        name: "lookup",
                        imp: Impl::Async(Map::lookup),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Option[U]
                        ),
                    },
                    Def {
                        name: "insert",
                        imp: Impl::Async(Map::insert),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T, U) -> Map[T, U]
                        ),
                    },
                    Def {
                        name: "remove",
                        imp: Impl::Async(Map::remove),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Map[T, U]
                        ),
                    },
                    Def {
                        name: "merge",
                        imp: Impl::Async(Map::merge),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], Map[T, U])
                                -> Map[T, U]
                        ),
                    },
                    Def {
                        name: "from-entries",
                        imp: Impl::Async(Map::from_entries),
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Array[(T, U)]) -> Map[T, U]
                        ),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
