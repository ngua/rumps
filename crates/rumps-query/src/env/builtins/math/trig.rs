use rumps_query_macros::scheme;

use super::super::super::{Environment, Module, PrimDef};
use crate::primitives::Trig;

impl Environment {
    pub(super) fn build_math_trig_module(&mut self) -> Module {
        let a = &mut self.ty_arena;

        let trig_mod = Module::from_prims(
            &[
                PrimDef {
                    name: "sin",
                    f: Trig::sin,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "cos",
                    f: Trig::cos,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "tan",
                    f: Trig::tan,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "asin",
                    f: Trig::asin,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "acos",
                    f: Trig::acos,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "atan",
                    f: Trig::atan,
                    ty: scheme!(a, (Float) -> Float),
                },
                PrimDef {
                    name: "atan2",
                    f: Trig::atan2,
                    ty: scheme!(a, (Float, Float) -> Float),
                },
            ],
            &mut self.consts.strings,
        );
        trig_mod
    }
}
