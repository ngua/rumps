use rumps_query_macros::scheme;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Str;

impl Environment {
    pub(super) fn register_string_builtin(&mut self) {
        let a = &mut self.ty_arena;

        let str_id = self.consts.strings.intern("String");
        self.modules.insert(
            str_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "length",
                        f: Str::length,
                        ty: scheme!(a, (String) -> Int),
                    },
                    PrimDef {
                        name: "upper",
                        f: Str::upper,
                        ty: scheme!(a, (String) -> String),
                    },
                    PrimDef {
                        name: "lower",
                        f: Str::lower,
                        ty: scheme!(a, (String) -> String),
                    },
                    PrimDef {
                        name: "trim",
                        f: Str::trim,
                        ty: scheme!(a, (String) -> String),
                    },
                    PrimDef {
                        name: "split",
                        f: Str::split,
                        ty: scheme!(a, (String, String) -> Array[String]),
                    },
                    PrimDef {
                        name: "join",
                        f: Str::join,
                        ty: scheme!(a, (Array[String], String) -> String),
                    },
                    PrimDef {
                        name: "slice",
                        f: Str::slice,
                        ty: scheme!(a, (String, Int, Int) -> String),
                    },
                    PrimDef {
                        name: "contains",
                        f: Str::contains,
                        ty: scheme!(a, (String, String) -> Bool),
                    },
                    PrimDef {
                        name: "replace",
                        f: Str::replace,
                        ty: scheme!(a, (String, String, String) -> String),
                    },
                    PrimDef {
                        name: "escape",
                        f: Str::escape,
                        ty: scheme!(a, (String) -> String),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
