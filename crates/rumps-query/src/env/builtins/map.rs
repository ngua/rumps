use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Map, Prim};

impl Environment {
    pub(super) fn register_map_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let map_id = self.consts.strings.intern("Map");
        self.modules.insert(
            map_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "empty",
                        f: Map::empty,
                        ty: scheme!(a, forall T: Ord, U. () -> Map[T, U]),
                    },
                    PrimDef {
                        name: "length",
                        f: Map::length,
                        ty: scheme!(a, forall T: Ord, U. (Map[T, U]) -> Int),
                    },
                    PrimDef {
                        name: "keys",
                        f: Map::keys,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[T]
                        ),
                    },
                    PrimDef {
                        name: "values",
                        f: Map::values,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[U]
                        ),
                    },
                    PrimDef {
                        name: "entries",
                        f: Map::entries,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U]) -> Array[(T, U)]
                        ),
                    },
                    PrimDef {
                        name: "has",
                        f: Map::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Bool
                        ),
                    },
                    PrimDef {
                        name: "lookup",
                        f: Map::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Option[U]
                        ),
                    },
                    PrimDef {
                        name: "insert",
                        f: Map::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T, U) -> Map[T, U]
                        ),
                    },
                    PrimDef {
                        name: "remove",
                        f: Map::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], T) -> Map[T, U]
                        ),
                    },
                    PrimDef {
                        name: "merge",
                        f: Map::placeholder,
                        ty: scheme!(
                            a,
                            forall T: Ord, U. (Map[T, U], Map[T, U])
                                -> Map[T, U]
                        ),
                    },
                    PrimDef {
                        name: "from-entries",
                        f: Map::placeholder,
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
