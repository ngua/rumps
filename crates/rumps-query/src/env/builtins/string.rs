use rumps_query_macros::scheme;

use super::super::{Environment, Module};
use crate::builtins::{Def, Impl, Str};

impl Environment {
    pub(super) fn register_string_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let str_id = self.consts.strings.intern("String");
        self.modules.insert(
            str_id,
            Module::from_defs(
                &[
                    Def {
                        name: "length",
                        imp: Impl::Sync(Str::length),
                        ty: scheme!(a, (String) -> Int),
                    },
                    Def {
                        name: "upper",
                        imp: Impl::Sync(Str::upper),
                        ty: scheme!(a, (String) -> String),
                    },
                    Def {
                        name: "lower",
                        imp: Impl::Sync(Str::lower),
                        ty: scheme!(a, (String) -> String),
                    },
                    Def {
                        name: "trim",
                        imp: Impl::Sync(Str::trim),
                        ty: scheme!(a, (String) -> String),
                    },
                    Def {
                        name: "split",
                        imp: Impl::Sync(Str::split),
                        ty: scheme!(a, (String, String) -> Array[String]),
                    },
                    Def {
                        name: "join",
                        imp: Impl::Sync(Str::join),
                        ty: scheme!(a, (Array[String], String) -> String),
                    },
                    Def {
                        name: "slice",
                        imp: Impl::Sync(Str::slice),
                        ty: scheme!(a, (String, Int, Int) -> String),
                    },
                    Def {
                        name: "contains",
                        imp: Impl::Sync(Str::contains),
                        ty: scheme!(a, (String, String) -> Bool),
                    },
                    Def {
                        name: "replace",
                        imp: Impl::Sync(Str::replace),
                        ty: scheme!(a, (String, String, String) -> String),
                    },
                    Def {
                        name: "escape",
                        imp: Impl::Sync(Str::escape),
                        ty: scheme!(a, (String) -> String),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
