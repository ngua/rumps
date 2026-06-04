use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Time;

impl Environment {
    pub(super) fn register_time_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let time_id = self.consts.strings.intern("Time");
        self.modules.insert(
            time_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "now",
                        f: Time::now,
                        ty: scheme!(a, () -> Time),
                    },
                    PrimDef {
                        name: "epoch",
                        f: Time::epoch,
                        ty: scheme!(a, () -> Time),
                    },
                    PrimDef {
                        name: "parse",
                        f: Time::parse,
                        ty: scheme!(
                            a,
                            (String, String) -> Result[Time, String]
                        ),
                    },
                    PrimDef {
                        name: "format",
                        f: Time::format,
                        ty: scheme!(a, (String, Time) -> String),
                    },
                    PrimDef {
                        name: "add-seconds",
                        f: Time::add_seconds,
                        ty: scheme!(a, (Time, Int) -> Time),
                    },
                    PrimDef {
                        name: "diff-seconds",
                        f: Time::diff_seconds,
                        ty: scheme!(a, (Time, Time) -> Float),
                    },
                    PrimDef {
                        name: "year",
                        f: Time::year,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "month",
                        f: Time::month,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "day",
                        f: Time::day,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "hour",
                        f: Time::hour,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "minute",
                        f: Time::minute,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "second",
                        f: Time::second,
                        ty: scheme!(a, (Time) -> Int),
                    },
                    PrimDef {
                        name: "sleep",
                        f: Time::sleep,
                        ty: scheme!(a, (Int) -> Unit),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
