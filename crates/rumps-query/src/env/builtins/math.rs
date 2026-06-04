mod trig;

use std::f64;

use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Math;
use crate::typecheck::{RuntimeTyId, Scheme, TyArena, TyId, TyVar, TypeClass};
use crate::value::{Payload, ValueMeta};
use crate::{ClassId, Span};

impl Environment {
    pub(super) fn register_math_builtin(&mut self) {
        // Math module
        let a = &mut self.ty_arena;
        let v0 = a.var(0);

        // `forall T: Numeric. (T) -> T`
        let num_unary = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0)],
            ty,
            constraints: smallvec![(
                TyVar::new(0),
                TypeClass::simple(ClassId::NUMERIC)
            )],
        };
        // `forall T: Numeric. (T, T) -> T`
        let num_binary = |ty: TyId| Scheme {
            vars: smallvec![TyVar::new(0)],
            ty,
            constraints: smallvec![(
                TyVar::new(0),
                TypeClass::simple(ClassId::NUMERIC)
            )],
        };

        let abs_ty = a.func(smallvec![v0], v0);
        let minmax_ty = a.func(smallvec![v0, v0], v0);
        let float_to_int = a.func(smallvec![TyArena::FLOAT], TyArena::INT);
        let float_to_float = a.func(smallvec![TyArena::FLOAT], TyArena::FLOAT);

        let mut math_module = Module::from_prims(
            &[
                PrimDef {
                    name: "abs",
                    f: Math::abs,
                    ty: num_unary(abs_ty),
                },
                PrimDef {
                    name: "min",
                    f: Math::min,
                    ty: num_binary(minmax_ty),
                },
                PrimDef {
                    name: "max",
                    f: Math::max,
                    ty: num_binary(minmax_ty),
                },
                PrimDef {
                    name: "floor",
                    f: Math::floor,
                    ty: Scheme::mono(float_to_int),
                },
                PrimDef {
                    name: "ceil",
                    f: Math::ceil,
                    ty: Scheme::mono(float_to_int),
                },
                PrimDef {
                    name: "round",
                    f: Math::round,
                    ty: Scheme::mono(float_to_int),
                },
                PrimDef {
                    name: "sqrt",
                    f: Math::sqrt,
                    ty: Scheme::mono(float_to_float),
                },
                PrimDef {
                    name: "log",
                    f: Math::log,
                    ty: Scheme::mono(float_to_float),
                },
            ],
            &mut self.consts.strings,
        );

        // Math constants (intern into `consts` arena)
        use ordered_float::OrderedFloat;
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
