mod directory;

use smallvec::smallvec;

use super::super::{Environment, Module, PrimDef};
use crate::primitives::Io;
use crate::typecheck::{Scheme, TyArena};

impl Environment {
    pub(super) fn register_io_builtin(&mut self) {
        // Build Directory submodule first to avoid double mutable borrow
        let directory_module = self.build_io_directory_module();

        // Io module
        let a = &mut self.ty_arena;
        let thunk_str = a.func(smallvec![], TyArena::STRING);
        let str_to_unit = a.func(smallvec![TyArena::STRING], TyArena::UNIT);

        let io_id = self.consts.strings.intern("Io");
        let dir_id = self.consts.strings.intern("Directory");
        let io_mod = Module::from_prims(
            &[
                PrimDef {
                    name: "get-line",
                    f: Io::get_line,
                    ty: Scheme::mono(thunk_str),
                },
                PrimDef {
                    name: "print",
                    f: Io::print,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "println",
                    f: Io::println,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "eprint",
                    f: Io::eprint,
                    ty: Scheme::mono(str_to_unit),
                },
                PrimDef {
                    name: "eprintln",
                    f: Io::eprintln,
                    ty: Scheme::mono(str_to_unit),
                },
            ],
            &mut self.consts.strings,
        )
        .with_submodule(dir_id, directory_module);
        self.modules.insert(io_id, io_mod);
    }
}
