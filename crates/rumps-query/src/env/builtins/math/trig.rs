use smallvec::smallvec;

use super::super::super::{Environment, Module, PrimDef};
use crate::primitives::Trig;
use crate::typecheck::{Scheme, TyArena};

impl Environment {
    pub(super) fn build_math_trig_module(&mut self) -> Module {
        let a = &mut self.ty_arena;
        let float_to_float = a.func(smallvec![TyArena::FLOAT], TyArena::FLOAT);
        let float2_to_float =
            a.func(smallvec![TyArena::FLOAT, TyArena::FLOAT], TyArena::FLOAT);

        let trig_mod = Module::from_prims(
            &[
                PrimDef {
                    name: "sin",
                    f: Trig::sin,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "cos",
                    f: Trig::cos,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "tan",
                    f: Trig::tan,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "asin",
                    f: Trig::asin,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "acos",
                    f: Trig::acos,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "atan",
                    f: Trig::atan,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "atan2",
                    f: Trig::atan2,
                    ty: Scheme::mono(float2_to_float),
                },
            ],
            &mut self.consts.strings,
        );
        trig_mod
    }
}
