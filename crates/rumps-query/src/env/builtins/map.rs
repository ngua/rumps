use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Map;
use crate::typecheck::{Scheme, Ty, TyArena, TyId, TyVar};

impl Environment {
    pub(super) fn register_map_builtin(&mut self) {
        // Map module
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let map_kv = a.map_ty(v0, v1);
        let arr_v0 = a.array(v0);
        let arr_v1 = a.array(v1);
        let opt_v1 = a.option(v1);

        let map_empty_ty = a.func(smallvec![], map_kv);
        let map_length_ty = a.func(smallvec![map_kv], TyArena::INT);
        let map_keys_ty = a.func(smallvec![map_kv], arr_v0);
        let map_values_ty = a.func(smallvec![map_kv], arr_v1);
        let pair_kv = a.alloc(Ty::Tuple(smallvec![v0, v1]));
        let arr_pair_kv = a.array(pair_kv);
        let map_entries_ty = a.func(smallvec![map_kv], arr_pair_kv);
        let map_has_ty = a.func(smallvec![map_kv, v0], TyArena::BOOL);
        let map_lookup_ty = a.func(smallvec![map_kv, v0], opt_v1);
        let map_insert_ty = a.func(smallvec![map_kv, v0, v1], map_kv);
        let map_remove_ty = a.func(smallvec![map_kv, v0], map_kv);
        let map_merge_ty = a.func(smallvec![map_kv, map_kv], map_kv);
        let map_from_entries_ty = a.func(smallvec![arr_pair_kv], map_kv);

        let poly2 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        let map_id = self.consts.strings.intern("Map");
        self.modules.insert(
            map_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "empty",
                        f: Map::empty,
                        ty: poly2(map_empty_ty),
                    },
                    PrimDef {
                        name: "length",
                        f: Map::length,
                        ty: poly2(map_length_ty),
                    },
                    PrimDef {
                        name: "keys",
                        f: Map::keys,
                        ty: poly2(map_keys_ty),
                    },
                    PrimDef {
                        name: "values",
                        f: Map::values,
                        ty: poly2(map_values_ty),
                    },
                    PrimDef {
                        name: "entries",
                        f: Map::entries,
                        ty: poly2(map_entries_ty),
                    },
                    PrimDef {
                        name: "has",
                        f: Map::has,
                        ty: poly2(map_has_ty),
                    },
                    PrimDef {
                        name: "lookup",
                        f: Map::get,
                        ty: poly2(map_lookup_ty),
                    },
                    PrimDef {
                        name: "insert",
                        f: Map::set,
                        ty: poly2(map_insert_ty),
                    },
                    PrimDef {
                        name: "remove",
                        f: Map::remove,
                        ty: poly2(map_remove_ty),
                    },
                    PrimDef {
                        name: "merge",
                        f: Map::merge,
                        ty: poly2(map_merge_ty),
                    },
                    PrimDef {
                        name: "from-entries",
                        f: Map::from_entries,
                        ty: poly2(map_from_entries_ty),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
