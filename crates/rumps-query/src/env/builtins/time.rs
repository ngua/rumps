use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Time;
use crate::typecheck::{Scheme, TyArena};

impl Environment {
    pub(super) fn register_time_builtin(&mut self) {
        // Time module
        let a = &mut self.ty_arena;
        let thunk_time = a.func(smallvec![], TyArena::TIME);
        let time_parse_ret = a.result(TyArena::TIME, TyArena::STRING);
        let time_parse_ty =
            a.func(smallvec![TyArena::STRING, TyArena::STRING], time_parse_ret);
        let time_format_ty =
            a.func(smallvec![TyArena::STRING, TyArena::TIME], TyArena::STRING);
        let time_add_ty =
            a.func(smallvec![TyArena::TIME, TyArena::INT], TyArena::TIME);
        let time_diff_ty =
            a.func(smallvec![TyArena::TIME, TyArena::TIME], TyArena::FLOAT);
        let time_to_int = a.func(smallvec![TyArena::TIME], TyArena::INT);
        let int_to_unit = a.func(smallvec![TyArena::INT], TyArena::UNIT);

        let time_id = self.consts.strings.intern("Time");
        self.modules.insert(
            time_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "now",
                        f: Time::now,
                        ty: Scheme::mono(thunk_time),
                    },
                    PrimDef {
                        name: "epoch",
                        f: Time::epoch,
                        ty: Scheme::mono(thunk_time),
                    },
                    PrimDef {
                        name: "parse",
                        f: Time::parse,
                        ty: Scheme::mono(time_parse_ty),
                    },
                    PrimDef {
                        name: "format",
                        f: Time::format,
                        ty: Scheme::mono(time_format_ty),
                    },
                    PrimDef {
                        name: "add-seconds",
                        f: Time::add_seconds,
                        ty: Scheme::mono(time_add_ty),
                    },
                    PrimDef {
                        name: "diff-seconds",
                        f: Time::diff_seconds,
                        ty: Scheme::mono(time_diff_ty),
                    },
                    PrimDef {
                        name: "year",
                        f: Time::year,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "month",
                        f: Time::month,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "day",
                        f: Time::day,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "hour",
                        f: Time::hour,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "minute",
                        f: Time::minute,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "second",
                        f: Time::second,
                        ty: Scheme::mono(time_to_int),
                    },
                    PrimDef {
                        name: "sleep",
                        f: Time::sleep,
                        ty: Scheme::mono(int_to_unit),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
