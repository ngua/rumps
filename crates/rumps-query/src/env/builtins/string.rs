use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Str;
use crate::typecheck::{Scheme, TyArena};

impl Environment {
    pub(super) fn register_string_builtin(&mut self) {
        // String module
        let a = &mut self.ty_arena;
        let str_to_int = a.func(smallvec![TyArena::STRING], TyArena::INT);
        let str_to_str = a.func(smallvec![TyArena::STRING], TyArena::STRING);
        let str2_to_bool =
            a.func(smallvec![TyArena::STRING, TyArena::STRING], TyArena::BOOL);
        let str2_to_arr = {
            let arr_s = a.array(TyArena::STRING);
            a.func(smallvec![TyArena::STRING, TyArena::STRING], arr_s)
        };
        let join_ty = {
            let arr_s = a.array(TyArena::STRING);
            a.func(smallvec![arr_s, TyArena::STRING], TyArena::STRING)
        };
        let str_slice_ty = a.func(
            smallvec![TyArena::STRING, TyArena::INT, TyArena::INT],
            TyArena::STRING,
        );
        let str_replace_ty = a.func(
            smallvec![TyArena::STRING, TyArena::STRING, TyArena::STRING],
            TyArena::STRING,
        );

        let str_id = self.consts.strings.intern("String");
        self.modules.insert(
            str_id,
            Module::from_prims(
                &[
                    PrimDef {
                        name: "length",
                        f: Str::length,
                        ty: Scheme::mono(str_to_int),
                    },
                    PrimDef {
                        name: "upper",
                        f: Str::upper,
                        ty: Scheme::mono(str_to_str),
                    },
                    PrimDef {
                        name: "lower",
                        f: Str::lower,
                        ty: Scheme::mono(str_to_str),
                    },
                    PrimDef {
                        name: "trim",
                        f: Str::trim,
                        ty: Scheme::mono(str_to_str),
                    },
                    PrimDef {
                        name: "split",
                        f: Str::split,
                        ty: Scheme::mono(str2_to_arr),
                    },
                    PrimDef {
                        name: "join",
                        f: Str::join,
                        ty: Scheme::mono(join_ty),
                    },
                    PrimDef {
                        name: "slice",
                        f: Str::slice,
                        ty: Scheme::mono(str_slice_ty),
                    },
                    PrimDef {
                        name: "contains",
                        f: Str::contains,
                        ty: Scheme::mono(str2_to_bool),
                    },
                    PrimDef {
                        name: "replace",
                        f: Str::replace,
                        ty: Scheme::mono(str_replace_ty),
                    },
                    PrimDef {
                        name: "escape",
                        f: Str::escape,
                        ty: Scheme::mono(str_to_str),
                    },
                ],
                &mut self.consts.strings,
            ),
        );
    }
}
