use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::{Prim, Res};
use crate::typecheck::{Scheme, TyId, TyVar};

impl Environment {
    pub(super) fn register_result_builtin(&mut self) {
        // Result module
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let v1 = a.var(1);
        let v2 = a.var(2);
        let opt_v0 = a.option(v0);

        // `map-err: (Result[T, U], (U) -> V) -> Result[T, V]`
        let res_tu = a.result(v0, v1);
        let res_tw = a.result(v0, v2);
        let res_map_err_cb = a.func(smallvec![v1], v2);
        let res_map_err_ty = a.func(smallvec![res_tu, res_map_err_cb], res_tw);
        // `unwrap-or: (Result[T, U], T) -> T`
        let res_unwrap_or_ty = a.func(smallvec![res_tu, v0], v0);
        // `flatten: (Result[Result[T, U], U]) -> Result[T, U]`
        let inner_res = a.result(v0, v1);
        let outer_res = a.result(inner_res, v1);
        let res_flatten_ty = a.func(smallvec![outer_res], inner_res);
        // `hush: (Result[T, U]) -> Option[T]`
        let res_hush_ty = a.func(smallvec![res_tu], opt_v0);

        let poly2 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };
        let poly3 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1), TyVar::new(2)],
            ty,
            constraints: smallvec![],
        };

        let res_id = self.consts.strings.intern("Result");
        self.modules.insert(
            res_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "map-err",
                        f: Res::placeholder,
                        ty: poly3(res_map_err_ty),
                    },
                    PrimDef {
                        name: "unwrap-or",
                        f: Res::unwrap_or,
                        ty: poly2(res_unwrap_or_ty),
                    },
                    PrimDef {
                        name: "flatten",
                        f: Res::flatten,
                        ty: poly2(res_flatten_ty),
                    },
                    PrimDef {
                        name: "hush",
                        f: Res::hush,
                        ty: poly2(res_hush_ty),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
