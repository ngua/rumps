mod trig;

use std::f64;

use ordered_float::OrderedFloat;
use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Math};
use crate::typecheck::{RuntimeTyId, TyArena};
use crate::value::{Payload, ValueMeta};
use crate::Span;

impl Environment {
    pub(super) fn register_math_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let mut math_module = Module::from_defs(
            &[
                Def {
                    name: "abs",
                    imp: Impl::Sync(Math::abs),
                    ty: scheme!(a, forall T: Numeric. (T) -> T),
                },
                Def {
                    name: "min",
                    imp: Impl::Sync(Math::min),
                    ty: scheme!(a, forall T: Numeric. (T, T) -> T),
                },
                Def {
                    name: "max",
                    imp: Impl::Sync(Math::max),
                    ty: scheme!(a, forall T: Numeric. (T, T) -> T),
                },
                Def {
                    name: "floor",
                    imp: Impl::Sync(Math::floor),
                    ty: scheme!(a, (Float) -> Int),
                },
                Def {
                    name: "ceil",
                    imp: Impl::Sync(Math::ceil),
                    ty: scheme!(a, (Float) -> Int),
                },
                Def {
                    name: "round",
                    imp: Impl::Sync(Math::round),
                    ty: scheme!(a, (Float) -> Int),
                },
                Def {
                    name: "sqrt",
                    imp: Impl::Sync(Math::sqrt),
                    ty: scheme!(a, (Float) -> Float),
                },
                Def {
                    name: "log",
                    imp: Impl::Sync(Math::log),
                    ty: scheme!(a, (Float) -> Float),
                },
            ],
            &mut self.consts.strings,
        );

        // Math constants (intern into `consts` arena)
        [
            ("pi", f64::consts::PI),
            ("e", f64::consts::E),
            ("tau", f64::consts::TAU),
            ("inf", f64::INFINITY),
            ("neg-inf", f64::NEG_INFINITY),
        ]
        .iter()
        .for_each(|&(name, val)| {
            let id = self.consts.add_typed(
                Payload::Float(OrderedFloat(val)),
                ValueMeta {
                    ty: RuntimeTyId::from(TyArena::FLOAT),
                    repr: RuntimeTyId::from(TyArena::FLOAT),
                },
                Span::MODULE_CONST,
            );
            let name_id = self.consts.strings.intern(name);
            math_module.add_const(name_id, id, TyArena::FLOAT);
        });

        let trig_mod = self.build_math_trig_module();
        let math_id = self.consts.strings.intern("Math");
        let trig_id = self.consts.strings.intern("Trig");
        self.modules
            .insert(math_id, math_module.with_submodule(trig_id, trig_mod));
    }
}
