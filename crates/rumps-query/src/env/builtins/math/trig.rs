use rumps_query_macros::scheme;

use super::super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Trig};

impl Environment {
    pub(super) fn build_math_trig_module(&mut self) -> Module {
        let a = &mut self.ty_arena;

        Module::from_defs(
            &[
                Def {
                    name: "sin",
                    imp: Impl::Sync(Trig::sin),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "cos",
                    imp: Impl::Sync(Trig::cos),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "tan",
                    imp: Impl::Sync(Trig::tan),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "asin",
                    imp: Impl::Sync(Trig::asin),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "acos",
                    imp: Impl::Sync(Trig::acos),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "atan",
                    imp: Impl::Sync(Trig::atan),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "atan2",
                    imp: Impl::Sync(Trig::atan2),
                    ty: scheme!(a, (Float, Float) -> Float),
                },
            ],
            &mut self.consts.strings,
        )
    }
}
