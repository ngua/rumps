use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Random;
use crate::typecheck::{Scheme, TyArena, TyId, TyVar};

impl Environment {
    pub(super) fn register_random_builtin(&mut self) {
        // Random module
        let a = &mut self.ty_arena;
        let v0 = a.var(0);
        let arr_v0 = a.array(v0);
        let opt_v0 = a.option(v0);

        let thunk_float = a.func(smallvec![], TyArena::FLOAT);
        let float2_to_float =
            a.func(smallvec![TyArena::FLOAT, TyArena::FLOAT], TyArena::FLOAT);
        let int2_to_int =
            a.func(smallvec![TyArena::INT, TyArena::INT], TyArena::INT);
        let thunk_bool = a.func(smallvec![], TyArena::BOOL);
        let choice_ty = a.func(smallvec![arr_v0], opt_v0);
        let shuffle_ty = a.func(smallvec![arr_v0], arr_v0);
        let sample_ret = {
            let r = a.result(arr_v0, TyArena::STRING);
            a.func(smallvec![arr_v0, TyArena::INT], r)
        };
        let thunk_str = a.func(smallvec![], TyArena::STRING);

        let poly1 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0)],
            ty,
            constraints: smallvec![],
        };
        let _poly2 = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0), TyVar::new(1)],
            ty,
            constraints: smallvec![],
        };

        let rand_id = self.consts.strings.intern("Random");
        self.modules.insert(
            rand_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "random",
                        f: Random::random,
                        ty: Scheme::mono(thunk_float),
                    },
                    PrimDef {
                        name: "range",
                        f: Random::range,
                        ty: Scheme::mono(float2_to_float),
                    },
                    PrimDef {
                        name: "int",
                        f: Random::int,
                        ty: Scheme::mono(int2_to_int),
                    },
                    PrimDef {
                        name: "bool",
                        f: Random::bool,
                        ty: Scheme::mono(thunk_bool),
                    },
                    PrimDef {
                        name: "choice",
                        f: Random::choice,
                        ty: poly1(choice_ty),
                    },
                    PrimDef {
                        name: "shuffle",
                        f: Random::shuffle,
                        ty: poly1(shuffle_ty),
                    },
                    PrimDef {
                        name: "sample",
                        f: Random::sample,
                        ty: poly1(sample_ret),
                    },
                    PrimDef {
                        name: "uuid",
                        f: Random::uuid,
                        ty: Scheme::mono(thunk_str),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
