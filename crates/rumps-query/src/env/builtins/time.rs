use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Time};

impl Environment {
    pub(super) fn register_time_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let time_id = self.consts.strings.intern("Time");
        self.modules.insert(
            time_id,
            Module::from_defs(
                &[
                    Def {
                        name: "now",
                        imp: Impl::Sync(Time::now),
                        ty: scheme!(a, () -> Time),
                    },
                    Def {
                        name: "epoch",
                        imp: Impl::Sync(Time::epoch),
                        ty: scheme!(a, () -> Time),
                    },
                    Def {
                        name: "parse",
                        imp: Impl::Sync(Time::parse),
                        ty: scheme!(
                            a,
                            (String, String) -> Result[Time, String]
                        ),
                    },
                    Def {
                        name: "format",
                        imp: Impl::Sync(Time::format),
                        ty: scheme!(a, (String, Time) -> String),
                    },
                    Def {
                        name: "add-seconds",
                        imp: Impl::Sync(Time::add_seconds),
                        ty: scheme!(a, (Time, Int) -> Time),
                    },
                    Def {
                        name: "diff-seconds",
                        imp: Impl::Sync(Time::diff_seconds),
                        ty: scheme!(a, (Time, Time) -> Float),
                    },
                    Def {
                        name: "year",
                        imp: Impl::Sync(Time::year),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "month",
                        imp: Impl::Sync(Time::month),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "day",
                        imp: Impl::Sync(Time::day),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "hour",
                        imp: Impl::Sync(Time::hour),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "minute",
                        imp: Impl::Sync(Time::minute),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "second",
                        imp: Impl::Sync(Time::second),
                        ty: scheme!(a, (Time) -> Int),
                    },
                    Def {
                        name: "sleep",
                        imp: Impl::Async(Time::sleep),
                        ty: scheme!(a, (Int) -> Unit),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
